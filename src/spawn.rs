//! Dynamic spawn manager for tracking and controlling spawned process trees.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    config::{SpawnLimitsConfig, TerminationPolicy},
    error::ProcessManagerError,
};

/// Tracks the spawn tree for a dynamically spawning parent service.
#[derive(Debug, Clone)]
pub struct SpawnTree {
    /// Service name of the root parent.
    pub service_name: String,
    /// Maximum depth allowed for spawning.
    pub max_depth: usize,
    /// Maximum number of direct children.
    pub max_children: usize,
    /// Maximum total descendants across all levels.
    pub max_descendants: usize,
    /// Memory quota in bytes for entire tree.
    pub memory_quota: Option<u64>,
    /// Memory currently used by all processes in tree.
    pub memory_used: u64,
    /// Termination policy for the tree.
    pub termination_policy: TerminationPolicy,
    /// Current spawn depth (0 for root).
    pub current_depth: usize,
    /// Total number of descendants spawned.
    pub total_descendants: usize,
}

impl SpawnTree {
    /// Creates a new spawn tree from configuration.
    pub fn from_config(service_name: String, config: &SpawnLimitsConfig) -> Self {
        Self {
            service_name,
            max_depth: config.max_depth.unwrap_or(3) as usize,
            max_children: config.max_children.unwrap_or(100) as usize,
            max_descendants: config.max_descendants.unwrap_or(500) as usize,
            memory_quota: config
                .total_memory
                .as_ref()
                .and_then(|m| parse_memory_limit(m)),
            memory_used: 0,
            termination_policy: config
                .termination_policy
                .clone()
                .unwrap_or(TerminationPolicy::Cascade),
            current_depth: 0,
            total_descendants: 0,
        }
    }

    /// Checks if a new spawn is allowed.
    pub fn can_spawn(&self, depth: usize) -> Result<(), ProcessManagerError> {
        if depth >= self.max_depth {
            return Err(ProcessManagerError::SpawnLimitExceeded(
                "Maximum spawn depth reached".into(),
            ));
        }
        if self.total_descendants >= self.max_descendants {
            return Err(ProcessManagerError::SpawnLimitExceeded(
                "Descendant limit exceeded".into(),
            ));
        }
        if let Some(quota) = self.memory_quota
            && self.memory_used >= quota
        {
            return Err(ProcessManagerError::SpawnLimitExceeded(
                "Memory quota exceeded".into(),
            ));
        }
        Ok(())
    }

    /// Creates a child spawn tree with incremented depth.
    pub fn create_child(&self) -> Self {
        let mut child = self.clone();
        child.current_depth += 1;
        child
    }
}

/// Information about a spawned child process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnedChild {
    /// Name of the child process.
    pub name: String,
    /// PID of the child process.
    pub pid: u32,
    /// PID of the parent that spawned this child.
    pub parent_pid: u32,
    /// Command used to spawn the child.
    pub command: String,
    /// Time when the child was spawned (seconds since spawn).
    #[serde(skip, default = "Instant::now")]
    pub spawned_at: Instant,
    /// Optional TTL for the child process.
    pub ttl: Option<Duration>,
    /// Spawn depth in the tree (0 = root service).
    pub depth: usize,
    /// LLM provider for agent processes.
    pub provider: Option<String>,
    /// Goal for autonomous agent processes.
    pub goal: Option<String>,
}

/// Manages dynamic spawning for all services.
pub struct DynamicSpawnManager {
    /// Map from service name to its spawn tree.
    spawn_trees: Arc<Mutex<HashMap<String, SpawnTree>>>,
    /// Map from parent PID to list of spawned children.
    children_by_parent: Arc<Mutex<HashMap<u32, Vec<SpawnedChild>>>>,
    /// Map from child PID to its spawn info.
    children_by_pid: Arc<Mutex<HashMap<u32, SpawnedChild>>>,
    /// Rate limiting: last spawn times per parent PID.
    spawn_timestamps: Arc<Mutex<HashMap<u32, Vec<Instant>>>>,
}

