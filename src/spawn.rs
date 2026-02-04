//! Dynamic spawn manager for tracking and controlling spawned process trees.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
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
            max_depth: config.depth.unwrap_or(3) as usize,
            max_children: config.children.unwrap_or(100) as usize,
            max_descendants: config.descendants.unwrap_or(500) as usize,
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
    /// Time when the child was spawned.
    pub started_at: SystemTime,
    /// Optional TTL for the child process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<Duration>,
    /// Spawn depth in the tree (0 = root service).
    pub depth: usize,
    /// Average CPU usage percentage across the process lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f32>,
    /// Resident memory in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    /// Exit metadata captured when the child terminates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exit: Option<SpawnedExit>,
}

/// Exit metadata recorded for a spawned child.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnedExit {
    /// Exit code returned by the process if it terminated normally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Signal number if the process was terminated by a signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// Timestamp when the process finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<SystemTime>,
}

/// Describes the outcome of a spawn authorization check.
#[derive(Debug, Clone)]
pub struct SpawnAuthorization {
    /// Depth the child will occupy within the spawn tree.
    pub depth: usize,
    /// Root service associated with the spawn tree, if identifiable.
    pub root_service: Option<String>,
}

/// Manages dynamic spawning for all services.
#[derive(Clone)]
pub struct DynamicSpawnManager {
    /// Map from service name to its spawn tree.
    spawn_trees: Arc<Mutex<HashMap<String, SpawnTree>>>,
    /// Map from service PID to service name.
    service_pids: Arc<Mutex<HashMap<u32, String>>>,
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
            service_pids: Arc::new(Mutex::new(HashMap::new())),
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

    /// Associates a service PID with its service name.
    pub fn register_service_pid(&self, service_name: String, pid: u32) {
        let mut service_pids = self.service_pids.lock().unwrap();
        service_pids.insert(pid, service_name);
    }

    /// Validates and authorizes a spawn request.
    pub fn authorize_spawn(
        &self,
        parent_pid: u32,
        _child_name: &str,
    ) -> Result<SpawnAuthorization, ProcessManagerError> {
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
        let (root_service, tree) = self.find_spawn_tree(parent_pid, &trees, &children)?;

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

        Ok(SpawnAuthorization {
            depth,
            root_service: Some(root_service),
        })
    }

    /// Records a successful spawn.
    pub fn record_spawn(
        &self,
        parent_pid: u32,
        child: SpawnedChild,
        root_hint: Option<String>,
    ) -> Result<Option<String>, ProcessManagerError> {
        {
            let mut children_by_parent = self.children_by_parent.lock().unwrap();
            children_by_parent
                .entry(parent_pid)
                .or_default()
                .push(child.clone());
        }

        {
            let mut children_by_pid = self.children_by_pid.lock().unwrap();
            children_by_pid.insert(child.pid, child.clone());
        }

        let mut service_name =
            root_hint.or_else(|| self.resolve_root_service_name(parent_pid));
        if service_name.is_none() {
            service_name = self.resolve_root_service_name(child.pid);
        }

        {
            let mut trees = self.spawn_trees.lock().unwrap();

            if let Some(name) = service_name.as_ref()
                && let Some(tree) = trees.get_mut(name)
            {
                tree.total_descendants += 1;
            } else if trees.len() == 1
                && let Some((_, tree)) = trees.iter_mut().next()
            {
                tree.total_descendants += 1;
            }
        }

        {
            let mut timestamps = self.spawn_timestamps.lock().unwrap();
            timestamps
                .entry(parent_pid)
                .or_default()
                .push(Instant::now());
        }

        Ok(service_name)
    }

