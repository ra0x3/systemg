//! Dynamic spawn manager for tracking and controlling spawned process trees.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime},
};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessesToUpdate, System};

use crate::{
    config::{SpawnLimitsConfig, TerminationPolicy},
    error::ProcessManagerError,
};

/// Maximum number of spawn requests accepted from one parent per rate window.
const MAX_SPAWNS_PER_WINDOW: usize = 10;
/// Window used to enforce the per-parent spawn rate limit.
const SPAWN_RATE_WINDOW: Duration = Duration::from_secs(1);

/// Returns whether a dynamic child PID still names a live process.
fn child_is_running(pid: u32) -> bool {
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Tracks the spawn tree for a dynamically spawning parent service.
#[derive(Debug, Clone)]
pub struct SpawnTree {
    /// Project owning the root parent.
    pub project: String,
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
    pub fn from_config(
        project: String,
        service_name: String,
        config: &SpawnLimitsConfig,
    ) -> Self {
        Self {
            project,
            service_name,
            max_depth: config.depth.unwrap_or(3) as usize,
            max_children: config.children.unwrap_or(100) as usize,
            max_descendants: config.descendants.unwrap_or(500) as usize,
            memory_quota: config
                .total_memory
                .as_ref()
                .and_then(|m| parse_byte_size(m)),
            memory_used: 0,
            termination_policy: config
                .termination_policy
                .clone()
                .unwrap_or(TerminationPolicy::Cascade),
            current_depth: 0,
            total_descendants: 0,
        }
    }

    /// Re-reads the ceilings from a reloaded definition, leaving live counters
    /// (`current_depth`, `total_descendants`, `memory_used`) untouched.
    pub fn apply_limits(&mut self, config: &SpawnLimitsConfig) {
        let refreshed =
            Self::from_config(self.project.clone(), self.service_name.clone(), config);
        self.max_depth = refreshed.max_depth;
        self.max_children = refreshed.max_children;
        self.max_descendants = refreshed.max_descendants;
        self.memory_quota = refreshed.memory_quota;
        self.termination_policy = refreshed.termination_policy;
    }

    /// Recomputes the tree's memory usage from a live sample of its children.
    ///
    /// `memory_used` was never assigned after construction, so the quota check
    /// below compared against a permanent zero and could not fire. It is
    /// sampled at authorization time, which is the only moment admission
    /// control has to be right.
    pub fn observe_memory(&mut self, used: u64) {
        self.memory_used = used;
    }

    /// Checks if a new spawn is allowed.
    pub fn can_spawn(&self, depth: usize) -> Result<(), ProcessManagerError> {
        // Inclusive: a direct child is depth 1, so `depth: 3` permits three
        // levels. The exclusive form silently allowed one fewer level than the
        // manifest asked for.
        if depth > self.max_depth {
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
    /// Process owner username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Tracking category for this child process.
    #[serde(default, skip_serializing_if = "is_spawned_kind")]
    pub kind: SpawnedChildKind,
}

/// Classification of a child process shown in status output.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpawnedChildKind {
    /// Directly created and tracked by the supervisor via `sysg spawn`.
    Spawned,
    /// Discovered descendant process not directly created by the supervisor.
    Peripheral,
}

impl Default for SpawnedChildKind {
    /// Returns the default this item.
    fn default() -> Self {
        Self::Spawned
    }
}

/// Returns whether spawned kind.
fn is_spawned_kind(kind: &SpawnedChildKind) -> bool {
    matches!(kind, SpawnedChildKind::Spawned)
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

/// Builds the key a spawn tree is registered under.
///
/// Service names are only unique inside a project, so keying trees by bare name
/// let two projects that use the same service name share one tree: one
/// project's limits governed the other's children, and a stop aimed at one
/// swept the other's. The key mirrors the state key's `{project}:{service}`
/// shape for the same reason.
pub fn unit_key(project: &str, service: &str) -> String {
    format!("{project}:{service}")
}

/// Splits a key built by [`unit_key`] back into its project and service halves.
pub fn split_unit_key(key: &str) -> Option<(&str, &str)> {
    key.split_once(':')
}

/// Describes the outcome of a spawn authorization check.
#[derive(Debug, Clone)]
pub struct SpawnAuthorization {
    /// Depth the child will occupy within the spawn tree.
    pub depth: usize,
    /// Key of the spawn tree's root unit, if identifiable. See [`unit_key`].
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

    /// Returns the number of live dynamic children whose wait/log ownership
    /// cannot yet be transferred by the supervisor handoff protocol.
    pub fn active_child_count(&self) -> usize {
        lock_recover(&self.children_by_pid)
            .values()
            .filter(|child| child.last_exit.is_none() && child_is_running(child.pid))
            .count()
    }

    /// Registers a service with dynamic spawn capability.
    ///
    /// Registration is what authorizes spawning at all: a unit with no tree has
    /// every spawn request refused. `limits` is therefore allowed to be absent —
    /// `spawn: {mode: dynamic}` on its own means "dynamic, with the default
    /// ceilings", not "dynamic, but nothing may spawn".
    pub fn register_service(
        &self,
        project: &str,
        service_name: &str,
        config: &SpawnLimitsConfig,
    ) -> Result<(), ProcessManagerError> {
        let mut trees = lock_recover(&self.spawn_trees);
        let key = unit_key(project, service_name);
        match trees.get_mut(&key) {
            // A reload must not reset counters on a tree whose children are
            // still alive; only the ceilings are re-read.
            Some(existing) => existing.apply_limits(config),
            None => {
                trees.insert(
                    key,
                    SpawnTree::from_config(
                        project.to_string(),
                        service_name.to_string(),
                        config,
                    ),
                );
            }
        }
        Ok(())
    }

    /// Retires a unit's spawn authorization, for definitions that are no longer
    /// dynamic or no longer exist.
    pub fn unregister_service(&self, project: &str, service_name: &str) {
        let key = unit_key(project, service_name);
        lock_recover(&self.spawn_trees).remove(&key);
        lock_recover(&self.service_pids).retain(|_, name| *name != key);
    }

    /// Associates a running service PID with its unit key, marking the
    /// generation that subsequent spawns belong to.
    pub fn register_service_pid(&self, project: &str, service_name: &str, pid: u32) {
        let key = unit_key(project, service_name);
        let mut service_pids = lock_recover(&self.service_pids);
        // Retire only generations that are actually gone. Dropping every prior
        // pid for the key would orphan a generation that is still being torn
        // down — its children would no longer resolve to any unit, which is the
        // leak this tracking exists to prevent.
        service_pids.retain(|tracked, name| {
            *name != key || *tracked == pid || child_is_running(*tracked)
        });
        service_pids.insert(pid, key);
    }

    /// Validates and authorizes a spawn request.
    pub fn authorize_spawn(
        &self,
        parent_pid: u32,
        _child_name: &str,
    ) -> Result<SpawnAuthorization, ProcessManagerError> {
        self.check_rate_limit(parent_pid)?;

        // Resolve the tree and gather what a memory sample would need, then
        // release the locks before walking the process table: the walk is far
        // too slow to hold the registry across.
        let (root_service, depth, quota_roots) = {
            let trees = lock_recover(&self.spawn_trees);
            let children = lock_recover(&self.children_by_pid);

            let depth = match children.get(&parent_pid) {
                Some(parent) => parent.depth + 1,
                None => 1,
            };
            let (root_service, tree) =
                self.find_spawn_tree(parent_pid, &trees, &children)?;

            // Only pay for the walk when a quota is actually declared.
            let quota_roots = tree.memory_quota.is_some().then(|| {
                root_pid_of(&root_service, &self.service_pids)
                    .into_iter()
                    .chain(
                        children
                            .values()
                            .filter(|child| child.last_exit.is_none())
                            .map(|child| child.pid),
                    )
                    .collect::<Vec<u32>>()
            });
            (root_service, depth, quota_roots)
        };

        // The quota is admission control, so it has to be measured now: a
        // figure sampled on some other schedule would be stale exactly when it
        // matters, and `memory_used` was never assigned at all before this.
        let sampled = quota_roots.map(|roots| resident_bytes_of_trees(&roots));

        let mut trees = lock_recover(&self.spawn_trees);
        let Some(tree) = trees.get_mut(&root_service) else {
            return Err(ProcessManagerError::SpawnAuthorizationFailed(
                "No spawn tree found for process".into(),
            ));
        };
        if let Some(used) = sampled {
            tree.observe_memory(used);
        }
        tree.can_spawn(depth)?;
        let max_children = tree.max_children;
        drop(trees);

        let parent_children = lock_recover(&self.children_by_parent);
        if let Some(siblings) = parent_children.get(&parent_pid)
            && siblings.len() >= max_children
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
            let mut children_by_parent = lock_recover(&self.children_by_parent);
            children_by_parent
                .entry(parent_pid)
                .or_default()
                .push(child.clone());
        }

        {
            let mut children_by_pid = lock_recover(&self.children_by_pid);
            children_by_pid.insert(child.pid, child.clone());
        }

        let mut service_name =
            root_hint.or_else(|| self.resolve_root_service_name(parent_pid));
        if service_name.is_none() {
            service_name = self.resolve_root_service_name(child.pid);
        }

        {
            let mut trees = lock_recover(&self.spawn_trees);

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
            let mut timestamps = lock_recover(&self.spawn_timestamps);
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
        let mut children_by_pid = lock_recover(&self.children_by_pid);
        let updated = children_by_pid.get_mut(&child_pid).map(|child| {
            child.last_exit = Some(exit.clone());
            child.clone()
        });

        if updated.is_some() {
            let mut children_by_parent = lock_recover(&self.children_by_parent);
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
            let mut children_by_pid = lock_recover(&self.children_by_pid);
            if let Some(child) = children_by_pid.get_mut(&child_pid) {
                child.cpu_percent = cpu_percent;
                child.rss_bytes = rss_bytes;
            }
        }

        let mut children_by_parent = lock_recover(&self.children_by_parent);
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
        let children = lock_recover(&self.children_by_parent);
        children.get(&parent_pid).cloned().unwrap_or_default()
    }

    /// Gets the spawn tree for a process.
    pub fn get_spawn_tree(&self, pid: u32) -> Option<SpawnTree> {
        let trees = lock_recover(&self.spawn_trees);
        let children = lock_recover(&self.children_by_pid);
        self.find_spawn_tree(pid, &trees, &children)
            .map(|(_, tree)| tree.clone())
            .ok()
    }

    /// Resolves the termination policy associated with the tree that owns `pid`.
    pub fn termination_policy_for(&self, pid: u32) -> Option<TerminationPolicy> {
        let trees = lock_recover(&self.spawn_trees);
        let children = lock_recover(&self.children_by_pid);
        self.find_spawn_tree(pid, &trees, &children)
            .map(|(_, tree)| tree.termination_policy.clone())
            .ok()
    }

    /// Removes the subtree rooted at `root_pid`, returning all removed children.
    pub fn remove_subtree(&self, root_pid: u32) -> Vec<SpawnedChild> {
        // Resolve the owning unit BEFORE taking the removal locks: the lookup
        // takes children_by_pid itself, and these mutexes are not reentrant.
        let owner = self.resolve_root_service_name(root_pid);
        let mut removed = Vec::new();

        let mut pid_guard = lock_recover(&self.children_by_pid);
        let mut parent_guard = lock_recover(&self.children_by_parent);

        let Some(root_child) = pid_guard.get(&root_pid).cloned() else {
            return removed;
        };

        let mut stack = vec![root_child.clone()];
        let ancestor_parent = root_child.parent_pid;

        while let Some(node) = stack.pop() {
            let pid = node.pid;

            if let Some(children) = parent_guard.remove(&pid) {
                for child in children.into_iter().rev() {
                    stack.push(child.clone());
                }
            }

            if let Some(child) = pid_guard.remove(&pid) {
                removed.push(child);
            }
        }

        if let Some(siblings) = parent_guard.get_mut(&ancestor_parent) {
            siblings.retain(|s| s.pid != root_pid);
            if siblings.is_empty() {
                parent_guard.remove(&ancestor_parent);
            }
        }

        drop(parent_guard);
        drop(pid_guard);

        if !removed.is_empty() {
            let mut timestamps = lock_recover(&self.spawn_timestamps);
            for child in &removed {
                timestamps.remove(&child.pid);
            }
        }

        // `total_descendants` only ever counted up, so a long-lived parent that
        // churned short-lived children eventually hit the `descendants` ceiling
        // and could never spawn again — the limit stopped describing what was
        // running and became a lifetime total.
        if let Some(key) = owner
            && !removed.is_empty()
            && let Some(tree) = lock_recover(&self.spawn_trees).get_mut(&key)
        {
            tree.total_descendants = tree.total_descendants.saturating_sub(removed.len());
        }

        removed
    }

    /// Checks rate limiting for spawn requests.
    fn check_rate_limit(&self, parent_pid: u32) -> Result<(), ProcessManagerError> {
        let mut timestamps = lock_recover(&self.spawn_timestamps);
        let now = Instant::now();

        if let Some(recent_spawns) = timestamps.get_mut(&parent_pid) {
            recent_spawns.retain(|t| now.duration_since(*t) < SPAWN_RATE_WINDOW);

            if recent_spawns.len() >= MAX_SPAWNS_PER_WINDOW {
                return Err(ProcessManagerError::SpawnLimitExceeded(format!(
                    "Spawn rate limit exceeded (max {MAX_SPAWNS_PER_WINDOW}/sec)"
                )));
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
        let service_pids = lock_recover(&self.service_pids);

        if let Some(service_name) = service_pids.get(&pid)
            && let Some(tree) = trees.get(service_name)
        {
            return Ok((service_name.clone(), tree));
        }

        let pid_is_tracked_child = children.contains_key(&pid);

        let mut current_pid = pid;
        while let Some(child_info) = children.get(&current_pid) {
            if let Some(parent_service) = service_pids.get(&child_info.parent_pid)
                && let Some(tree) = trees.get(parent_service)
            {
                return Ok((parent_service.clone(), tree));
            }

            current_pid = child_info.parent_pid;
        }

        if let Some(service_name) = service_pids.get(&current_pid)
            && let Some(tree) = trees.get(service_name)
        {
            return Ok((service_name.clone(), tree));
        }

        // Single-tree fallback: only authorize when the requesting pid is actually
        // linked to the manager (a registered service pid or a tracked child).
        // An arbitrary, unrelated pid must not be auto-authorized just because one
        // tree happens to exist.
        if trees.len() == 1
            && (pid_is_tracked_child || service_pids.contains_key(&pid))
            && let Some((name, tree)) = trees.iter().next()
        {
            return Ok((name.clone(), tree));
        }

        Err(ProcessManagerError::SpawnAuthorizationFailed(
            "No spawn tree found for process".into(),
        ))
    }

    /// Walks a tracked pid up to the registered service PID that roots it.
    ///
    /// That pid identifies the *generation*: a restarted unit gets a new pid, so
    /// children recorded under the previous one must never be swept by a stop
    /// aimed at the replacement.
    pub fn root_pid_for(&self, pid: u32) -> Option<u32> {
        // Lock order across this type is
        // spawn_trees -> children_by_pid -> children_by_parent ->
        // spawn_timestamps -> service_pids. `find_spawn_tree` takes
        // children_by_pid before service_pids, so taking them the other way
        // round here would let an authorization and a sweep deadlock.
        let children = lock_recover(&self.children_by_pid);
        let service_pids = lock_recover(&self.service_pids);
        let mut current = pid;
        loop {
            if service_pids.contains_key(&current) {
                return Some(current);
            }
            match children.get(&current) {
                Some(child) => current = child.parent_pid,
                None => return None,
            }
        }
    }

    /// Returns the termination policy registered for a unit.
    pub fn policy_for_unit(
        &self,
        project: &str,
        service_name: &str,
    ) -> TerminationPolicy {
        lock_recover(&self.spawn_trees)
            .get(&unit_key(project, service_name))
            .map(|tree| tree.termination_policy.clone())
            .unwrap_or(TerminationPolicy::Cascade)
    }

    /// Removes and returns every dynamic child belonging to one generation of a
    /// unit, at any depth.
    ///
    /// Dynamic children are forked by the supervisor, not by the service, so
    /// they are neither descendants of the service pid nor members of its
    /// process group or session — the ordinary teardown sweep cannot see them.
    /// Stopping a unit has to reclaim them explicitly, and only the generation
    /// being stopped: a rolling restart would otherwise kill the replacement's
    /// children along with the outgoing one's.
    pub fn take_generation_children(
        &self,
        project: &str,
        service_name: &str,
        root_pid: u32,
    ) -> Vec<SpawnedChild> {
        let key = unit_key(project, service_name);

        // One critical section, in the type's canonical lock order. Snapshotting
        // ownership and then removing under separate locks let a spawn slip in
        // between the two: the child was authorized, missed the snapshot,
        // survived the sweep, and then lost the root that would have identified
        // it later.
        let mut trees = lock_recover(&self.spawn_trees);
        let mut pid_guard = lock_recover(&self.children_by_pid);
        let mut parent_guard = lock_recover(&self.children_by_parent);
        let mut timestamps = lock_recover(&self.spawn_timestamps);
        let mut service_pids = lock_recover(&self.service_pids);

        if service_pids.get(&root_pid) != Some(&key) {
            return Vec::new();
        }

        let owned: Vec<u32> = pid_guard
            .keys()
            .copied()
            .filter(|pid| {
                let mut current = *pid;
                loop {
                    if current == root_pid {
                        return true;
                    }
                    match pid_guard.get(&current) {
                        Some(child) => current = child.parent_pid,
                        None => return false,
                    }
                }
            })
            .collect();

        let mut removed = Vec::new();
        for pid in owned {
            if let Some(child) = pid_guard.remove(&pid) {
                if let Some(siblings) = parent_guard.get_mut(&child.parent_pid) {
                    siblings.retain(|sibling| sibling.pid != pid);
                    if siblings.is_empty() {
                        parent_guard.remove(&child.parent_pid);
                    }
                }
                removed.push(child);
            }
            parent_guard.remove(&pid);
            timestamps.remove(&pid);
        }

        if let Some(tree) = trees.get_mut(&key) {
            tree.total_descendants = tree.total_descendants.saturating_sub(removed.len());
        }
        service_pids.remove(&root_pid);

        removed
    }

    /// Removes a terminated child from tracking.
    pub fn remove_child(&self, child_pid: u32) -> Option<SpawnedChild> {
        let child = {
            let mut children_by_pid = lock_recover(&self.children_by_pid);
            children_by_pid.remove(&child_pid)
        };

        if let Some(child) = child {
            let mut children_by_parent = lock_recover(&self.children_by_parent);
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

    /// Resolves root service name.
    fn resolve_root_service_name(&self, mut pid: u32) -> Option<String> {
        loop {
            {
                let service_pids = lock_recover(&self.service_pids);
                if let Some(service_name) = service_pids.get(&pid) {
                    return Some(service_name.clone());
                }
            }

            let next_pid = {
                let children_by_pid = lock_recover(&self.children_by_pid);
                children_by_pid.get(&pid).map(|child| child.parent_pid)
            };

            match next_pid {
                Some(parent) => pid = parent,
                None => return None,
            }
        }
    }
}

/// Resolves the generation pid registered under a unit key.
fn root_pid_of(
    key: &str,
    service_pids: &Arc<Mutex<HashMap<u32, String>>>,
) -> Option<u32> {
    lock_recover(service_pids)
        .iter()
        .find(|(_, name)| name.as_str() == key)
        .map(|(pid, _)| *pid)
}

/// Sums resident memory over each given pid and everything beneath it.
///
/// Dynamic children are forked by the supervisor rather than by the unit, so a
/// single walk from the unit's pid would miss them; every tracked child is a
/// root in its own right, and each may have forked a tree of its own.
fn resident_bytes_of_trees(roots: &[u32]) -> u64 {
    if roots.is_empty() {
        return 0;
    }
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);

    let mut by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            by_parent
                .entry(parent.as_u32())
                .or_default()
                .push(pid.as_u32());
        }
    }

    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut stack: Vec<u32> = roots.to_vec();
    let mut total: u64 = 0;
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(process) = system.process(sysinfo::Pid::from_u32(pid)) {
            total = total.saturating_add(process.memory());
        }
        if let Some(kids) = by_parent.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    total
}

/// Parses a byte-size string (e.g. `256M`, `2G`) into bytes.
fn parse_byte_size(input: &str) -> Option<u64> {
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

    number_part
        .parse::<u64>()
        .ok()
        .and_then(|v| v.checked_mul(factor))
}

impl Default for DynamicSpawnManager {
    /// Returns the default this item.
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn parse_byte_size_handles_overflow() {
        assert_eq!(parse_byte_size("256M"), Some(256 * (1u64 << 20)));
        assert_eq!(parse_byte_size("99999999999999999999G"), None);
    }

    #[test]
    fn authorize_spawn_rejects_unrelated_pid_with_single_tree() {
        let manager = DynamicSpawnManager::new();
        let limits = SpawnLimitsConfig {
            children: Some(10),
            depth: Some(6),
            descendants: Some(50),
            total_memory: None,
            termination_policy: Some(TerminationPolicy::Cascade),
        };
        manager.register_service("proj", "svc", &limits).unwrap();
        manager.register_service_pid("proj", "svc", 1);

        assert!(
            manager.authorize_spawn(1, "child").is_ok(),
            "registered service pid should be authorized"
        );
        assert!(
            manager.authorize_spawn(99999, "child").is_err(),
            "unrelated pid must not be authorized via single-tree fallback"
        );
    }

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

        manager.register_service("proj", "svc", &limits).unwrap();
        manager.register_service_pid("proj", "svc", 1);

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
            user: None,
            kind: SpawnedChildKind::Spawned,
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

        manager.register_service("proj", "svc", &limits).unwrap();

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
            user: None,
            kind: SpawnedChildKind::Spawned,
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

        manager.register_service("proj", "svc", &limits).unwrap();
        manager.register_service_pid("proj", "svc", 1);

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
            user: None,
            kind: SpawnedChildKind::Spawned,
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

        manager.register_service("proj", "svc", &limits).unwrap();
        manager.register_service_pid("proj", "svc", 1);

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
            user: None,
            kind: SpawnedChildKind::Spawned,
        };

        manager
            .record_spawn(1, child, Some("svc".to_string()))
            .expect("record_spawn should succeed");

        manager.update_child_metrics(2, Some(42.0), Some(1024));

        let children = manager.get_children(1);
        assert_eq!(children[0].cpu_percent, Some(42.0));
        assert_eq!(children[0].rss_bytes, Some(1024));
    }

    #[test]
    fn termination_policy_for_returns_configured_policy() {
        let manager = DynamicSpawnManager::new();
        let limits = SpawnLimitsConfig {
            children: Some(10),
            depth: Some(6),
            descendants: Some(50),
            total_memory: None,
            termination_policy: Some(TerminationPolicy::Orphan),
        };

        manager.register_service("proj", "svc", &limits).unwrap();
        manager.register_service_pid("proj", "svc", 1);

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
            user: None,
            kind: SpawnedChildKind::Spawned,
        };

        manager
            .record_spawn(1, child, Some("svc".to_string()))
            .expect("record_spawn should succeed");

        let policy = manager
            .termination_policy_for(2)
            .expect("termination policy should be resolvable");
        assert_eq!(policy, TerminationPolicy::Orphan);
    }

    #[test]
    fn remove_subtree_removes_all_descendants() {
        let manager = DynamicSpawnManager::new();
        let limits = SpawnLimitsConfig {
            children: Some(10),
            depth: Some(6),
            descendants: Some(50),
            total_memory: None,
            termination_policy: Some(TerminationPolicy::Cascade),
        };

        manager.register_service("proj", "svc", &limits).unwrap();
        manager.register_service_pid("proj", "svc", 1);

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
            user: None,
            kind: SpawnedChildKind::Spawned,
        };

        let grandchild = SpawnedChild {
            name: "grandchild".to_string(),
            pid: 3,
            parent_pid: 2,
            command: "cmd".to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth: 2,
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
            user: None,
            kind: SpawnedChildKind::Spawned,
        };

        manager
            .record_spawn(1, child, Some("svc".to_string()))
            .expect("record_spawn should succeed");
        manager
            .record_spawn(2, grandchild, Some("svc".to_string()))
            .expect("record_spawn should succeed");

        let removed = manager.remove_subtree(2);
        let removed_pids: HashSet<_> = removed.into_iter().map(|c| c.pid).collect();
        assert_eq!(removed_pids, HashSet::from([2, 3]));

        assert!(
            manager.get_children(1).is_empty(),
            "parent should have no children"
        );
        assert!(
            manager.get_children(2).is_empty(),
            "removed child should have no tracked descendants"
        );
    }

    fn child(name: &str, pid: u32, parent_pid: u32, depth: usize) -> SpawnedChild {
        SpawnedChild {
            name: name.to_string(),
            pid,
            parent_pid,
            command: "worker".to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth,
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
            user: None,
            kind: SpawnedChildKind::Spawned,
        }
    }

    /// `spawn: {mode: dynamic}` with no `limits` block must still authorize
    /// spawning. Registration keyed off `limits` meant the documented shorthand
    /// registered no tree at all and every request was refused.
    #[test]
    fn dynamic_without_limits_authorizes_with_defaults() {
        let manager = DynamicSpawnManager::new();
        manager
            .register_service("proj", "svc", &SpawnLimitsConfig::default())
            .expect("register");
        manager.register_service_pid("proj", "svc", 1);

        let auth = manager
            .authorize_spawn(1, "worker")
            .expect("a dynamic unit with default limits must authorize spawning");
        assert_eq!(auth.root_service.as_deref(), Some("proj:svc"));

        let tree = manager.get_spawn_tree(1).expect("tree");
        assert_eq!(tree.max_children, 100);
        assert_eq!(tree.max_descendants, 500);
        assert_eq!(tree.termination_policy, TerminationPolicy::Cascade);
    }

    /// Two projects using the same service name must not share one tree: one
    /// project's ceilings governed the other's children, and a stop aimed at one
    /// swept the other's.
    #[test]
    fn same_service_name_in_two_projects_gets_separate_trees() {
        let manager = DynamicSpawnManager::new();
        let tight = SpawnLimitsConfig {
            children: Some(1),
            ..SpawnLimitsConfig::default()
        };
        manager
            .register_service("alpha", "worker", &SpawnLimitsConfig::default())
            .expect("register alpha");
        manager
            .register_service("beta", "worker", &tight)
            .expect("register beta");
        manager.register_service_pid("alpha", "worker", 10);
        manager.register_service_pid("beta", "worker", 20);

        assert_eq!(
            manager.get_spawn_tree(10).expect("alpha tree").max_children,
            100
        );
        assert_eq!(
            manager.get_spawn_tree(20).expect("beta tree").max_children,
            1
        );

        manager.unregister_service("beta", "worker");
        assert!(
            manager.get_spawn_tree(10).is_some(),
            "retiring one project's unit must leave the other's tree standing"
        );
    }

    /// A definition that stops being dynamic must lose its authorization.
    #[test]
    fn unregister_revokes_authorization() {
        let manager = DynamicSpawnManager::new();
        manager
            .register_service("proj", "svc", &SpawnLimitsConfig::default())
            .expect("register");
        manager.register_service_pid("proj", "svc", 1);
        manager.unregister_service("proj", "svc");

        assert!(
            manager.authorize_spawn(1, "worker").is_err(),
            "a retired unit must not keep spawning"
        );
    }

    /// The sweep must return children at every depth — dynamic children are all
    /// forked by the supervisor, so ancestry does not chain and a direct-children
    /// sweep left the deeper ones running.
    #[test]
    fn generation_sweep_takes_every_depth() {
        let manager = DynamicSpawnManager::new();
        manager
            .register_service("proj", "svc", &SpawnLimitsConfig::default())
            .expect("register");
        manager.register_service_pid("proj", "svc", 1);
        manager
            .record_spawn(1, child("a", 2, 1, 1), Some("proj:svc".to_string()))
            .expect("record a");
        manager
            .record_spawn(2, child("b", 3, 2, 2), Some("proj:svc".to_string()))
            .expect("record b");

        let taken = manager.take_generation_children("proj", "svc", 1);
        let pids: HashSet<_> = taken.into_iter().map(|c| c.pid).collect();
        assert_eq!(pids, HashSet::from([2, 3]));
        assert!(manager.get_children(1).is_empty());
    }

    /// Scoping to the generation's root pid is what keeps a rolling restart from
    /// killing the replacement's children along with the outgoing generation's.
    #[test]
    fn generation_sweep_ignores_a_different_generation() {
        let manager = DynamicSpawnManager::new();
        manager
            .register_service("proj", "svc", &SpawnLimitsConfig::default())
            .expect("register");
        manager.register_service_pid("proj", "svc", 1);
        manager
            .record_spawn(1, child("a", 2, 1, 1), Some("proj:svc".to_string()))
            .expect("record a");

        assert!(
            manager
                .take_generation_children("proj", "svc", 99)
                .is_empty(),
            "a stop aimed at another generation must sweep nothing"
        );
        assert_eq!(manager.get_children(1).len(), 1);
    }

    /// Registering a new generation must not orphan a live previous one: its
    /// children would stop resolving to any unit, which is the leak this
    /// tracking exists to prevent.
    #[test]
    fn a_new_generation_keeps_a_live_predecessor_resolvable() {
        let manager = DynamicSpawnManager::new();
        manager
            .register_service("proj", "svc", &SpawnLimitsConfig::default())
            .expect("register");

        // std::process::id() is this test process: a pid that is certainly live.
        let live_old = std::process::id();
        manager.register_service_pid("proj", "svc", live_old);
        manager
            .record_spawn(live_old, child("a", 424242, live_old, 1), None)
            .expect("record a");
        manager.register_service_pid("proj", "svc", 424243);

        assert_eq!(
            manager.root_pid_for(424242),
            Some(live_old),
            "the outgoing generation must still own its children"
        );
        let taken = manager.take_generation_children("proj", "svc", live_old);
        assert_eq!(taken.len(), 1, "its children remain sweepable");
    }

    /// `depth: N` must permit N levels. The exclusive comparison silently gave
    /// one level fewer than the manifest asked for.
    #[test]
    fn depth_ceiling_is_inclusive() {
        let tree = SpawnTree::from_config(
            "proj".into(),
            "svc".into(),
            &SpawnLimitsConfig {
                depth: Some(2),
                ..SpawnLimitsConfig::default()
            },
        );
        assert!(tree.can_spawn(1).is_ok(), "a direct child is depth 1");
        assert!(tree.can_spawn(2).is_ok(), "depth: 2 must permit two levels");
        assert!(tree.can_spawn(3).is_err(), "the third level is refused");
    }

    /// The quota check compared against a field nothing ever assigned, so it
    /// could not fire however much the tree consumed.
    #[test]
    fn memory_quota_refuses_once_observed() {
        let mut tree = SpawnTree::from_config(
            "proj".into(),
            "svc".into(),
            &SpawnLimitsConfig {
                total_memory: Some("1M".into()),
                ..SpawnLimitsConfig::default()
            },
        );
        assert_eq!(tree.memory_quota, Some(1024 * 1024));
        assert!(tree.can_spawn(1).is_ok(), "an idle tree admits work");

        tree.observe_memory(2 * 1024 * 1024);
        assert!(
            tree.can_spawn(1).is_err(),
            "a tree over its quota must stop admitting children"
        );
    }

    /// `total_descendants` only counted up, so a parent that churned children
    /// eventually hit the ceiling permanently.
    #[test]
    fn descendant_count_falls_when_children_are_removed() {
        let manager = DynamicSpawnManager::new();
        manager
            .register_service("proj", "svc", &SpawnLimitsConfig::default())
            .expect("register");
        manager.register_service_pid("proj", "svc", 1);
        manager
            .record_spawn(1, child("a", 2, 1, 1), Some("proj:svc".to_string()))
            .expect("record a");
        assert_eq!(
            manager.get_spawn_tree(1).expect("tree").total_descendants,
            1
        );

        manager.remove_subtree(2);
        assert_eq!(
            manager.get_spawn_tree(1).expect("tree").total_descendants,
            0,
            "a departed child must free its slot"
        );
    }

    /// A reload must re-read the ceilings without resetting live counters.
    #[test]
    fn reregistering_keeps_live_counters() {
        let manager = DynamicSpawnManager::new();
        manager
            .register_service("proj", "svc", &SpawnLimitsConfig::default())
            .expect("register");
        manager.register_service_pid("proj", "svc", 1);
        manager
            .record_spawn(1, child("a", 2, 1, 1), Some("proj:svc".to_string()))
            .expect("record a");

        manager
            .register_service(
                "proj",
                "svc",
                &SpawnLimitsConfig {
                    children: Some(7),
                    ..SpawnLimitsConfig::default()
                },
            )
            .expect("re-register");

        let tree = manager.get_spawn_tree(1).expect("tree");
        assert_eq!(tree.max_children, 7, "ceilings are re-read");
        assert_eq!(
            tree.total_descendants, 1,
            "a live child must survive its unit's reload"
        );
    }
}