impl DynamicSpawnManager {
    /// Creates a new spawn manager.
    pub fn new() -> Self {
        Self {
            spawn_trees: Arc::new(Mutex::new(HashMap::new())),
            children_by_parent: Arc::new(Mutex::new(HashMap::new())),
            children_by_pid: Arc::new(Mutex::new(HashMap::new())),
            spawn_timestamps: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Registers a service with dynamic spawn capability.
    pub fn register_service(
        &self,
        service_name: String,
        config: &SpawnLimitsConfig,
    ) -> Result<(), ProcessManagerError> {
        let mut trees = self.spawn_trees.lock().unwrap();
        trees.insert(
            service_name.clone(),
            SpawnTree::from_config(service_name, config),
        );
        Ok(())
    }

    /// Validates and authorizes a spawn request.
    pub fn authorize_spawn(
        &self,
        parent_pid: u32,
        _child_name: &str,
    ) -> Result<usize, ProcessManagerError> {
        // Check rate limiting (max 10 spawns per second)
        self.check_rate_limit(parent_pid)?;

        // Find the parent's spawn tree
        let trees = self.spawn_trees.lock().unwrap();
        let children = self.children_by_pid.lock().unwrap();

        // Determine spawn depth
        let depth = if let Some(parent_info) = children.get(&parent_pid) {
            parent_info.depth + 1
        } else {
            // Direct child of a root service
            1
        };

        // Find the appropriate spawn tree
        let tree = self.find_spawn_tree(parent_pid, &trees, &children)?;

        // Check if spawn is allowed
        tree.can_spawn(depth)?;

        // Check direct children limit for this parent
        let parent_children = self.children_by_parent.lock().unwrap();
        if let Some(siblings) = parent_children.get(&parent_pid)
            && siblings.len() >= tree.max_children
        {
            return Err(ProcessManagerError::SpawnLimitExceeded(
                "Maximum direct children reached".into(),
            ));
        }

        Ok(depth)
    }

    /// Records a successful spawn.
    pub fn record_spawn(
        &self,
        parent_pid: u32,
        child: SpawnedChild,
    ) -> Result<(), ProcessManagerError> {
        let mut children_by_parent = self.children_by_parent.lock().unwrap();
        let mut children_by_pid = self.children_by_pid.lock().unwrap();

        // Record the child
        children_by_parent
            .entry(parent_pid)
            .or_default()
            .push(child.clone());
        children_by_pid.insert(child.pid, child);

        // Update spawn tree counters
        let mut trees = self.spawn_trees.lock().unwrap();
        if let Some(tree) = self.find_spawn_tree_mut(parent_pid, &mut trees) {
            tree.total_descendants += 1;
        }

        // Update rate limiting
        let mut timestamps = self.spawn_timestamps.lock().unwrap();
        timestamps
            .entry(parent_pid)
            .or_default()
            .push(Instant::now());

        Ok(())
    }

    /// Gets all children of a parent process.
    pub fn get_children(&self, parent_pid: u32) -> Vec<SpawnedChild> {
        let children = self.children_by_parent.lock().unwrap();
        children.get(&parent_pid).cloned().unwrap_or_default()
    }

    /// Gets the spawn tree for a process.
    pub fn get_spawn_tree(&self, pid: u32) -> Option<SpawnTree> {
        let trees = self.spawn_trees.lock().unwrap();
        let children = self.children_by_pid.lock().unwrap();
        self.find_spawn_tree(pid, &trees, &children).ok().cloned()
    }

    /// Removes a terminated child from tracking.
    pub fn remove_child(&self, child_pid: u32) -> Option<SpawnedChild> {
        let mut children_by_pid = self.children_by_pid.lock().unwrap();
        if let Some(child) = children_by_pid.remove(&child_pid) {
            let mut children_by_parent = self.children_by_parent.lock().unwrap();
            if let Some(siblings) = children_by_parent.get_mut(&child.parent_pid) {
                siblings.retain(|c| c.pid != child_pid);
            }
            return Some(child);
        }
        None
    }

    /// Checks rate limiting for spawn requests.
    fn check_rate_limit(&self, parent_pid: u32) -> Result<(), ProcessManagerError> {
        let mut timestamps = self.spawn_timestamps.lock().unwrap();
        let now = Instant::now();

        if let Some(recent_spawns) = timestamps.get_mut(&parent_pid) {
            // Remove timestamps older than 1 second
            recent_spawns.retain(|t| now.duration_since(*t) < Duration::from_secs(1));

            // Check if we've hit the limit (10 per second)
            if recent_spawns.len() >= 10 {
                return Err(ProcessManagerError::SpawnLimitExceeded(
                    "Spawn rate limit exceeded (max 10/sec)".into(),
                ));
            }
        }

        Ok(())
    }

    /// Finds the spawn tree for a process.
    fn find_spawn_tree<'a>(
        &self,
        pid: u32,
        trees: &'a HashMap<String, SpawnTree>,
        children: &HashMap<u32, SpawnedChild>,
    ) -> Result<&'a SpawnTree, ProcessManagerError> {
        // Walk up the parent chain to find the root service
        let mut current_pid = pid;
        let mut service_name = None;

        while let Some(child_info) = children.get(&current_pid) {
            if child_info.depth == 1 {
                // This is a direct child of a root service
                // We need to find which service owns this child
                if let Some((name, _tree)) = trees.iter().next() {
                    // Check if this service has this child
                    // In a real implementation, we'd need to track service->PID mapping
                    service_name = Some(name.clone());
                }
                break;
            }
            current_pid = child_info.parent_pid;
        }

        // If we didn't find it in children, check if it's a root service PID
        // This would require tracking service PIDs separately
        if service_name.is_none() {
            // For now, try to find any tree with dynamic spawn mode
            if let Some((name, _)) = trees.iter().next() {
                service_name = Some(name.clone());
            }
        }

        service_name
            .and_then(|name| trees.get(&name))
            .ok_or_else(|| {
                ProcessManagerError::SpawnAuthorizationFailed(
                    "No spawn tree found for process".into(),
                )
            })
    }

    /// Finds the mutable spawn tree for a process.
    fn find_spawn_tree_mut<'a>(
        &self,
        _pid: u32,
        trees: &'a mut HashMap<String, SpawnTree>,
    ) -> Option<&'a mut SpawnTree> {
        // Simplified: return the first tree
        // In production, we'd properly track service->PID mapping
        trees.values_mut().next()
    }
}

/// Parses a memory limit string into bytes.
fn parse_memory_limit(input: &str) -> Option<u64> {
    let trimmed = input.trim();
    let normalized = trimmed.replace('_', "");
    let without_bytes = normalized.trim_end_matches(&['B', 'b'][..]);

    let (number_part, factor) = match without_bytes.chars().last() {
        Some(suffix) if suffix.is_ascii_alphabetic() => {
            let len = without_bytes.len() - suffix.len_utf8();
            let number_part = &without_bytes[..len];
            let multiplier = match suffix.to_ascii_uppercase() {
                'K' => 1u64 << 10,
                'M' => 1u64 << 20,
                'G' => 1u64 << 30,
                'T' => 1u64 << 40,
                _ => return None,
            };
            (number_part.trim(), multiplier)
        }
        _ => (without_bytes.trim(), 1u64),
    };

    number_part.parse::<u64>().ok().map(|v| v * factor)
}

impl Default for DynamicSpawnManager {
    fn default() -> Self {
        Self::new()
    }
}