    /// Stores exit metadata for a spawned child while leaving the tree entry intact.
    pub fn record_spawn_exit(
        &self,
        child_pid: u32,
        exit: SpawnedExit,
    ) -> Option<SpawnedChild> {
        let mut children_by_pid = self.children_by_pid.lock().unwrap();
        let updated = children_by_pid.get_mut(&child_pid).map(|child| {
            child.last_exit = Some(exit.clone());
            child.clone()
        });

        if updated.is_some() {
            let mut children_by_parent = self.children_by_parent.lock().unwrap();
            for siblings in children_by_parent.values_mut() {
                if let Some(node) =
                    siblings.iter_mut().find(|sibling| sibling.pid == child_pid)
                {
                    node.last_exit = Some(exit.clone());
                    break;
                }
            }
        }

        updated
    }

    /// Updates runtime metrics for a tracked child.
    pub fn update_child_metrics(
        &self,
        child_pid: u32,
        cpu_percent: Option<f32>,
        rss_bytes: Option<u64>,
    ) {
        {
            let mut children_by_pid = self.children_by_pid.lock().unwrap();
            if let Some(child) = children_by_pid.get_mut(&child_pid) {
                child.cpu_percent = cpu_percent;
                child.rss_bytes = rss_bytes;
            }
        }

        let mut children_by_parent = self.children_by_parent.lock().unwrap();
        for siblings in children_by_parent.values_mut() {
            if let Some(node) =
                siblings.iter_mut().find(|sibling| sibling.pid == child_pid)
            {
                node.cpu_percent = cpu_percent;
                node.rss_bytes = rss_bytes;
                break;
            }
        }
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
        self.find_spawn_tree(pid, &trees, &children)
            .map(|(_, tree)| tree.clone())
            .ok()
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
    ) -> Result<(String, &'a SpawnTree), ProcessManagerError> {
        let service_pids = self.service_pids.lock().unwrap();

        // First check if this PID is a registered service
        if let Some(service_name) = service_pids.get(&pid)
            && let Some(tree) = trees.get(service_name)
        {
            return Ok((service_name.clone(), tree));
        }

        // Walk up the parent chain to find the root service
        let mut current_pid = pid;
        while let Some(child_info) = children.get(&current_pid) {
            if let Some(parent_service) = service_pids.get(&child_info.parent_pid)
                && let Some(tree) = trees.get(parent_service)
            {
                return Ok((parent_service.clone(), tree));
            }

            // Keep walking up the chain
            current_pid = child_info.parent_pid;
        }

        if let Some(service_name) = service_pids.get(&current_pid)
            && let Some(tree) = trees.get(service_name)
        {
            return Ok((service_name.clone(), tree));
        }

        // Fallback: if there's only one tree, use it (for backward compatibility)
        if trees.len() == 1
            && let Some((name, tree)) = trees.iter().next()
        {
            return Ok((name.clone(), tree));
        }

        Err(ProcessManagerError::SpawnAuthorizationFailed(
            "No spawn tree found for process".into(),
        ))
    }

    /// Removes a terminated child from tracking.
    pub fn remove_child(&self, child_pid: u32) -> Option<SpawnedChild> {
        let child = {
            let mut children_by_pid = self.children_by_pid.lock().unwrap();
            children_by_pid.remove(&child_pid)
        };

        if let Some(child) = child {
            let mut children_by_parent = self.children_by_parent.lock().unwrap();
            if let Some(siblings) = children_by_parent.get_mut(&child.parent_pid) {
                siblings.retain(|c| c.pid != child_pid);
                if siblings.is_empty() {
                    children_by_parent.remove(&child.parent_pid);
                }
            }
            Some(child)
        } else {
            None
        }
    }

    fn resolve_root_service_name(&self, mut pid: u32) -> Option<String> {
        loop {
            {
                let service_pids = self.service_pids.lock().unwrap();
                if let Some(service_name) = service_pids.get(&pid) {
                    return Some(service_name.clone());
                }
            }

            let next_pid = {
                let children_by_pid = self.children_by_pid.lock().unwrap();
                children_by_pid.get(&pid).map(|child| child.parent_pid)
            };

            match next_pid {
                Some(parent) => pid = parent,
                None => return None,
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_spawn_completes_without_deadlock() {
        let manager = DynamicSpawnManager::new();
        let limits = SpawnLimitsConfig {
            children: Some(10),
            depth: Some(6),
            descendants: Some(50),
            total_memory: None,
            termination_policy: Some(TerminationPolicy::Cascade),
        };

        manager
            .register_service("svc".to_string(), &limits)
            .unwrap();
        manager.register_service_pid("svc".to_string(), 1);

        let child = SpawnedChild {
            name: "child".to_string(),
            pid: 2,
            parent_pid: 1,
            command: "cmd".to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth: 1,
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
        };

        let (tx, rx) = std::sync::mpsc::channel();
        let manager_clone = manager.clone();

        std::thread::spawn(move || {
            manager_clone
                .record_spawn(1, child, None)
                .expect("record_spawn should succeed");
            tx.send(()).expect("should signal completion");
        });

        assert!(
            rx.recv_timeout(Duration::from_secs(1)).is_ok(),
            "record_spawn did not complete in time"
        );
    }

    #[test]
    fn record_spawn_uses_root_hint_when_parent_untracked() {
        let manager = DynamicSpawnManager::new();
        let limits = SpawnLimitsConfig {
            children: Some(10),
            depth: Some(6),
            descendants: Some(50),
            total_memory: None,
            termination_policy: Some(TerminationPolicy::Cascade),
        };

        manager
            .register_service("svc".to_string(), &limits)
            .unwrap();

        let child = SpawnedChild {
            name: "child".to_string(),
            pid: 42,
            parent_pid: 9999,
            command: "cmd".to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth: 1,
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
        };

        let root = manager
            .record_spawn(9999, child, Some("svc".to_string()))
            .expect("record_spawn should succeed");

        assert_eq!(root.as_deref(), Some("svc"));
    }

    #[test]
    fn record_spawn_exit_tracks_metadata() {
        let manager = DynamicSpawnManager::new();
        let limits = SpawnLimitsConfig {
            children: Some(10),
            depth: Some(6),
            descendants: Some(50),
            total_memory: None,
            termination_policy: Some(TerminationPolicy::Cascade),
        };

        manager
            .register_service("svc".to_string(), &limits)
            .unwrap();
        manager.register_service_pid("svc".to_string(), 1);

        let child = SpawnedChild {
            name: "child".to_string(),
            pid: 2,
            parent_pid: 1,
            command: "cmd".to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth: 1,
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
        };

        manager
            .record_spawn(1, child, Some("svc".to_string()))
            .expect("record_spawn should succeed");

        let exit = SpawnedExit {
            exit_code: Some(0),
            signal: None,
            finished_at: Some(SystemTime::now()),
        };

        manager.record_spawn_exit(2, exit.clone());

        let children = manager.get_children(1);
        assert_eq!(children.len(), 1);
        let recorded_exit = children[0]
            .last_exit
            .as_ref()
            .expect("exit metadata present");
        assert_eq!(recorded_exit.exit_code, exit.exit_code);
    }

    #[test]
    fn update_child_metrics_caches_latest_values() {
        let manager = DynamicSpawnManager::new();
        let limits = SpawnLimitsConfig {
            children: Some(10),
            depth: Some(6),
            descendants: Some(50),
            total_memory: None,
            termination_policy: Some(TerminationPolicy::Cascade),
        };

        manager
            .register_service("svc".to_string(), &limits)
            .unwrap();
        manager.register_service_pid("svc".to_string(), 1);

        let child = SpawnedChild {
            name: "child".to_string(),
            pid: 2,
            parent_pid: 1,
            command: "cmd".to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth: 1,
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
        };

        manager
            .record_spawn(1, child, Some("svc".to_string()))
            .expect("record_spawn should succeed");

        manager.update_child_metrics(2, Some(42.0), Some(1024));

        let children = manager.get_children(1);
        assert_eq!(children[0].cpu_percent, Some(42.0));
        assert_eq!(children[0].rss_bytes, Some(1024));
    }
}
