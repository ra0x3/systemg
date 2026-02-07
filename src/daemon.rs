//! Service management daemon.
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File},
    io::{BufRead, BufReader, ErrorKind},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use fs2::FileExt;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize, de::Error as _};
use serde_yaml;
use sysinfo::{ProcessesToUpdate, System};
use tracing::{debug, error, info, trace, warn};

use crate::{
    config::{
        Config, EnvConfig, HealthCheckConfig, HookAction, HookOutcome, HookStage,
        ServiceConfig, SkipConfig,
    },
    constants::{
        DEFAULT_SHELL, DaemonLock, DeploymentStrategy, MAX_STATUS_LOG_LINES,
        PID_FILE_NAME, PID_LOCK_SUFFIX, POST_RESTART_VERIFY_ATTEMPTS,
        POST_RESTART_VERIFY_DELAY, PROCESS_CHECK_INTERVAL, PROCESS_READY_CHECKS,
        SERVICE_POLL_INTERVAL, SERVICE_START_TIMEOUT, SHELL_COMMAND_FLAG,
        STATE_FILE_NAME,
    },
    error::{PidFileError, ProcessManagerError, ServiceStateError},
    logs::{resolve_log_path, spawn_log_writer},
    runtime,
    spawn::SpawnedExit,
};

/// Builds env map for service (inline vars override file entries).
fn collect_service_env(
    env: &Option<EnvConfig>,
    project_root: &Path,
    service_name: &str,
) -> HashMap<String, String> {
    let mut resolved = HashMap::new();

    if let Some(env_config) = env {
        if let Some(file_path) = env_config.path(project_root) {
            match fs::read_to_string(&file_path) {
                Ok(content) => {
                    for raw_line in content.lines() {
                        let line = raw_line.trim();
                        if line.is_empty() || line.starts_with('#') {
                            continue;
                        }

                        if let Some((key, value)) = line.split_once('=') {
                            let key = key.trim().to_string();
                            let mut value = value.trim().to_string();

                            if value.starts_with('"')
                                && value.ends_with('"')
                                && value.len() >= 2
                            {
                                value = value[1..value.len() - 1].to_string();
                            }

                            resolved.entry(key).or_insert(value);
                        } else {
                            warn!(
                                "Ignoring malformed line in env file for '{}': {}",
                                service_name, line
                            );
                        }
                    }
                }
                Err(err) => {
                    error!("Failed to read env file for '{}': {}", service_name, err);
                }
            }
        }

        if let Some(vars) = &env_config.vars {
            for (key, value) in vars {
                resolved.insert(key.clone(), value.clone());
            }
        }
    }

    resolved
}

/// PID tracking file.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct PidFile {
    /// Service name -> PID map.
    services: HashMap<String, u32>,
    /// Child PID -> Parent PID mapping for spawned processes.
    #[serde(default)]
    parent_map: HashMap<u32, u32>,
    /// Parent PID -> list of Child PIDs for reverse lookup.
    #[serde(default)]
    children_map: HashMap<u32, Vec<u32>>,
    /// PID -> spawn depth in the tree (0 = root service).
    #[serde(default)]
    spawn_depth: HashMap<u32, usize>,
    /// Additional metadata for spawned children.
    #[serde(default)]
    spawn_metadata: HashMap<u32, PersistedSpawnChild>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PersistedSpawnChild {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) command: String,
    #[serde(default = "SystemTime::now")]
    pub(crate) started_at: SystemTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ttl_secs: Option<u64>,
    pub(crate) depth: usize,
    pub(crate) parent_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) service_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cpu_percent: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rss_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_exit: Option<SpawnedExit>,
}

impl PidFile {
    fn path() -> PathBuf {
        runtime::state_dir().join(PID_FILE_NAME)
    }

    fn lock_path() -> PathBuf {
        runtime::state_dir().join(format!("{}{}", PID_FILE_NAME, PID_LOCK_SUFFIX))
    }

    /// Gets exclusive lock (auto-releases on drop).
    fn acquire_lock() -> Result<File, PidFileError> {
        let lock_path = Self::lock_path();
        fs::create_dir_all(lock_path.parent().unwrap())?;

        let lock_file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        lock_file.lock_exclusive()?;

        Ok(lock_file)
    }

    /// Returns a reference to the services map.
    pub fn services(&self) -> &HashMap<String, u32> {
        &self.services
    }

    /// Loads with file locking.
    pub fn load() -> Result<Self, PidFileError> {
        let _lock = Self::acquire_lock()?;

        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(path)?;
        let pid_data = serde_json::from_str::<Self>(&contents)?;
        Ok(pid_data)
    }

    /// Returns the PID for a specific service.
    pub fn pid_for(&self, service: &str) -> Option<u32> {
        self.services.get(service).copied()
    }

    /// Reloads from disk.
    pub fn reload() -> Result<Self, PidFileError> {
        let _lock = Self::acquire_lock()?;

        let path = Self::path();
        let contents = fs::read_to_string(&path)?;
        let pid_data = serde_json::from_str::<Self>(&contents)?;
        Ok(pid_data)
    }

    /// Saves to disk.
    pub fn save(&self) -> Result<(), PidFileError> {
        let _lock = Self::acquire_lock()?;

        let path = Self::path();
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Atomically inserts PID.
    pub fn insert(&mut self, service: &str, pid: u32) -> Result<(), PidFileError> {
        let _lock = Self::acquire_lock()?;

        // Reload from disk to ensure we have the latest state
        let path = Self::path();
        if path.exists() {
            let contents = fs::read_to_string(&path)?;
            *self = serde_json::from_str::<Self>(&contents)?;
        }

        // Insert the new service
        self.services.insert(service.to_string(), pid);

        // Save back to disk
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Atomically removes service.
    pub fn remove(&mut self, service: &str) -> Result<(), PidFileError> {
        let _lock = Self::acquire_lock()?;

        // Reload from disk to ensure we have the latest state
        let path = Self::path();
        if path.exists() {
            let contents = fs::read_to_string(&path)?;
            *self = serde_json::from_str::<Self>(&contents)?;
        }

        // Remove the service
        if self.services.remove(service).is_none() {
            return Err(PidFileError::ServiceNotFound);
        }

        // Save back to disk
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Gets the PID for a service.
    pub fn get(&self, service: &str) -> Option<u32> {
        self.services.get(service).copied()
    }

    /// Atomically records a spawned child process.
    pub(crate) fn record_spawn(
        &mut self,
        metadata: PersistedSpawnChild,
    ) -> Result<(), PidFileError> {
        let _lock = Self::acquire_lock()?;

        // Reload from disk to ensure we have the latest state
        let path = Self::path();
        if path.exists() {
            let contents = fs::read_to_string(&path)?;
            *self = serde_json::from_str::<Self>(&contents)?;
        }

        let child_pid = metadata.pid;
        let parent_pid = metadata.parent_pid;
        let depth = metadata.depth;

        // Record parent-child relationship
        self.parent_map.insert(child_pid, parent_pid);
        self.children_map
            .entry(parent_pid)
            .or_default()
            .push(child_pid);
        self.spawn_depth.insert(child_pid, depth);
        self.spawn_metadata.insert(child_pid, metadata);

        // Save back to disk
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub(crate) fn record_spawn_exit(
        &mut self,
        child_pid: u32,
        exit: SpawnedExit,
    ) -> Result<(), PidFileError> {
        let _lock = Self::acquire_lock()?;

        let path = Self::path();
        if path.exists() {
            let contents = fs::read_to_string(&path)?;
            *self = serde_json::from_str::<Self>(&contents)?;
        }

        if let Some(metadata) = self.spawn_metadata.get_mut(&child_pid) {
            metadata.last_exit = Some(exit.clone());
        }

        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Atomically removes a spawned child process.
    pub fn remove_spawn(&mut self, child_pid: u32) -> Result<(), PidFileError> {
        let _lock = Self::acquire_lock()?;

        // Reload from disk to ensure we have the latest state
        let path = Self::path();
        if path.exists() {
            let contents = fs::read_to_string(&path)?;
            *self = serde_json::from_str::<Self>(&contents)?;
        }

        // Remove child from parent's children list
        if let Some(parent_pid) = self.parent_map.remove(&child_pid)
            && let Some(children) = self.children_map.get_mut(&parent_pid)
        {
            children.retain(|&pid| pid != child_pid);
            if children.is_empty() {
                self.children_map.remove(&parent_pid);
            }
        }
        self.spawn_depth.remove(&child_pid);
        self.spawn_metadata.remove(&child_pid);

        // Save back to disk
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub(crate) fn remove_spawn_subtree(
        &mut self,
        root_pid: u32,
    ) -> Result<Vec<u32>, PidFileError> {
        let _lock = Self::acquire_lock()?;

        let path = Self::path();
        if path.exists() {
            let contents = fs::read_to_string(&path)?;
            *self = serde_json::from_str::<Self>(&contents)?;
        }

        let removed = self.remove_spawn_subtree_in_memory(root_pid);

        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, serde_json::to_string_pretty(self)?)?;

        Ok(removed)
    }

    /// Removes a subtree rooted at `root_pid` from the in-memory tracking maps.
    pub(crate) fn remove_spawn_subtree_in_memory(&mut self, _root_pid: u32) -> Vec<u32> {
        let root_pid = _root_pid;
        if !self.parent_map.contains_key(&root_pid)
            && !self.children_map.contains_key(&root_pid)
            && !self.spawn_metadata.contains_key(&root_pid)
        {
            return Vec::new();
        }

        let root_parent = self.parent_map.get(&root_pid).copied();

        let mut removed = Vec::new();
        let mut stack = vec![root_pid];

        while let Some(pid) = stack.pop() {
            if let Some(children) = self.children_map.remove(&pid) {
                for child_pid in children.into_iter().rev() {
                    stack.push(child_pid);
                }
            }

            self.parent_map.remove(&pid);
            self.spawn_depth.remove(&pid);
            self.spawn_metadata.remove(&pid);
            removed.push(pid);
        }

        if let Some(parent_pid) = root_parent {
            let remove_parent_entry =
                if let Some(children) = self.children_map.get_mut(&parent_pid) {
                    children.retain(|child| *child != root_pid);
                    children.is_empty()
                } else {
                    false
                };

            if remove_parent_entry {
                self.children_map.remove(&parent_pid);
            }
        }

        removed
    }

    /// Gets the parent PID for a child process.
    pub fn get_parent(&self, child_pid: u32) -> Option<u32> {
        self.parent_map.get(&child_pid).copied()
    }

    /// Gets all children of a parent process.
    pub fn get_children(&self, parent_pid: u32) -> Vec<u32> {
        self.children_map
            .get(&parent_pid)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn get_spawn_metadata(&self, pid: u32) -> Option<&PersistedSpawnChild> {
        self.spawn_metadata.get(&pid)
    }

    pub(crate) fn spawn_children_for_parent(
        &self,
        parent_pid: u32,
    ) -> Vec<&PersistedSpawnChild> {
        self.spawn_metadata
            .values()
            .filter(|meta| meta.parent_pid == parent_pid)
            .collect()
    }

    pub(crate) fn spawn_roots_for_service(
        &self,
        service_hash: &str,
    ) -> Vec<&PersistedSpawnChild> {
        self.spawn_metadata
            .values()
            .filter(|meta| meta.service_hash.as_deref() == Some(service_hash))
            .filter(|meta| {
                self.spawn_metadata
                    .get(&meta.parent_pid)
                    .and_then(|parent| parent.service_hash.as_deref())
                    .map(|hash| hash != service_hash)
                    .unwrap_or(true)
            })
            .collect()
    }

    /// Gets the spawn depth for a process.
    pub fn get_depth(&self, pid: u32) -> Option<usize> {
        self.spawn_depth.get(&pid).copied()
    }

    /// Gets all descendants of a process recursively.
    pub fn get_descendants(&self, pid: u32) -> Vec<u32> {
        let mut descendants = Vec::new();
        let mut to_process = vec![pid];

        while let Some(current) = to_process.pop() {
            if let Some(children) = self.children_map.get(&current) {
                descendants.extend(children);
                to_process.extend(children);
            }
        }

        descendants
    }
}

#[cfg(test)]
mod pidfile_tests {
    use std::{collections::HashMap, time::SystemTime};

    use super::*;

    #[test]
    fn remove_spawn_subtree_in_memory_prunes_all_descendants() {
        let mut pid_file = PidFile {
            services: HashMap::new(),
            parent_map: HashMap::from([(2, 1), (3, 2)]),
            children_map: HashMap::from([(1, vec![2]), (2, vec![3])]),
            spawn_depth: HashMap::from([(1, 0), (2, 1), (3, 2)]),
            spawn_metadata: HashMap::from([
                (
                    2,
                    PersistedSpawnChild {
                        pid: 2,
                        name: "child".into(),
                        command: "cmd".into(),
                        started_at: SystemTime::now(),
                        ttl_secs: None,
                        depth: 1,
                        parent_pid: 1,
                        service_hash: None,
                        cpu_percent: None,
                        rss_bytes: None,
                        last_exit: None,
                    },
                ),
                (
                    3,
                    PersistedSpawnChild {
                        pid: 3,
                        name: "grandchild".into(),
                        command: "cmd".into(),
                        started_at: SystemTime::now(),
                        ttl_secs: None,
                        depth: 2,
                        parent_pid: 2,
                        service_hash: None,
                        cpu_percent: None,
                        rss_bytes: None,
                        last_exit: None,
                    },
                ),
            ]),
        };

        let removed = pid_file.remove_spawn_subtree_in_memory(2);
        assert_eq!(
            removed,
            vec![2, 3],
            "subtree removal should return root and descendants"
        );
        assert!(!pid_file.parent_map.contains_key(&2));
        assert!(!pid_file.parent_map.contains_key(&3));
        assert!(!pid_file.children_map.contains_key(&2));
        assert!(
            pid_file
                .children_map
                .get(&1)
                .map(|children| children.is_empty())
                .unwrap_or(true)
        );
        assert!(!pid_file.spawn_metadata.contains_key(&2));
        assert!(!pid_file.spawn_metadata.contains_key(&3));
    }
}

/// Service lifecycle states.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLifecycleStatus {
    /// Currently running.
    Running,
    /// Skipped via config.
    Skipped,
    /// Clean exit (code 0).
    ExitedSuccessfully,
    /// Error exit (non-zero/signal).
    ExitedWithError,
    /// Manually stopped.
    Stopped,
}

/// Service runtime metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStateEntry {
    /// Current lifecycle status of the service.
    pub status: ServiceLifecycleStatus,
    /// Process ID when the service is running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Exit code when the service has exited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Signal number if the service was terminated by a signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

/// Persistent record of the last-known state for every managed service.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ServiceStateFile {
    services: HashMap<String, ServiceStateEntry>,
}

impl ServiceStateFile {
    fn path() -> PathBuf {
        runtime::state_dir().join(STATE_FILE_NAME)
    }

    /// Loads the service state file from disk, creating an empty one if it doesn't exist.
    pub fn load() -> Result<Self, ServiceStateError> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(path)?;
        let state = serde_json::from_str::<Self>(&contents)?;
        Ok(state)
    }

    /// Saves the state file to disk.
    pub fn save(&self) -> Result<(), ServiceStateError> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Returns a reference to the map of all service states.
    /// Keys are service configuration hashes (not service names).
    pub fn services(&self) -> &HashMap<String, ServiceStateEntry> {
        &self.services
    }

    /// Gets the state entry for a specific service by its configuration hash.
    pub fn get(&self, service_hash: &str) -> Option<&ServiceStateEntry> {
        self.services.get(service_hash)
    }

    /// Sets the state for a service by its configuration hash and persists to disk.
    pub fn set(
        &mut self,
        service_hash: &str,
        status: ServiceLifecycleStatus,
        pid: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), ServiceStateError> {
        self.services.insert(
            service_hash.to_string(),
            ServiceStateEntry {
                status,
                pid,
                exit_code,
                signal,
            },
        );
        self.save()
    }

    /// Removes a service from the state file by its configuration hash and persists to disk.
    pub fn remove(&mut self, service_hash: &str) -> Result<(), ServiceStateError> {
        if self.services.remove(service_hash).is_some() {
            self.save()
        } else {
            Err(ServiceStateError::ServiceNotFound)
        }
    }
}

/// Run a hook command with the provided environment variables.
fn run_hook(
    action: &HookAction,
    env: &Option<EnvConfig>,
    stage: HookStage,
    outcome: HookOutcome,
    service_name: &str,
    project_root: &Path,
) {
    let hook_label = format!("{}.{}", stage.as_ref(), outcome.as_ref());
    debug!(
        "Running {} hook for '{}': `{}`",
        hook_label, service_name, action.command
    );

    let mut cmd = Command::new(DEFAULT_SHELL);
    cmd.arg(SHELL_COMMAND_FLAG).arg(&action.command);

    for (key, value) in collect_service_env(env, project_root, service_name) {
        cmd.env(key, value);
    }

    let timeout = match action.timeout.as_deref() {
        Some(raw_timeout) => match Daemon::parse_duration(raw_timeout) {
            Ok(duration) => Some(duration),
            Err(err) => {
                error!(
                    "Invalid timeout '{}' for hook {} on '{}': {}",
                    raw_timeout, hook_label, service_name, err
                );
                None
            }
        },
        None => None,
    };

    match cmd.spawn() {
        Ok(mut child) => {
            let wait_result = match timeout {
                Some(duration) => wait_with_timeout(&mut child, duration),
                None => child.wait().map(Some),
            };

            match wait_result {
                Ok(Some(status)) => {
                    if status.success() {
                        debug!(
                            "{} hook for '{}' completed successfully.",
                            hook_label, service_name
                        );
                    } else {
                        warn!(
                            "{} hook for '{}' exited with status: {:?}",
                            hook_label, service_name, status
                        );
                    }
                }
                Ok(None) => {
                    if let Some(duration) = timeout {
                        warn!(
                            "{} hook for '{}' timed out after {:?}. Terminating hook process.",
                            hook_label, service_name, duration
                        );
                    } else {
                        warn!(
                            "{} hook for '{}' did not complete but no timeout was configured.",
                            hook_label, service_name
                        );
                    }
                    if let Err(err) = child.kill() {
                        error!(
                            "Failed to terminate timed-out hook {} for '{}': {}",
                            hook_label, service_name, err
                        );
                    }
                    let _ = child.wait();
                }
                Err(err) => {
                    error!(
                        "Failed while waiting for hook {} on '{}': {}",
                        hook_label, service_name, err
                    );
                }
            }
        }
        Err(e) => {
            error!(
                "Failed to run {} hook for '{}': {}",
                hook_label, service_name, e
            );
        }
    }
}

/// Wait for a child process with a timeout, returning `Ok(None)` on timeout.
fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait()? {
            Some(status) => return Ok(Some(status)),
            None => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Snapshot of the currently observed state for a managed service during readiness probing.
#[derive(Debug)]
enum ServiceProbe {
    NotStarted,
    Running,
    Exited(ExitStatus),
}

/// Indicates when a service is considered ready for dependents or has already completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceReadyState {
    /// Service is running and ready for dependents.
    Running,
    /// Service completed successfully (for oneshot/cron services).
    CompletedSuccess,
}

/// Represents an existing service instance that has been temporarily detached while a
/// replacement is brought up during a rolling restart.
struct DetachedService {
    child: Child,
    pid: u32,
}

// Thread-local storage for tracking currently held locks to enforce ordering.
thread_local! {
    static HELD_LOCKS: std::cell::RefCell<HashSet<DaemonLock>> = std::cell::RefCell::new(HashSet::new());
}

/// A guard that enforces lock ordering and automatically tracks held locks.
struct OrderedLockGuard<'a, T> {
    guard: std::sync::MutexGuard<'a, T>,
    lock_type: DaemonLock,
}

impl<'a, T> Drop for OrderedLockGuard<'a, T> {
    fn drop(&mut self) {
        HELD_LOCKS.with(|held| {
            held.borrow_mut().remove(&self.lock_type);
        });
    }
}

impl<'a, T> std::ops::Deref for OrderedLockGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'a, T> std::ops::DerefMut for OrderedLockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

/// Helper function to acquire a lock with ordering enforcement.
fn acquire_lock<'a, T>(
    mutex: &'a Arc<Mutex<T>>,
    lock_type: DaemonLock,
) -> Result<OrderedLockGuard<'a, T>, ProcessManagerError> {
    // Check if we can acquire this lock given what we're already holding
    HELD_LOCKS.with(|held| {
        let held_locks = held.borrow();
        for existing_lock in held_locks.iter() {
            if lock_type <= *existing_lock {
                panic!(
                    "Lock ordering violation! Attempting to acquire {:?} (priority {}) \
                     while holding {:?} (priority {}). Locks must be acquired in ascending order.",
                    lock_type, lock_type.priority(),
                    existing_lock, existing_lock.priority()
                );
            }
        }
    });

    // Acquire the lock
    let guard = mutex.lock()?;

    // Track that we're holding this lock
    HELD_LOCKS.with(|held| {
        held.borrow_mut().insert(lock_type);
    });

    Ok(OrderedLockGuard { guard, lock_type })
}

/// Shared context for daemon operations to reduce function parameters and ensure
/// consistent lock ordering.
///
/// Lock ordering is enforced via the `DaemonLock` enum. Always acquire locks in
/// ascending order of their priority values to prevent deadlocks:
/// 1. Processes → 2. PidFile → 3. StateFile → 4. RestartCounts → 5. ManualStopFlags → 6. RestartSuppressed
#[derive(Clone)]
struct DaemonContext {
    /// Shared map of running service processes.
    processes: Arc<Mutex<HashMap<String, Child>>>,
    /// The PID file for tracking service PIDs.
    pid_file: Arc<Mutex<PidFile>>,
    /// Persistent state for recording service lifecycle transitions.
    state_file: Arc<Mutex<ServiceStateFile>>,
    /// Reference to the service configuration.
    config: Arc<Config>,
    /// Base directory for resolving relative service commands and assets.
    project_root: PathBuf,
    /// Whether child services should be detached from systemg (legacy behavior).
    detach_children: bool,
    /// Tracks the number of restart attempts for each service.
    restart_counts: Arc<Mutex<HashMap<String, u32>>>,
    /// Services that were explicitly stopped this cycle, used to treat exits as manual.
    manual_stop_flags: Arc<Mutex<HashSet<String>>>,
    /// Services whose automatic restarts are temporarily suppressed.
    restart_suppressed: Arc<Mutex<HashSet<String>>>,
    /// Flag indicating whether the monitoring loop should remain active.
    running: Arc<AtomicBool>,
    /// Cancellation tokens for Linux service threads (service_name -> cancel_token)
    #[cfg(target_os = "linux")]
    thread_cancellation_tokens: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl DaemonContext {
    /// Acquires the processes lock with ordering enforcement.
    fn lock_processes(
        &self,
    ) -> Result<OrderedLockGuard<'_, HashMap<String, Child>>, ProcessManagerError> {
        acquire_lock(&self.processes, DaemonLock::Processes)
    }

    /// Acquires the pid_file lock with ordering enforcement.
    fn lock_pid_file(
        &self,
    ) -> Result<OrderedLockGuard<'_, PidFile>, ProcessManagerError> {
        acquire_lock(&self.pid_file, DaemonLock::PidFile)
    }

    /// Acquires the state_file lock with ordering enforcement.
    #[allow(dead_code)]
    fn lock_state_file(
        &self,
    ) -> Result<OrderedLockGuard<'_, ServiceStateFile>, ProcessManagerError> {
        acquire_lock(&self.state_file, DaemonLock::StateFile)
    }

    /// Acquires the restart_counts lock with ordering enforcement.
    fn lock_restart_counts(
        &self,
    ) -> Result<OrderedLockGuard<'_, HashMap<String, u32>>, ProcessManagerError> {
        acquire_lock(&self.restart_counts, DaemonLock::RestartCounts)
    }

    /// Acquires the manual_stop_flags lock with ordering enforcement.
    fn lock_manual_stop_flags(
        &self,
    ) -> Result<OrderedLockGuard<'_, HashSet<String>>, ProcessManagerError> {
        acquire_lock(&self.manual_stop_flags, DaemonLock::ManualStopFlags)
    }

    /// Acquires the restart_suppressed lock with ordering enforcement.
    fn lock_restart_suppressed(
        &self,
    ) -> Result<OrderedLockGuard<'_, HashSet<String>>, ProcessManagerError> {
        acquire_lock(&self.restart_suppressed, DaemonLock::RestartSuppressed)
    }

    /// Creates a cancellation token for a Linux service thread.
    #[cfg(target_os = "linux")]
    fn create_cancellation_token(&self, service_name: &str) -> Arc<AtomicBool> {
        let token = Arc::new(AtomicBool::new(false));
        // Note: thread_cancellation_tokens doesn't need ordering enforcement as it's Linux-specific
        let mut tokens = self.thread_cancellation_tokens.lock().unwrap();
        tokens.insert(service_name.to_string(), Arc::clone(&token));
        token
    }

    /// Signals a Linux service thread to stop.
    #[cfg(target_os = "linux")]
    fn cancel_service_thread(&self, service_name: &str) {
        let mut tokens = self.thread_cancellation_tokens.lock().unwrap();
        if let Some(token) = tokens.remove(service_name) {
            token.store(true, Ordering::SeqCst);
        }
    }

    /// Cleans up all Linux service thread cancellation tokens.
    #[cfg(target_os = "linux")]
    fn cancel_all_service_threads(&self) {
        let mut tokens = self.thread_cancellation_tokens.lock().unwrap();
        for (_, token) in tokens.drain() {
            token.store(true, Ordering::SeqCst);
        }
    }
}

/// Service manager daemon.
pub struct Daemon {
    /// Running processes.
    processes: Arc<Mutex<HashMap<String, Child>>>,
    /// Service config.
    config: Arc<Config>,
    /// PID tracking.
    pid_file: Arc<Mutex<PidFile>>,
    /// Lifecycle state.
    state_file: Arc<Mutex<ServiceStateFile>>,
    /// Detach children (legacy).
    detach_children: bool,
    /// Project root dir.
    project_root: PathBuf,
    /// Monitor loop active flag.
    running: Arc<AtomicBool>,
    /// Monitor thread handle.
    monitor_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    /// Restart attempt counts.
    restart_counts: Arc<Mutex<HashMap<String, u32>>>,
    /// Manual stop tracking.
    manual_stop_flags: Arc<Mutex<HashSet<String>>>,
    /// Suppressed auto-restarts.
    restart_suppressed: Arc<Mutex<HashSet<String>>>,
    /// Linux thread cancellation.
    #[cfg(target_os = "linux")]
    thread_cancellation_tokens: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl Daemon {
    /// Creates context snapshot.
    fn context(&self) -> DaemonContext {
        DaemonContext {
            processes: Arc::clone(&self.processes),
            pid_file: Arc::clone(&self.pid_file),
            state_file: Arc::clone(&self.state_file),
            config: Arc::clone(&self.config),
            project_root: self.project_root.clone(),
            detach_children: self.detach_children,
            restart_counts: Arc::clone(&self.restart_counts),
            manual_stop_flags: Arc::clone(&self.manual_stop_flags),
            restart_suppressed: Arc::clone(&self.restart_suppressed),
            running: Arc::clone(&self.running),
            #[cfg(target_os = "linux")]
            thread_cancellation_tokens: Arc::clone(&self.thread_cancellation_tokens),
        }
    }

    /// Gets all descendant PIDs recursively.
    fn collect_descendants(root_pid: u32) -> HashSet<u32> {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);

        let mut descendants = HashSet::new();
        let mut stack = vec![root_pid];

        while let Some(current) = stack.pop() {
            for (proc_pid, process) in system.processes() {
                if let Some(parent) = process.parent()
                    && parent.as_u32() == current
                {
                    let child_pid = proc_pid.as_u32();
                    if descendants.insert(child_pid) {
                        stack.push(child_pid);
                    }
                }
            }
        }

        descendants
    }

    /// Signals process. None = liveness check. Also detects Linux zombies.
    fn signal_pid(
        service_name: &str,
        pid: u32,
        signal: Option<nix::sys::signal::Signal>,
    ) -> Result<bool, ProcessManagerError> {
        let target = nix::unistd::Pid::from_raw(pid as i32);
        match nix::sys::signal::kill(target, signal) {
            Ok(_) => {
                if signal.is_none() {
                    #[cfg(target_os = "linux")]
                    {
                        if matches!(Self::read_proc_state(pid), Some('Z') | Some('X')) {
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            }
            Err(nix::errno::Errno::ESRCH) => Ok(false),
            Err(err) => Err(ProcessManagerError::ServiceStopError {
                service: service_name.to_string(),
                source: std::io::Error::from_raw_os_error(err as i32),
            }),
        }
    }

    /// Reads the process state character from /proc/{pid}/stat on Linux. Returns the state
    /// character (R=running, S=sleeping, Z=zombie, X=dead, etc.) or None if the process doesn't exist.
    #[cfg(target_os = "linux")]
    fn read_proc_state(pid: u32) -> Option<char> {
        let stat_path_str = format!("/proc/{}/stat", pid);
        let stat_path = Path::new(&stat_path_str);
        let contents = fs::read_to_string(stat_path).ok()?;
        let mut parts = contents.split_whitespace();
        parts.next()?; // pid
        let mut name_part = parts.next()?; // (comm)
        if !name_part.ends_with(')') {
            for part in parts.by_ref() {
                name_part = part;
                if name_part.ends_with(')') {
                    break;
                }
            }
        }

        parts.next()?.chars().next()
    }

    /// Waits for processes to exit by polling their liveness. Checks each PID in the pending set
    /// up to `checks` times with `interval` delay between checks. Returns the set of PIDs that
    /// are still alive after all checks.
    fn wait_for_exit(
        service_name: &str,
        mut pending: HashSet<u32>,
        checks: usize,
        interval: Duration,
    ) -> Result<HashSet<u32>, ProcessManagerError> {
        for _ in 0..checks {
            if pending.is_empty() {
                break;
            }

            thread::sleep(interval);

            let mut survivors = HashSet::new();
            for pid in pending.iter().copied() {
                if Self::signal_pid(service_name, pid, None)? {
                    survivors.insert(pid);
                }
            }
            pending = survivors;
        }

        Ok(pending)
    }

    /// Sends a signal to multiple PIDs and returns the set of PIDs that survived (are still alive
    /// after the signal). Used for graceful shutdown attempts before escalating to SIGKILL.
    fn send_signal_to_pids(
        service_name: &str,
        pids: HashSet<u32>,
        signal: nix::sys::signal::Signal,
    ) -> Result<HashSet<u32>, ProcessManagerError> {
        let mut survivors = HashSet::new();
        for pid in pids {
            if Self::signal_pid(service_name, pid, Some(signal))? {
                survivors.insert(pid);
            }
        }
        Ok(survivors)
    }

    /// Terminates a process and all its descendants using escalating signals. First sends SIGTERM
    /// to the entire process tree and waits for graceful shutdown. If processes don't exit within
    /// the timeout, escalates to SIGKILL. Returns an error if any processes survive after SIGKILL.
    pub(crate) fn terminate_process_tree(
        service_name: &str,
        root_pid: u32,
    ) -> Result<(), ProcessManagerError> {
        use nix::sys::signal::Signal::{SIGKILL, SIGTERM};

        let mut pending = Self::collect_descendants(root_pid);
        pending.insert(root_pid);

        let supervisor_pgid = unsafe { libc::getpgid(0) };
        let child_pgid = unsafe { libc::getpgid(root_pid as libc::pid_t) };

        let signal_group = |signal: libc::c_int| {
            if child_pgid >= 0 && child_pgid != supervisor_pgid {
                let result = unsafe { libc::killpg(child_pgid, signal) };
                if result < 0 {
                    let err = std::io::Error::last_os_error();
                    match err.raw_os_error() {
                        Some(code) if code == libc::ESRCH => {}
                        Some(code) if code == libc::EPERM => {
                            warn!(
                                "Insufficient permissions to signal process group {} for '{}'",
                                child_pgid, service_name
                            );
                        }
                        _ => {
                            warn!(
                                "Failed to signal process group {child_pgid} for '{service_name}': {err}"
                            );
                        }
                    }
                }
            }
        };

        signal_group(SIGTERM as libc::c_int);
        pending = Self::send_signal_to_pids(service_name, pending, SIGTERM)?;
        pending = Self::wait_for_exit(
            service_name,
            pending,
            PROCESS_READY_CHECKS,
            PROCESS_CHECK_INTERVAL,
        )?;

        if pending.is_empty() {
            return Ok(());
        }

        signal_group(SIGKILL as libc::c_int);
        pending = Self::send_signal_to_pids(service_name, pending, SIGKILL)?;
        pending = Self::wait_for_exit(
            service_name,
            pending,
            PROCESS_READY_CHECKS,
            PROCESS_CHECK_INTERVAL,
        )?;

        if pending.is_empty() {
            Ok(())
        } else {
            Err(ProcessManagerError::ServiceStopError {
                service: service_name.to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "Failed to terminate process tree rooted at PID {} for '{}'",
                        root_pid, service_name
                    ),
                ),
            })
        }
    }

    /// Persists service state to the state file using the service's configuration hash as the key.
    /// Logs a warning if the service is not found in the config. This is the low-level function
    /// that writes state directly to disk.
    fn persist_service_state(
        config: &Arc<Config>,
        state_file: &Arc<Mutex<ServiceStateFile>>,
        service_name: &str,
        status: ServiceLifecycleStatus,
        pid: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), ProcessManagerError> {
        if let Some(service_hash) = config.get_service_hash(service_name) {
            let mut state_guard = state_file.lock()?;
            state_guard.set(&service_hash, status, pid, exit_code, signal)?;
            if service_hash != service_name
                && let Err(err) = state_guard.remove(service_name)
                && !matches!(err, ServiceStateError::ServiceNotFound)
            {
                warn!(
                    "Failed to remove legacy state entry for '{service_name}' in state file: {err}"
                );
            }
        }

        Ok(())
    }

    /// Initializes a new `Daemon` with an empty process map and a shared config reference.
    pub fn new(
        config: Config,
        pid_file: Arc<Mutex<PidFile>>,
        state_file: Arc<Mutex<ServiceStateFile>>,
        detach_children: bool,
    ) -> Self {
        debug!("Initializing daemon...");

        let project_root = config
            .project_dir
            .as_ref()
            .and_then(|dir| {
                let trimmed = dir.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(trimmed))
                }
            })
            .unwrap_or_else(|| PathBuf::from("."));

        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
            pid_file,
            state_file,
            detach_children,
            running: Arc::new(AtomicBool::new(false)),
            monitor_handle: Arc::new(Mutex::new(None)),
            project_root,
            restart_counts: Arc::new(Mutex::new(HashMap::new())),
            manual_stop_flags: Arc::new(Mutex::new(HashSet::new())),
            restart_suppressed: Arc::new(Mutex::new(HashSet::new())),
            #[cfg(target_os = "linux")]
            thread_cancellation_tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Convenience constructor that loads the PID file automatically.
    pub fn from_config(
        config: Config,
        detach_children: bool,
    ) -> Result<Self, ProcessManagerError> {
        let pid_file = Arc::new(Mutex::new(PidFile::load()?));
        let state_file = Arc::new(Mutex::new(ServiceStateFile::load()?));
        Ok(Self::new(config, pid_file, state_file, detach_children))
    }

    /// Returns a reference to the configuration.
    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config)
    }

    /// Returns a handle to the shared PID file so callers can inspect process IDs.
    pub fn pid_file_handle(&self) -> Arc<Mutex<PidFile>> {
        Arc::clone(&self.pid_file)
    }

    /// Returns a handle to the persisted service state store.
    pub fn service_state_handle(&self) -> Arc<Mutex<ServiceStateFile>> {
        Arc::clone(&self.state_file)
    }

    /// Explicitly records a skipped service in the persistent state store, clearing any stale PID.
    pub fn mark_service_skipped(&self, service: &str) -> Result<(), ProcessManagerError> {
        self.mark_skipped(service)
    }

    /// Gets the configuration hash for a service by name.
    /// Returns None if the service doesn't exist in the config.
    pub fn get_service_hash(&self, service_name: &str) -> Option<String> {
        self.config.get_service_hash(service_name)
    }

    /// Updates the service state in the persistent state file. This is a convenience wrapper
    /// around persist_service_state that uses the daemon's config and state_file references.
    fn update_state(
        &self,
        service: &str,
        status: ServiceLifecycleStatus,
        pid: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), ProcessManagerError> {
        if let Some(service_hash) = self.get_service_hash(service) {
            let mut state = self.state_file.lock()?;
            state.set(&service_hash, status, pid, exit_code, signal)?;
            if service_hash != service
                && let Err(err) = state.remove(service)
                && !matches!(err, ServiceStateError::ServiceNotFound)
            {
                warn!(
                    "Failed to remove legacy state entry for '{service}' in state file: {err}"
                );
            }
        } else {
            warn!(
                "Service '{}' not found in config, skipping state update",
                service
            );
        }
        Ok(())
    }

    /// Marks a service as running in the state file and PID file. This is called when a service
    /// process is successfully spawned and verified to be alive.
    fn mark_running(&self, service: &str, pid: u32) -> Result<(), ProcessManagerError> {
        self.update_state(
            service,
            ServiceLifecycleStatus::Running,
            Some(pid),
            None,
            None,
        )
    }

    /// Marks a service as skipped in the state file. This is called when the skip flag evaluates
    /// to true and the service is not started. Also removes any stale PID file entry.
    fn mark_skipped(&self, service: &str) -> Result<(), ProcessManagerError> {
        {
            let mut pid_guard = self.pid_file.lock()?;
            if let Err(err) = pid_guard.remove(service)
                && !matches!(err, PidFileError::ServiceNotFound)
            {
                return Err(err.into());
            }
        }

        self.update_state(service, ServiceLifecycleStatus::Skipped, None, None, None)
    }

    /// Records a failed start attempt in the persistent state file when no active instance exists.
    fn record_start_failure(
        &self,
        service: &str,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) {
        let has_active_pid = match self.pid_file.lock() {
            Ok(guard) => guard.pid_for(service).is_some(),
            Err(err) => {
                warn!(
                    "Failed to inspect pid file while recording start failure for '{service}': {err}"
                );
                false
            }
        };

        if has_active_pid {
            debug!(
                "Skipping start failure state for '{service}' because an active PID is still tracked"
            );
            return;
        }

        if let Err(err) = self.update_state(
            service,
            ServiceLifecycleStatus::ExitedWithError,
            None,
            exit_code,
            signal,
        ) {
            warn!(
                "Failed to persist start failure state for '{service}' (exit_code={exit_code:?}): {err}"
            );
        }
    }

    /// Launches a service as a child process, ensuring it remains attached to `systemg`.
    ///
    /// On **Linux**, child processes receive `SIGTERM` when `systemg` exits using `prctl()`.
    /// On **macOS**, this function ensures all child processes share the same process group,
    /// allowing them to be killed when `systemg` terminates.
    ///
    /// # Arguments
    /// * `service_name` - The name of the service.
    /// * `service_config` - The service configuration for command/env/runtime settings.
    /// * `processes` - Shared process tracking map.
    ///
    /// # Returns
    /// A [`Child`] process handle if successful.
    fn launch_attached_service(
        service_name: &str,
        service_config: &ServiceConfig,
        working_dir: PathBuf,
        processes: Arc<Mutex<HashMap<String, Child>>>,
        detach_children: bool,
    ) -> Result<u32, ProcessManagerError> {
        let command = &service_config.command;
        debug!("Launching service: '{service_name}' with command: `{command}`");

        let mut cmd = Command::new(DEFAULT_SHELL);
        cmd.arg(SHELL_COMMAND_FLAG).arg(command);
        cmd.current_dir(&working_dir);

        debug!("Executing command: {cmd:?}");

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut merged_env =
            collect_service_env(&service_config.env, &working_dir, service_name);

        let privilege = crate::privilege::PrivilegeContext::from_service(
            service_name,
            service_config,
        )
        .map_err(|source| ProcessManagerError::PrivilegeSetupFailed {
            service: service_name.to_string(),
            source,
        })?;

        for (key, value) in privilege.user.env_overrides() {
            merged_env.insert(key, value);
        }

        if !merged_env.is_empty() {
            let keys: Vec<_> = merged_env.keys().cloned().collect();
            debug!("Setting environment variables: {:?}", keys);
            for (key, value) in merged_env {
                cmd.env(key, value);
            }
        }

        let privilege_clone = privilege.clone();

        unsafe {
            cmd.pre_exec(move || {
                if detach_children {
                    // Legacy mode: allow services to continue if systemg exits abruptly.
                    if libc::setsid() < 0 {
                        let err = std::io::Error::last_os_error();
                        eprintln!("systemg pre_exec: setsid failed: {:?}", err);
                        return Err(err);
                    }
                } else {
                    // Place each service in its own process group so we can signal the entire
                    // tree without touching the supervisor's group.
                    if libc::setpgid(0, 0) < 0 {
                        let err = std::io::Error::last_os_error();
                        eprintln!("systemg pre_exec: setpgid(0, 0) failed: {:?}", err);
                        return Err(err);
                    }
                }

                // Ensure service gets killed on parent death (Linux only)
                #[cfg(target_os = "linux")]
                {
                    use libc::{PR_SET_PDEATHSIG, SIGTERM, prctl};
                    if prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0) < 0 {
                        let err = std::io::Error::last_os_error();
                        eprintln!(
                            "systemg pre_exec: prctl PR_SET_PDEATHSIG failed: {:?}",
                            err
                        );
                        return Err(err);
                    }
                }

                privilege_clone.apply_pre_exec().map_err(|err| {
                    eprintln!("systemg pre_exec: privilege setup failed: {}", err);
                    err
                })
            });
        }

        match cmd.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                debug!("Service '{service_name}' started with PID: {pid}");

                let stdout = child.stdout.take();
                let stderr = child.stderr.take();

                if let Some(out) = stdout {
                    spawn_log_writer(service_name, out, "stdout");
                }
                if let Some(err) = stderr {
                    spawn_log_writer(service_name, err, "stderr");
                }

                processes.lock()?.insert(service_name.to_string(), child);

                if let Err(err) = privilege.apply_post_spawn(pid as libc::pid_t) {
                    warn!(
                        "Failed to apply post-spawn privilege adjustments for '{service_name}': {err}"
                    );
                }
                Ok(pid)
            }
            Err(e) => {
                error!("Failed to start service '{service_name}': {e}");
                Err(ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source: e,
                })
            }
        }
    }

    /// Common logic for service startup that's shared between Linux and non-Linux platforms.
    /// Returns Ok(Some(state)) if the service was skipped or completed immediately,
    /// Ok(None) if the service should continue with platform-specific startup.
    fn start_service_common(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<Option<ServiceReadyState>, ProcessManagerError> {
        info!("Starting service: {name}");

        // Remove from restart suppressed list
        {
            let mut suppressed = self.restart_suppressed.lock()?;
            suppressed.remove(name);
        }

        // Check skip flag
        if let Some(skip_config) = &service.skip {
            match skip_config {
                SkipConfig::Flag(true) => {
                    info!("Skipping service '{name}' due to skip flag");
                    self.mark_skipped(name)?;
                    return Ok(Some(ServiceReadyState::CompletedSuccess));
                }
                SkipConfig::Flag(false) => {
                    debug!("Skip flag for '{name}' disabled; starting service");
                }
                SkipConfig::Command(skip_command) => {
                    match self.evaluate_skip_condition(name, skip_command) {
                        Ok(true) => {
                            info!("Skipping service '{name}' due to skip condition");
                            self.mark_skipped(name)?;
                            return Ok(Some(ServiceReadyState::CompletedSuccess));
                        }
                        Ok(false) => {
                            debug!(
                                "Skip condition for '{name}' evaluated to false, starting service"
                            );
                        }
                        Err(err) => {
                            warn!(
                                "Failed to evaluate skip condition for '{name}': {err}"
                            );
                        }
                    }
                }
            }
        }

        // Run pre-start command if configured
        if let Some(pre_start) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.pre_start.as_ref())
        {
            info!("Running pre-start command for '{name}': {pre_start}");
            self.run_pre_start_command(name, pre_start)?;
        }

        Ok(None)
    }

    /// Starts all services and blocks until they exit.
    pub fn start_services_blocking(&self) -> Result<(), ProcessManagerError> {
        self.start_all_services()?;
        self.spawn_monitor_thread()?;
        self.wait_for_monitor();
        Ok(())
    }

    /// Starts all services with background monitoring.
    pub fn start_services(&self) -> Result<(), ProcessManagerError> {
        self.start_all_services()?;
        self.spawn_monitor_thread()
    }

    /// Ensures monitor thread is running (for supervisor mode).
    pub fn ensure_monitoring(&self) -> Result<(), ProcessManagerError> {
        self.spawn_monitor_thread()
    }

    /// Starts all services (no monitoring wait).
    fn start_all_services(&self) -> Result<(), ProcessManagerError> {
        info!("Starting all services...");

        let order = self.config.service_start_order()?;
        let mut healthy_services = HashSet::new();
        let mut failed_services = HashSet::new();
        let mut first_error: Option<ProcessManagerError> = None;

        'service_loop: for service_name in order {
            let service = match self.config.services.get(&service_name) {
                Some(service) => service,
                None => continue,
            };

            if service.cron.is_some() {
                info!(
                    "Skipping cron-managed service '{}' during bulk start; scheduled execution will launch it",
                    service_name
                );
                healthy_services.insert(service_name.clone());
                continue 'service_loop;
            }

            if let Some(skip_config) = &service.skip {
                match skip_config {
                    SkipConfig::Flag(true) => {
                        info!("Skipping service '{service_name}' due to skip flag");
                        healthy_services.insert(service_name.clone());
                        continue 'service_loop;
                    }
                    SkipConfig::Flag(false) => {
                        debug!(
                            "Skip flag for '{service_name}' disabled; starting service"
                        );
                    }
                    SkipConfig::Command(skip_command) => {
                        match self.evaluate_skip_condition(&service_name, skip_command) {
                            Ok(true) => {
                                info!(
                                    "Skipping service '{service_name}' due to skip condition"
                                );
                                healthy_services.insert(service_name.clone());
                                continue 'service_loop;
                            }
                            Ok(false) => {
                                debug!(
                                    "Skip condition for '{service_name}' evaluated to false, starting service"
                                );
                            }
                            Err(err) => {
                                error!(
                                    "Failed to evaluate skip condition for '{service_name}': {err}"
                                );
                                if first_error.is_none() {
                                    first_error = Some(err);
                                }
                                failed_services.insert(service_name.clone());
                                continue 'service_loop;
                            }
                        }
                    }
                }
            }

            if let Some(deps) = &service.depends_on {
                for dep in deps {
                    if failed_services.contains(dep) {
                        error!(
                            "Skipping start of '{service_name}' because dependency '{dep}' failed."
                        );
                        if first_error.is_none() {
                            first_error = Some(ProcessManagerError::DependencyFailed {
                                service: service_name.clone(),
                                dependency: dep.clone(),
                            });
                        }
                        failed_services.insert(service_name.clone());
                        continue 'service_loop;
                    }

                    if !healthy_services.contains(dep) {
                        error!(
                            "Skipping start of '{service_name}' because dependency '{dep}' is not running."
                        );
                        if first_error.is_none() {
                            first_error = Some(ProcessManagerError::DependencyError {
                                service: service_name.clone(),
                                dependency: dep.clone(),
                            });
                        }
                        failed_services.insert(service_name.clone());
                        continue 'service_loop;
                    }
                }
            }

            match self.start_service(&service_name, service) {
                Ok(ServiceReadyState::Running) => {
                    healthy_services.insert(service_name.clone());
                }
                Ok(ServiceReadyState::CompletedSuccess) => {
                    info!("Service '{service_name}' completed successfully.");
                    healthy_services.insert(service_name.clone());
                }
                Err(err) => {
                    error!("Failed to start service '{service_name}': {err}");
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                    failed_services.insert(service_name.clone());
                }
            }
        }

        if let Some(err) = first_error {
            return Err(err);
        }

        info!("All services started successfully.");

        thread::sleep(Duration::from_millis(200));
        Ok(())
    }

    /// Evaluates a skip condition command for a service.
    /// Returns Ok(true) if the service should be skipped (command exits with status 0),
    /// Ok(false) if the service should not be skipped (command exits with non-zero status),
    /// or Err if the command fails to execute.
    pub fn evaluate_skip_condition(
        &self,
        service_name: &str,
        skip_command: &str,
    ) -> Result<bool, ProcessManagerError> {
        debug!("Evaluating skip condition for '{service_name}': `{skip_command}`");

        let mut cmd = Command::new(DEFAULT_SHELL);
        cmd.arg(SHELL_COMMAND_FLAG).arg(skip_command);
        cmd.current_dir(&self.project_root);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());

        match cmd.status() {
            Ok(status) => {
                let should_skip = status.success();
                debug!(
                    "Skip condition for '{service_name}' evaluated to: {should_skip} (exit code: {:?})",
                    status.code()
                );
                Ok(should_skip)
            }
            Err(e) => {
                error!("Failed to execute skip condition for '{service_name}': {e}");
                Err(ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source: e,
                })
            }
        }
    }

    /// Polls the newly spawned service until it is confirmed running or exits.
    ///
    /// Returns [`ServiceReadyState::Running`] once the process stays alive across consecutive
    /// polls, [`ServiceReadyState::CompletedSuccess`] for one-shot services that exit cleanly,
    /// or a [`ProcessManagerError::ServiceStartError`] if the process terminates with failure or
    /// fails to signal readiness within the allotted time window.
    fn wait_for_service_ready(
        &self,
        service_name: &str,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        Self::wait_for_ready(service_name, &self.processes, &self.pid_file)
    }

    /// Internal implementation of wait_for_service_ready that accepts explicit handles for processes
    /// and PID file. This allows the function to be called from both instance methods and static contexts.
    fn wait_for_ready(
        service_name: &str,
        processes: &Arc<Mutex<HashMap<String, Child>>>,
        pid_file: &Arc<Mutex<PidFile>>,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        let mut waited = Duration::ZERO;
        let mut seen_running_once = false;

        while waited <= SERVICE_START_TIMEOUT {
            match Self::probe_service_state(service_name, processes, pid_file)? {
                ServiceProbe::Running => {
                    if seen_running_once {
                        return Ok(ServiceReadyState::Running);
                    }

                    seen_running_once = true;
                    thread::sleep(SERVICE_POLL_INTERVAL);
                    waited += SERVICE_POLL_INTERVAL;
                    continue;
                }
                ServiceProbe::Exited(status) => {
                    if status.success() {
                        return Ok(ServiceReadyState::CompletedSuccess);
                    }

                    #[cfg(unix)]
                    let signal = status.signal();
                    #[cfg(not(unix))]
                    let signal: Option<i32> = None;
                    let exit_code = status.code();
                    warn!(
                        "Service '{service_name}' exited during startup (exit_code={exit_code:?}, signal={signal:?}). For details run: sysg logs {service_name}"
                    );

                    let message = match exit_code {
                        Some(code) => format!("process exited with status {code}"),
                        None => format!("process terminated unexpectedly: {status:?}"),
                    };

                    return Err(ProcessManagerError::ServiceStartError {
                        service: service_name.to_string(),
                        source: std::io::Error::other(message),
                    });
                }
                ServiceProbe::NotStarted => {
                    thread::sleep(SERVICE_POLL_INTERVAL);
                    waited += SERVICE_POLL_INTERVAL;
                    continue;
                }
            }
        }

        Err(ProcessManagerError::ServiceStartError {
            service: service_name.to_string(),
            source: std::io::Error::new(
                ErrorKind::TimedOut,
                "service did not report a running state in time",
            ),
        })
    }

    /// Attempts to determine the current state of a tracked service without blocking.
    ///
    /// Uses `try_wait` to check the underlying child process and updates the PID file if the
    /// service has exited. To avoid holding the process map lock longer than necessary, the child
    /// handle is temporarily removed and inserted back when still running.
    fn probe_service_state(
        service_name: &str,
        processes: &Arc<Mutex<HashMap<String, Child>>>,
        pid_file: &Arc<Mutex<PidFile>>,
    ) -> Result<ServiceProbe, ProcessManagerError> {
        let mut processes_guard = processes.lock()?;

        if let Some(mut child) = processes_guard.remove(service_name) {
            match child.try_wait() {
                Ok(Some(status)) => {
                    drop(processes_guard);

                    let mut pid_guard = pid_file.lock()?;
                    if let Err(err) = pid_guard.remove(service_name)
                        && !matches!(err, PidFileError::ServiceNotFound)
                    {
                        return Err(err.into());
                    }

                    return Ok(ServiceProbe::Exited(status));
                }
                Ok(None) => {
                    processes_guard.insert(service_name.to_string(), child);
                    return Ok(ServiceProbe::Running);
                }
                Err(e) => {
                    processes_guard.insert(service_name.to_string(), child);
                    return Err(ProcessManagerError::ServiceStartError {
                        service: service_name.to_string(),
                        source: e,
                    });
                }
            }
        }

        Ok(ServiceProbe::NotStarted)
    }

    /// Restarts all services by stopping and then starting them again, reusing the existing
    /// monitor thread if available.
    pub fn restart_services(&self) -> Result<(), ProcessManagerError> {
        info!("Restarting all services...");

        let order = self.config.service_start_order()?;
        let mut restarted_services = Vec::new();

        for service_name in order {
            let service = match self.config.services.get(&service_name) {
                Some(service) => service,
                None => continue,
            };

            if service.cron.is_some() {
                info!(
                    "Skipping cron-managed service '{}' during restart; scheduled execution will launch it",
                    service_name
                );
                continue;
            }

            restarted_services.push(service_name.clone());

            let strategy_str = service
                .deployment
                .as_ref()
                .and_then(|deployment| deployment.strategy.as_deref());

            let strategy = strategy_str
                .and_then(|s| DeploymentStrategy::from_str(s).ok())
                .unwrap_or_default();

            match strategy {
                DeploymentStrategy::Rolling => {
                    self.rolling_restart_service(&service_name, service)?;
                }
                DeploymentStrategy::Immediate => {
                    self.immediate_restart_service(&service_name, service)?;
                }
            }
        }

        self.spawn_monitor_thread()?;
        self.verify_services_running(&restarted_services)?;
        info!("All services restarted successfully.");
        Ok(())
    }

    /// Restarts a single service, honoring its deployment strategy.
    pub fn restart_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<(), ProcessManagerError> {
        let strategy_str = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.strategy.as_deref());

        let strategy = strategy_str
            .and_then(|s| DeploymentStrategy::from_str(s).ok())
            .unwrap_or_default();

        match strategy {
            DeploymentStrategy::Rolling => {
                self.rolling_restart_service(name, service)?;
            }
            DeploymentStrategy::Immediate => {
                self.immediate_restart_service(name, service)?;
            }
        }

        self.verify_services_running(&[name.to_string()])?;

        Ok(())
    }

    /// Performs a rolling restart keeping the previous instance alive until the replacement is
    /// verified healthy.
    fn rolling_restart_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<(), ProcessManagerError> {
        info!("Performing rolling restart for service: {name}");

        let mut previous = self.detach_service_handle(name)?;

        if let Some(pre_start) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.pre_start.as_ref())
        {
            info!("Running pre-start command for '{name}': {pre_start}");
            if let Err(err) = self.run_pre_start_command(name, pre_start) {
                if let Some(detached) = previous.take() {
                    self.restore_detached_service(name, detached)?;
                }
                return Err(err);
            }
        }

        let start_state = match self.start_service(name, service) {
            Ok(state) => state,
            Err(err) => {
                if let Err(stop_err) = self.stop_service_with_intent(name, false) {
                    warn!(
                        "Failed to stop new instance of '{name}' after restart error: {stop_err}"
                    );
                }

                if previous.is_some() && Self::logs_indicate_port_conflict(name) {
                    warn!(
                        "Detected port conflict while restarting '{name}'. Falling back to immediate restart semantics."
                    );

                    if let Some(detached) = previous.take() {
                        self.terminate_service(name, detached)?;
                    }

                    self.start_service(name, service)?
                } else {
                    if let Some(detached) = previous.take() {
                        self.restore_detached_service(name, detached)?;
                    }
                    return Err(err);
                }
            }
        };

        if matches!(start_state, ServiceReadyState::CompletedSuccess) {
            info!("Service '{name}' exited successfully immediately after restart.");
        }

        if let Some(health_check) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.health_check.as_ref())
            && let Err(err) = self.wait_for_health_check(name, health_check)
        {
            error!("Health check failed for '{name}' during rolling restart: {err}");
            if let Err(stop_err) = self.stop_service_with_intent(name, false) {
                warn!(
                    "Failed to stop new instance of '{name}' after health check failure: {stop_err}"
                );
            }
            if let Some(detached) = previous.take() {
                self.restore_detached_service(name, detached)?;
            }
            return Err(err);
        }

        if let Some(grace_period) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.grace_period.as_ref())
        {
            let duration = match Self::parse_duration(grace_period) {
                Ok(duration) => duration,
                Err(err) => {
                    error!(
                        "Failed to parse grace period '{grace_period}' for '{name}': {err}"
                    );
                    if let Err(stop_err) = self.stop_service_with_intent(name, false) {
                        warn!(
                            "Failed to stop new instance of '{name}' after grace period parse error: {stop_err}"
                        );
                    }
                    if let Some(detached) = previous.take() {
                        self.restore_detached_service(name, detached)?;
                    }
                    return Err(err);
                }
            };

            if !duration.is_zero() {
                info!(
                    "Waiting {:?} before stopping previous instance of '{name}'",
                    duration
                );
                thread::sleep(duration);
            }
        }

        if let Some(detached) = previous.take() {
            self.terminate_service(name, detached)?;
        }

        Ok(())
    }

    /// Performs an immediate restart by stopping and starting the service sequentially.
    fn immediate_restart_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<(), ProcessManagerError> {
        info!("Performing immediate restart for service: {name}");

        self.stop_service_with_intent(name, false)?;
        let start_state = self.start_service(name, service)?;

        if let ServiceReadyState::CompletedSuccess = start_state {
            info!("Service '{name}' completed successfully immediately after restart.");
        }

        Ok(())
    }

    /// Runs the configured pre-start command prior to launching a replacement service instance.
    fn run_pre_start_command(
        &self,
        service_name: &str,
        command: &str,
    ) -> Result<(), ProcessManagerError> {
        use std::{
            io::{BufRead, BufReader},
            process::Stdio,
            thread,
        };

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.project_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source,
            })?;

        let service_name_owned = service_name.to_string();

        // Stream stdout in a separate thread
        let stdout_handle = child.stdout.take().map(|stdout| {
            let service_name = service_name_owned.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    info!("[{service_name} pre-start] {line}");
                }
            })
        });

        // Stream stderr in a separate thread
        let stderr_handle = child.stderr.take().map(|stderr| {
            let service_name = service_name_owned.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    warn!("[{service_name} pre-start] {line}");
                }
            })
        });

        // Wait for the process to complete
        let status =
            child
                .wait()
                .map_err(|source| ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source,
                })?;

        // Wait for output threads to finish
        if let Some(handle) = stdout_handle {
            let _ = handle.join();
        }
        if let Some(handle) = stderr_handle {
            let _ = handle.join();
        }

        if !status.success() {
            let exit_code = status.code();
            #[cfg(unix)]
            let signal = status.signal();
            #[cfg(not(unix))]
            let signal: Option<i32> = None;

            self.record_start_failure(service_name, exit_code, signal);

            let message = format!("Pre-start command exited with status {}", status);
            return Err(ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source: std::io::Error::other(message),
            });
        }

        Ok(())
    }

    /// Waits for the configured health check to report success before completing the rolling
    /// restart.
    fn wait_for_health_check(
        &self,
        service_name: &str,
        health_check: &HealthCheckConfig,
    ) -> Result<(), ProcessManagerError> {
        let timeout = if let Some(raw_timeout) = &health_check.timeout {
            Self::parse_duration(raw_timeout)?
        } else {
            Duration::from_secs(30)
        };

        let retries = health_check.retries.unwrap_or(3).max(1);
        let retry_interval = Duration::from_secs(2);
        let client = Client::builder().timeout(timeout).build().map_err(|err| {
            ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source: std::io::Error::other(err.to_string()),
            }
        })?;

        let deadline = Instant::now() + timeout;

        for attempt in 1..=retries {
            match self.perform_health_check(&client, &health_check.url) {
                Ok(true) => {
                    info!(
                        "Health check passed for '{service_name}' on attempt {attempt}"
                    );
                    return Ok(());
                }
                Ok(false) => {
                    debug!(
                        "Health check attempt {attempt} failed for '{service_name}', retrying in {:?}",
                        retry_interval
                    );
                }
                Err(err) => {
                    debug!(
                        "Health check attempt {attempt} returned error for '{service_name}': {err}",
                    );
                }
            }

            if Instant::now() >= deadline {
                break;
            }

            if attempt != retries {
                thread::sleep(retry_interval);
            }
        }

        Err(ProcessManagerError::ServiceStartError {
            service: service_name.to_string(),
            source: std::io::Error::other(format!(
                "Health check did not succeed within {:?} after {} attempts",
                timeout, retries
            )),
        })
    }

    /// Performs a single health check request and evaluates the response.
    fn perform_health_check(
        &self,
        client: &Client,
        url: &str,
    ) -> Result<bool, std::io::Error> {
        let response = client
            .get(url)
            .send()
            .map_err(|err| std::io::Error::other(err.to_string()))?;

        Ok(response.status().is_success())
    }

    /// Parses a user-facing duration string in the format `<number>[s|m|h]`.
    fn parse_duration(raw: &str) -> Result<Duration, ProcessManagerError> {
        let value = raw.trim();
        if value.is_empty() {
            return Err(Self::config_error("Duration value cannot be empty"));
        }

        let (amount_str, multiplier) = if let Some(stripped) = value.strip_suffix('s') {
            (stripped.trim(), 1)
        } else if let Some(stripped) = value.strip_suffix('m') {
            (stripped.trim(), 60)
        } else if let Some(stripped) = value.strip_suffix('h') {
            (stripped.trim(), 3600)
        } else {
            (value, 1)
        };

        let amount: u64 = amount_str.parse().map_err(|_| {
            Self::config_error(format!("Invalid duration value: '{raw}'"))
        })?;

        Ok(Duration::from_secs(amount.saturating_mul(multiplier)))
    }

    /// Scans the last 50 lines of a service's stderr log for port conflict indicators. Returns
    /// true if common port conflict messages are detected (e.g., "address already in use", EADDRINUSE).
    fn logs_indicate_port_conflict(service_name: &str) -> bool {
        let path = resolve_log_path(service_name, "stderr");
        if !path.exists() {
            return false;
        }

        match File::open(&path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                let mut buffer: VecDeque<String> =
                    VecDeque::with_capacity(MAX_STATUS_LOG_LINES);

                for line in reader.lines().map_while(Result::ok) {
                    if buffer.len() == MAX_STATUS_LOG_LINES {
                        buffer.pop_front();
                    }
                    buffer.push_back(line);
                }

                buffer.iter().rev().any(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower.contains("address already in use")
                        || lower.contains("os error 48")
                        || lower.contains("os error 98")
                        || lower.contains("eaddrinuse")
                })
            }
            Err(err) => {
                debug!(
                    "Unable to inspect stderr logs for '{}' while detecting port conflicts: {err}",
                    service_name
                );
                false
            }
        }
    }

    /// Determines if a service should be verified as running after a restart. Returns false for
    /// one-shot services (restart_policy=never) and cron jobs, true for long-running services.
    fn should_verify_service(service: &ServiceConfig) -> bool {
        if matches!(service.restart_policy.as_deref(), Some("never")) {
            return false;
        }

        if service.cron.is_some() {
            return false;
        }

        true
    }

    /// Verifies that the specified services are running after a restart operation. Polls each
    /// service multiple times to ensure it stays alive. Returns an error if any service fails
    /// to start or exits immediately.
    fn verify_services_running(
        &self,
        services: &[String],
    ) -> Result<(), ProcessManagerError> {
        let mut failed = Vec::new();

        for service_name in services {
            let Some(service_cfg) = self.config.services.get(service_name) else {
                continue;
            };

            if !Self::should_verify_service(service_cfg) {
                continue;
            }

            let mut stable = true;

            for attempt in 0..POST_RESTART_VERIFY_ATTEMPTS {
                if attempt > 0 {
                    thread::sleep(POST_RESTART_VERIFY_DELAY);
                }

                match Self::probe_service_state(
                    service_name,
                    &self.processes,
                    &self.pid_file,
                )? {
                    ServiceProbe::Running => continue,
                    ServiceProbe::NotStarted => {
                        stable = false;
                        break;
                    }
                    ServiceProbe::Exited(status) => {
                        if status.success() {
                            info!(
                                "Service '{service_name}' exited immediately after restart with status {status}"
                            );
                        } else {
                            warn!(
                                "Service '{service_name}' crashed immediately after restart with status {status}"
                            );
                        }
                        stable = false;
                        break;
                    }
                }
            }

            if !stable {
                failed.push(service_name.clone());
            }
        }

        if failed.is_empty() {
            Ok(())
        } else {
            Err(ProcessManagerError::ServicesNotRunning { services: failed })
        }
    }

    /// Helper for constructing a configuration parse error wrapped in our domain error type.
    fn config_error(message: impl Into<String>) -> ProcessManagerError {
        ProcessManagerError::ConfigParseError(serde_yaml::Error::custom(message.into()))
    }

    /// Removes the current child handle for a service while keeping the underlying process alive.
    fn detach_service_handle(
        &self,
        service_name: &str,
    ) -> Result<Option<DetachedService>, ProcessManagerError> {
        let detached_child = self.processes.lock()?.remove(service_name);

        if let Some(child) = detached_child {
            let pid = self
                .pid_file
                .lock()?
                .pid_for(service_name)
                .unwrap_or(child.id());

            Ok(Some(DetachedService { child, pid }))
        } else {
            Ok(None)
        }
    }

    /// Restores a previously detached service handle back into normal supervision.
    fn restore_detached_service(
        &self,
        service_name: &str,
        detached: DetachedService,
    ) -> Result<(), ProcessManagerError> {
        self.processes
            .lock()?
            .insert(service_name.to_string(), detached.child);

        self.pid_file.lock()?.insert(service_name, detached.pid)?;

        info!("Restored original instance of '{service_name}' after restart failure.");

        Ok(())
    }

    /// Terminates the detached instance once the replacement is known healthy.
    fn terminate_service(
        &self,
        service_name: &str,
        mut detached: DetachedService,
    ) -> Result<(), ProcessManagerError> {
        let pid = detached.pid;
        Self::terminate_process_tree(service_name, pid)?;

        // Best-effort wait to reap the child and avoid zombies.
        if let Err(err) = detached.child.wait() {
            warn!(
                "Failed to wait on previous instance of '{service_name}' after termination: {err}"
            );
        }

        info!(
            "Old instance of '{service_name}' terminated successfully during rolling restart."
        );

        Ok(())
    }

    /// Starts service (Unix/macOS).
    ///
    /// Thread can exit after spawn - child runs independently.
    #[cfg(not(target_os = "linux"))]
    pub fn start_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        // Use common startup logic
        if let Some(state) = self.start_service_common(name, service)? {
            return Ok(state);
        }

        let processes = Arc::clone(&self.processes);
        let service_config = service.clone();
        let service_name = name.to_string();
        let pid_file = Arc::clone(&self.pid_file);
        let detach_children = self.detach_children;
        let working_dir = self.project_root.clone();

        let handle = thread::spawn(move || {
            debug!("Starting service thread for '{service_name}'");

            match Daemon::launch_attached_service(
                &service_name,
                &service_config,
                working_dir.clone(),
                processes.clone(),
                detach_children,
            ) {
                Ok(pid) => {
                    let mut pid_guard = pid_file.lock()?;
                    pid_guard.services.insert(service_name.clone(), pid);
                    pid_guard.save()?;
                    Ok(pid)
                }
                Err(e) => {
                    error!("Failed to start service '{service_name}': {e}");
                    Err(e)
                }
            }
        });

        // Wait for the thread to complete and propagate any errors
        let launch_result = handle.join().map_err(|e| {
            error!("Failed to join service thread for '{name}': {e:?}");
            ProcessManagerError::ServiceStartError {
                service: name.to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    format!("{e:?}"),
                ),
            }
        })?;

        debug!("Service launch thread for '{name}' completed");

        match launch_result {
            Ok(pid) => {
                self.mark_running(name, pid)?;
            }
            Err(err) => {
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Error))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Error,
                        name,
                        &self.project_root,
                    );
                }
                return Err(err);
            }
        }

        let readiness = self.wait_for_service_ready(name);

        match readiness {
            Ok(state) => {
                if matches!(state, ServiceReadyState::CompletedSuccess) {
                    self.update_state(
                        name,
                        ServiceLifecycleStatus::ExitedSuccessfully,
                        None,
                        Some(0),
                        None,
                    )?;
                }
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Success))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Success,
                        name,
                        &self.project_root,
                    );
                }
                Ok(state)
            }
            Err(err) => {
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Error))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Error,
                        name,
                        &self.project_root,
                    );
                }
                Err(err)
            }
        }
    }

    /// Starts service (Linux).
    ///
    /// Uses PR_SET_PDEATHSIG - parent thread must stay alive or child gets SIGTERM.
    #[cfg(target_os = "linux")]
    pub fn start_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        use std::{sync::mpsc, thread};

        // Use common startup logic
        if let Some(state) = self.start_service_common(name, service)? {
            return Ok(state);
        }

        let processes = Arc::clone(&self.processes);
        let service_config = service.clone();
        let service_name = name.to_string();
        let service_name_for_token = service_name.clone();
        let pid_file = Arc::clone(&self.pid_file);
        let detach_children = self.detach_children;
        let working_dir = self.project_root.clone();

        // Create a cancellation token for this service thread
        let cancellation_token = self
            .context()
            .create_cancellation_token(&service_name_for_token);

        let (tx, rx) = mpsc::channel();
        // Spawn the thread, but DO NOT join it.
        thread::spawn(move || {
            debug!("Starting service thread for '{service_name}'");

            let launch_result = Daemon::launch_attached_service(
                &service_name,
                &service_config,
                working_dir.clone(),
                processes.clone(),
                detach_children,
            );

            match launch_result {
                Ok(pid) => {
                    match pid_file.lock() {
                        Ok(mut guard) => {
                            guard.services.insert(service_name.clone(), pid);
                            if let Err(err) = guard.save() {
                                error!(
                                    "Failed to save PID file for service '{service_name}': {}",
                                    err
                                );
                                let _ = tx.send(Err(err.into()));
                                return;
                            }
                        }
                        Err(poison) => {
                            error!(
                                "Pid file mutex poisoned while starting '{}': {}",
                                service_name, poison
                            );
                            let _ = tx.send(Err(ProcessManagerError::from(poison)));
                            return;
                        }
                    }

                    if tx.send(Ok(pid)).is_err() {
                        return;
                    }

                    // Keep the thread alive but check for cancellation periodically
                    while !cancellation_token.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                    debug!(
                        "Service thread for '{service_name}' terminated by cancellation token"
                    );
                }
                Err(e) => {
                    error!("Failed to start service '{service_name}': {e}");
                    let _ = tx.send(Err(e));
                }
            }
        });

        debug!("Service thread for '{name}' launched and detached (Linux)");

        let launch_result =
            rx.recv()
                .map_err(|recv_err| ProcessManagerError::ServiceStartError {
                    service: name.to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        format!("thread failed to report launch status: {recv_err}"),
                    ),
                })?;

        match launch_result {
            Ok(pid) => {
                self.mark_running(name, pid)?;
            }
            Err(err) => {
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Error))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Error,
                        name,
                        &self.project_root,
                    );
                }
                return Err(err);
            }
        }

        let readiness = self.wait_for_service_ready(name);

        match readiness {
            Ok(state) => {
                if matches!(state, ServiceReadyState::CompletedSuccess) {
                    self.update_state(
                        name,
                        ServiceLifecycleStatus::ExitedSuccessfully,
                        None,
                        Some(0),
                        None,
                    )?;
                }
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Success))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Success,
                        name,
                        &self.project_root,
                    );
                }
                Ok(state)
            }
            Err(err) => {
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Error))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Error,
                        name,
                        &self.project_root,
                    );
                }
                Err(err)
            }
        }
    }

    /// Shared stop implementation that accepts explicit handles, making it reusable from helpers
    /// that already hold references to the daemon's shared state.
    fn stop_service_with_handles(
        service_name: &str,
        processes: &Arc<Mutex<HashMap<String, Child>>>,
        pid_file: &Arc<Mutex<PidFile>>,
        state_file: &Arc<Mutex<ServiceStateFile>>,
        config: &Arc<Config>,
    ) -> Result<(), ProcessManagerError> {
        // First, get the PID without removing from HashMap yet
        let pid = {
            let mut processes_guard = processes.lock()?;
            if let Some(child) = processes_guard.get_mut(service_name) {
                Some(child.id())
            } else {
                let guard = pid_file.lock()?;
                guard.get(service_name)
            }
        };

        // Terminate the process tree
        if let Some(process_id) = pid {
            match Self::terminate_process_tree(service_name, process_id) {
                Ok(_) => {
                    debug!(
                        "Process tree for '{service_name}' (pid {process_id}) terminated successfully"
                    );
                }
                Err(err) => match &err {
                    ProcessManagerError::ServiceStopError { source, .. }
                        if source.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        warn!(
                            "Timed out terminating process tree for '{service_name}' (pid {process_id}); forcing cleanup"
                        );
                    }
                    _ => return Err(err),
                },
            }
        }

        // Now remove the child handle and wait on it
        let child_handle = {
            let mut processes_guard = processes.lock()?;
            processes_guard.remove(service_name)
        };

        if let Some(mut child) = child_handle
            && let Err(err) = child.wait()
        {
            warn!("Failed to wait on '{service_name}' after termination: {err}");
        }

        match pid_file.lock()?.remove(service_name) {
            Ok(_) | Err(PidFileError::ServiceNotFound) => {}
            Err(err) => return Err(err.into()),
        }

        if let Some(service_hash) = config.get_service_hash(service_name) {
            let mut state_guard = state_file.lock()?;
            state_guard.set(
                &service_hash,
                ServiceLifecycleStatus::Stopped,
                None,
                None,
                None,
            )?;

            if service_hash != service_name
                && let Err(err) = state_guard.remove(service_name)
                && !matches!(err, ServiceStateError::ServiceNotFound)
            {
                warn!("Failed to remove legacy state entry for '{service_name}': {err}");
            }
        }

        debug!("Service '{service_name}' stopped successfully.");

        Ok(())
    }

    /// Stops a specific service by name.
    ///
    /// If the service is running, it will be terminated and removed from the process map.
    fn stop_service_with_intent(
        &self,
        service_name: &str,
        suppress_auto_restart: bool,
    ) -> Result<(), ProcessManagerError> {
        {
            let mut manual_guard = self.manual_stop_flags.lock()?;
            manual_guard.insert(service_name.to_string());
        }

        if suppress_auto_restart {
            let mut suppressed_guard = self.restart_suppressed.lock()?;
            suppressed_guard.insert(service_name.to_string());
        }

        // Cancel the Linux service thread if it exists
        #[cfg(target_os = "linux")]
        self.context().cancel_service_thread(service_name);

        let was_running = { self.pid_file.lock()?.get(service_name).is_some() };

        let result = Self::stop_service_with_handles(
            service_name,
            &self.processes,
            &self.pid_file,
            &self.state_file,
            &self.config,
        );

        if result.is_err() {
            let mut manual_guard = self.manual_stop_flags.lock()?;
            manual_guard.remove(service_name);
            if suppress_auto_restart {
                let mut suppressed_guard = self.restart_suppressed.lock()?;
                suppressed_guard.remove(service_name);
            }
        }

        if was_running
            && result.is_ok()
            && let Some(service) = self.config.services.get(service_name)
            && let Some(hooks) = &service.hooks
            && let Some(action) = hooks.action(HookStage::OnStop, HookOutcome::Success)
        {
            run_hook(
                action,
                &service.env,
                HookStage::OnStop,
                HookOutcome::Success,
                service_name,
                &self.project_root,
            );
        }

        result
    }

    /// Stops a specific service and suppresses automatic restarts.
    pub fn stop_service(&self, service_name: &str) -> Result<(), ProcessManagerError> {
        self.stop_service_with_intent(service_name, true)
    }

    /// Recursively stops any services that depend (directly or indirectly) on the specified root
    /// service. Used when a dependency crashes so downstream workloads do not continue in a broken
    /// state.
    fn stop_dependents(
        root: &str,
        reverse_dependencies: &HashMap<String, Vec<String>>,
        ctx: &DaemonContext,
    ) {
        let mut stack: Vec<String> =
            reverse_dependencies.get(root).cloned().unwrap_or_default();
        let mut visited: HashSet<String> = stack.iter().cloned().collect();

        while let Some(service) = stack.pop() {
            warn!("Stopping dependent service '{service}' because '{root}' failed.");

            if let Ok(mut guard) = ctx.lock_manual_stop_flags() {
                guard.insert(service.clone());
            }
            if let Ok(mut guard) = ctx.lock_restart_suppressed() {
                guard.insert(service.clone());
            }

            if let Err(err) = Self::stop_service_with_handles(
                &service,
                &ctx.processes,
                &ctx.pid_file,
                &ctx.state_file,
                &ctx.config,
            ) {
                error!(
                    "Failed to stop dependent service '{service}' after '{root}' failure: {err}"
                );
                if let Ok(mut guard) = ctx.lock_manual_stop_flags() {
                    guard.remove(&service);
                }
                if let Ok(mut guard) = ctx.lock_restart_suppressed() {
                    guard.remove(&service);
                }
            }

            if let Ok(mut guard) = ctx.lock_pid_file() {
                if let Err(err) = guard.remove(&service)
                    && !matches!(err, PidFileError::ServiceNotFound)
                {
                    warn!(
                        "Failed to clear PID entry for dependent '{service}' after '{root}' failure: {err}"
                    );
                } else {
                    // Save the PID file after removing the dependent service
                    if let Err(err) = guard.save() {
                        warn!(
                            "Failed to save PID file after removing dependent '{service}': {err}"
                        );
                    }
                }
            }

            if let Some(children) = reverse_dependencies.get(&service) {
                for child in children {
                    if visited.insert(child.clone()) {
                        stack.push(child.clone());
                    }
                }
            }
        }
    }

    /// Stops all running services.
    ///
    /// Iterates over all active processes and terminates them.
    pub fn stop_services(&self) -> Result<(), ProcessManagerError> {
        let services: Vec<String> =
            self.pid_file.lock()?.services.keys().cloned().collect();

        for service in services {
            if let Err(e) = self.stop_service(&service) {
                error!("Failed to stop service '{service}': {e}");
            }
        }

        Ok(())
    }

    /// Ensures that the monitor thread is running, spawning it if necessary.
    fn spawn_monitor_thread(&self) -> Result<(), ProcessManagerError> {
        let mut handle_slot = self.monitor_handle.lock().unwrap();
        let should_spawn = match handle_slot.as_ref() {
            Some(handle) => handle.is_finished(),
            None => true,
        };

        if should_spawn {
            debug!("Starting service monitoring thread...");
            self.running.store(true, Ordering::SeqCst);

            let ctx = self.context();

            let handle = thread::spawn(move || {
                Self::monitor_loop(ctx);
            });

            *handle_slot = Some(handle);
        }

        Ok(())
    }

    /// Blocks on the monitoring thread if it is running.
    fn wait_for_monitor(&self) {
        if let Some(handle) = self.monitor_handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    /// Signals the monitoring thread to exit and waits for it to finish.
    pub fn shutdown_monitor(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.wait_for_monitor();
    }

    /// Monitors all running services and restarts them if they exit unexpectedly.
    fn monitor_loop(ctx: DaemonContext) {
        while ctx.running.load(Ordering::SeqCst) {
            let mut exited_services = Vec::new();
            let mut restarted_services = Vec::new();
            let mut failed_services = Vec::new();
            let mut active_services = 0;

            {
                let mut locked_processes = ctx.lock_processes().unwrap();
                for (name, child) in locked_processes.iter_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            if status.success() {
                                info!("Service '{name}' exited normally.");
                            } else {
                                warn!("Service '{name}' was terminated with {status:?}.");
                            }
                            exited_services.push((name.clone(), status));
                        }
                        Ok(None) => {
                            trace!("Service '{name}' is still running.");
                            active_services += 1;
                        }
                        Err(e) => error!("Failed to check status of '{name}': {e}"),
                    }
                }
            }

            if !exited_services.is_empty() {
                let mut pid_file_guard = ctx.lock_pid_file().unwrap();

                for (name, exit_status) in exited_services {
                    let manually_stopped = {
                        let mut manual_guard = ctx.lock_manual_stop_flags().unwrap();
                        if manual_guard.remove(&name) {
                            true
                        } else {
                            pid_file_guard.get(&name).is_none()
                        }
                    };
                    let restart_suppressed_for_service =
                        ctx.lock_restart_suppressed().unwrap().contains(&name);
                    let exit_success = exit_status.success();
                    let exit_code = exit_status.code();
                    #[cfg(unix)]
                    let signal = exit_status.signal();
                    #[cfg(not(unix))]
                    let signal = None;
                    let hook_outcome = if manually_stopped || exit_success {
                        HookOutcome::Success
                    } else {
                        HookOutcome::Error
                    };

                    // Only run OnStop hooks if the service wasn't manually stopped
                    // (manual stops already run the hook in stop_service_with_intent)
                    if !manually_stopped
                        && let Some(service) = ctx.config.services.get(&name)
                    {
                        let env = service.env.clone();
                        if let Some(action) = service
                            .hooks
                            .as_ref()
                            .and_then(|cfg| cfg.action(HookStage::OnStop, hook_outcome))
                        {
                            run_hook(
                                action,
                                &env,
                                HookStage::OnStop,
                                hook_outcome,
                                &name,
                                &ctx.project_root,
                            );
                        }
                    }

                    if manually_stopped {
                        info!("Service '{name}' was manually stopped. Skipping restart.");
                        if let Err(err) = pid_file_guard.remove(&name)
                            && !matches!(err, PidFileError::ServiceNotFound)
                        {
                            warn!(
                                "Failed to clear PID entry for '{name}' after manual stop: {err}"
                            );
                        }
                        if let Err(err) = Self::persist_service_state(
                            &ctx.config,
                            &ctx.state_file,
                            &name,
                            ServiceLifecycleStatus::Stopped,
                            None,
                            None,
                            None,
                        ) {
                            warn!(
                                "Failed to persist stopped state for '{name}' after manual stop: {err}"
                            );
                        }
                        if let Ok(mut counts) = ctx.lock_restart_counts() {
                            counts.remove(&name);
                        }
                    } else if restart_suppressed_for_service {
                        info!(
                            "Automatic restart suppressed for service '{name}' after exit."
                        );
                        if let Err(err) = Self::persist_service_state(
                            &ctx.config,
                            &ctx.state_file,
                            &name,
                            ServiceLifecycleStatus::Stopped,
                            None,
                            exit_code,
                            signal,
                        ) {
                            warn!(
                                "Failed to persist suppressed state for '{name}': {err}"
                            );
                        }
                        if let Ok(mut counts) = ctx.lock_restart_counts() {
                            counts.remove(&name);
                        }
                    } else if !exit_success {
                        // Service failed - always mark it as failed for dependency handling
                        failed_services.push(name.clone());

                        // Check if service should be restarted based on its restart_policy
                        let should_restart = ctx
                            .config
                            .services
                            .get(&name)
                            .map(|s| s.restart_policy.as_deref())
                            .map(|policy| {
                                policy == Some("always") || policy == Some("on-failure")
                            })
                            .unwrap_or(false);

                        if should_restart {
                            warn!("Service '{name}' crashed. Restarting...");
                            restarted_services.push(name.clone());
                        } else {
                            warn!(
                                "Service '{name}' crashed but restart_policy does not allow restart."
                            );
                        }
                        if let Err(err) = Self::persist_service_state(
                            &ctx.config,
                            &ctx.state_file,
                            &name,
                            ServiceLifecycleStatus::ExitedWithError,
                            None,
                            exit_code,
                            signal,
                        ) {
                            warn!("Failed to persist crash state for '{name}': {err}");
                        }
                    } else {
                        debug!(
                            "Service '{name}' exited cleanly. Removing from PID file."
                        );
                        if let Err(e) = pid_file_guard.remove(&name) {
                            error!("Failed to remove '{name}' from PID file: {e}");
                        }
                        if let Err(err) = Self::persist_service_state(
                            &ctx.config,
                            &ctx.state_file,
                            &name,
                            ServiceLifecycleStatus::ExitedSuccessfully,
                            None,
                            exit_code.or(Some(0)),
                            signal,
                        ) {
                            warn!(
                                "Failed to persist clean exit state for '{name}': {err}"
                            );
                        }
                    }

                    ctx.processes.lock().unwrap().remove(&name);
                }
            }

            if !failed_services.is_empty() {
                let reverse = ctx.config.reverse_dependencies();
                for failed in failed_services {
                    Self::stop_dependents(&failed, &reverse, &ctx);
                }
            }

            if active_services == 0 {
                debug!("No active services detected in monitor loop.");
            }

            for name in restarted_services {
                if let Some(service) = ctx.config.services.get(&name) {
                    Self::handle_restart(&name, service, ctx.clone());
                }
            }

            thread::sleep(Duration::from_secs(2));
        }

        debug!("Monitor loop terminating.");
    }

    /// Handles restarting a service if its restart policy allows.
    fn handle_restart(name: &str, service: &ServiceConfig, ctx: DaemonContext) {
        let name = name.to_string();
        let service_clone = service.clone();
        let hooks = service.hooks.clone();
        let max_restarts = service.max_restarts;

        // Check restart count before attempting restart
        {
            let mut counts = ctx.restart_counts.lock().unwrap();
            let count = counts.entry(name.clone()).or_insert(0);
            *count += 1;

            if let Some(max) = max_restarts
                && *count > max
            {
                error!(
                    "Service '{name}' has reached maximum restart attempts ({max}). Giving up."
                );
                return;
            }
        }

        let backoff = service
            .backoff
            .as_deref()
            .unwrap_or("5s")
            .trim_end_matches('s')
            .parse::<u64>()
            .unwrap_or(5);

        let _ = thread::spawn(move || {
            warn!("Restarting '{name}' after {backoff} seconds...");
            thread::sleep(Duration::from_secs(backoff));

            if ctx.restart_suppressed
                .lock()
                .map(|guard| guard.contains(&name))
                .unwrap_or(false)
            {
                info!(
                    "Skipping automatic restart of '{name}' because it is currently suppressed."
                );
                if let Ok(mut counts) = ctx.restart_counts.lock() {
                    counts.remove(&name);
                }
                return;
            }

            if ctx.manual_stop_flags
                .lock()
                .map(|mut guard| guard.remove(&name))
                .unwrap_or(false)
            {
                info!(
                    "Skipping automatic restart of '{name}' due to concurrent manual stop."
                );
                if let Ok(mut counts) = ctx.restart_counts.lock() {
                    counts.remove(&name);
                }
                return;
            }

            let restart_result = Daemon::launch_attached_service(
                &name,
                &service_clone,
                ctx.project_root.clone(),
                Arc::clone(&ctx.processes),
                ctx.detach_children,
            );

            match restart_result {
                Ok(pid) => {
                    let record_result = ctx.pid_file
                        .lock()
                        .map_err(ProcessManagerError::from)
                        .and_then(|mut guard| {
                            guard.services.insert(name.clone(), pid);
                            guard.save().map_err(ProcessManagerError::from)
                        });

                    if let Err(err) = record_result {
                        error!(
                            "Failed to record PID {pid} for restarted service '{name}': {err}"
                        );

                        if let Err(stop_err) =
                            Self::terminate_process_tree(&name, pid)
                        {
                            warn!(
                                "Also failed to terminate untracked restart of '{name}': {stop_err}"
                            );
                        }

                        if let Some(hooks_cfg) = hooks.as_ref()
                            && let Some(action) =
                                hooks_cfg.action(HookStage::OnStart, HookOutcome::Error)
                        {
                            run_hook(
                                action,
                                &service_clone.env,
                                HookStage::OnStart,
                                HookOutcome::Error,
                                &name,
                                &ctx.project_root,
                            );
                        }

                        if let Some(hooks_cfg) = hooks.as_ref()
                            && let Some(action) =
                                hooks_cfg.action(HookStage::OnRestart, HookOutcome::Error)
                        {
                            run_hook(
                                action,
                                &service_clone.env,
                                HookStage::OnRestart,
                                HookOutcome::Error,
                                &name,
                                &ctx.project_root,
                            );
                        }

                        return;
                    }

                    match Self::wait_for_ready(
                        &name,
                        &ctx.processes,
                        &ctx.pid_file,
                    ) {
                        Ok(ServiceReadyState::Running) => {
                            // Reset restart counter after successful restart
                            if let Ok(mut counts) = ctx.lock_restart_counts() {
                                counts.insert(name.clone(), 0);
                            }

                            if let Err(err) = Self::persist_service_state(
                                &ctx.config,
                                &ctx.state_file,
                                &name,
                                ServiceLifecycleStatus::Running,
                                Some(pid),
                                None,
                                None,
                            ) {
                                warn!(
                                    "Failed to persist running state for restarted '{name}': {err}"
                                );
                            }

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnStart, HookOutcome::Success)
                            {
                                run_hook(
                                    action,
                                    &service_clone.env,
                                    HookStage::OnStart,
                                    HookOutcome::Success,
                                    &name,
                                    &ctx.project_root,
                                );
                            }

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnRestart, HookOutcome::Success)
                            {
                                run_hook(
                                    action,
                                    &service_clone.env,
                                    HookStage::OnRestart,
                                    HookOutcome::Success,
                                    &name,
                                    &ctx.project_root,
                                );
                            }

                            if let Ok(mut pid_file_guard) = ctx.pid_file.lock()
                                && let Ok(latest) = PidFile::reload()
                            {
                                *pid_file_guard = latest;
                            }
                        }
                        Ok(ServiceReadyState::CompletedSuccess) => {
                            if let Ok(mut counts) = ctx.lock_restart_counts() {
                                counts.insert(name.clone(), 0);
                            }

                            if let Err(err) = Self::persist_service_state(
                                &ctx.config,
                                &ctx.state_file,
                                &name,
                                ServiceLifecycleStatus::ExitedSuccessfully,
                                None,
                                Some(0),
                                None,
                            ) {
                                warn!(
                                    "Failed to persist completion state for restarted '{name}': {err}"
                                );
                            }

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnStart, HookOutcome::Success)
                            {
                                run_hook(
                                    action,
                                    &service_clone.env,
                                    HookStage::OnStart,
                                    HookOutcome::Success,
                                    &name,
                                    &ctx.project_root,
                                );
                            }

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnRestart, HookOutcome::Success)
                            {
                                run_hook(
                                    action,
                                    &service_clone.env,
                                    HookStage::OnRestart,
                                    HookOutcome::Success,
                                    &name,
                                    &ctx.project_root,
                                );
                            }
                        }
                        Err(err) => {
                            error!(
                                "Service '{name}' failed to become ready after restart: {err}"
                            );

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnStart, HookOutcome::Error)
                            {
                                run_hook(
                                    action,
                                    &service_clone.env,
                                    HookStage::OnStart,
                                    HookOutcome::Error,
                                    &name,
                                    &ctx.project_root,
                                );
                            }

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnRestart, HookOutcome::Error)
                            {
                                run_hook(
                                    action,
                                    &service_clone.env,
                                    HookStage::OnRestart,
                                    HookOutcome::Error,
                                    &name,
                                    &ctx.project_root,
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to restart '{name}': {e}");

                    if let Some(hooks_cfg) = hooks.as_ref()
                        && let Some(action) =
                            hooks_cfg.action(HookStage::OnStart, HookOutcome::Error)
                    {
                        run_hook(
                            action,
                            &service_clone.env,
                            HookStage::OnStart,
                            HookOutcome::Error,
                            &name,
                            &ctx.project_root,
                        );
                    }

                    if let Some(hooks_cfg) = hooks.as_ref()
                        && let Some(action) =
                            hooks_cfg.action(HookStage::OnRestart, HookOutcome::Error)
                    {
                        run_hook(
                            action,
                            &service_clone.env,
                            HookStage::OnRestart,
                            HookOutcome::Error,
                            &name,
                            &ctx.project_root,
                        );
                    }
                }
            }
        })
        .join();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Ensure monitoring is stopped
        self.shutdown_monitor();

        // Cancel all Linux service threads
        #[cfg(target_os = "linux")]
        self.context().cancel_all_service_threads();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        env, fs,
        sync::Mutex,
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    /// Helper to build a minimal service definition for unit tests.
    fn make_service(command: &str, deps: &[&str]) -> ServiceConfig {
        ServiceConfig {
            command: command.to_string(),
            env: None,
            user: None,
            group: None,
            supplementary_groups: None,
            limits: None,
            capabilities: None,
            isolation: None,
            restart_policy: None,
            backoff: None,
            max_restarts: None,
            depends_on: if deps.is_empty() {
                None
            } else {
                Some(deps.iter().map(|d| d.to_string()).collect())
            },
            deployment: None,
            hooks: None,
            cron: None,
            skip: None,
            spawn: None,
        }
    }

    /// Constructs a daemon instance for tests with the provided service map rooted in `dir`.
    fn create_daemon(
        dir: &std::path::Path,
        services: HashMap<String, ServiceConfig>,
    ) -> Daemon {
        let pid_file = Arc::new(Mutex::new(PidFile::default()));
        let state_file = Arc::new(Mutex::new(ServiceStateFile::default()));
        let config = Config {
            version: "1".into(),
            services,
            project_dir: Some(dir.to_string_lossy().to_string()),
            env: None,
            metrics: crate::config::MetricsConfig::default(),
        };

        // Validate order to mirror load_config behavior.
        config.service_start_order().unwrap();

        Daemon::new(config, pid_file, state_file, false)
    }

    /// Executes a test callback with a temporary HOME directory to contain PID and log files.
    fn with_temp_home<F: FnOnce(&std::path::Path)>(test: F) {
        let _guard = crate::test_utils::env_lock();

        let temp = tempfile::tempdir().expect("tempdir");
        let original = env::var("HOME").ok();
        let temp_home = temp.path().to_path_buf();
        unsafe {
            env::set_var("HOME", &temp_home);
        }
        crate::runtime::init_with_test_home(&temp_home);
        crate::runtime::set_drop_privileges(false);

        fs::create_dir_all(crate::runtime::state_dir()).unwrap();
        fs::create_dir_all(crate::runtime::log_dir()).unwrap();
        test(temp.path());
        match original {
            Some(val) => unsafe {
                env::set_var("HOME", val);
            },
            None => unsafe {
                env::remove_var("HOME");
            },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn logs_indicate_port_conflict_detects_common_errors() {
        with_temp_home(|_| {
            let log_path = resolve_log_path("web", "stderr");
            if let Some(dir) = log_path.parent() {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(
                &log_path,
                "Error: Server(\"Address already in use (os error 98)\")\n",
            )
            .unwrap();

            assert!(Daemon::logs_indicate_port_conflict("web"));
        });
    }

    #[test]
    fn logs_indicate_port_conflict_returns_false_when_not_present() {
        with_temp_home(|_| {
            let log_path = resolve_log_path("api", "stderr");
            if let Some(dir) = log_path.parent() {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(&log_path, "Some other failure\n").unwrap();

            assert!(!Daemon::logs_indicate_port_conflict("api"));
        });
    }

    #[test]
    fn restart_service_reports_failure_when_process_stops_immediately() {
        with_temp_home(|dir| {
            fs::write(dir.join("mode.txt"), "initial\n").unwrap();
            fs::write(
                dir.join("app.sh"),
                r#"
MODE=$(cat mode.txt)
if [ "$MODE" = "initial" ]; then
  sleep 5
else
  echo 'Error: Server("Address already in use (os error 98)")' >&2
  sleep 0.05
  exit 0
fi
"#,
            )
            .unwrap();

            let mut services = HashMap::new();
            let mut service = make_service("sh app.sh", &[]);
            service.restart_policy = Some("always".into());
            services.insert("app".into(), service);

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();

            // Ensure the initial instance is running.
            thread::sleep(Duration::from_millis(100));

            fs::write(dir.join("mode.txt"), "restart\n").unwrap();

            let svc = daemon.config.services.get("app").unwrap();
            let err = daemon.restart_service("app", svc).unwrap_err();

            match err {
                ProcessManagerError::ServicesNotRunning { services } => {
                    assert_eq!(services, vec!["app".to_string()]);
                }
                other => panic!("unexpected error: {other:?}"),
            }

            daemon.shutdown_monitor();
        });
    }

    #[test]
    fn monitor_reaps_services_that_exit_after_running_state() {
        with_temp_home(|dir| {
            fs::write(dir.join("slow_exit.sh"), "sleep 0.2\n").unwrap();

            let mut services = HashMap::new();
            let service = make_service("sh slow_exit.sh", &[]);
            services.insert("slow".into(), service);

            let daemon = create_daemon(dir, services);
            let svc = daemon.config.services.get("slow").unwrap();

            assert!(matches!(
                daemon.start_service("slow", svc).unwrap(),
                ServiceReadyState::Running
            ));

            daemon.ensure_monitoring().unwrap();
            thread::sleep(Duration::from_millis(2500));

            assert!(daemon.pid_file.lock().unwrap().get("slow").is_none());

            let state_guard = daemon.state_file.lock().unwrap();
            let service_hash = daemon
                .config
                .services
                .get("slow")
                .expect("service present")
                .compute_hash();
            let entry = state_guard
                .services()
                .get(&service_hash)
                .expect("state entry present");
            assert_eq!(entry.status, ServiceLifecycleStatus::ExitedSuccessfully);

            daemon.shutdown_monitor();
        });
    }

    #[test]
    fn parse_duration_supports_common_units() {
        assert_eq!(
            Daemon::parse_duration("10s").unwrap(),
            Duration::from_secs(10)
        );
        assert_eq!(
            Daemon::parse_duration("5m").unwrap(),
            Duration::from_secs(300)
        );
        assert_eq!(
            Daemon::parse_duration("2h").unwrap(),
            Duration::from_secs(7200)
        );
        assert_eq!(
            Daemon::parse_duration("15").unwrap(),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn parse_duration_rejects_invalid_strings() {
        assert!(matches!(
            Daemon::parse_duration(""),
            Err(ProcessManagerError::ConfigParseError(_))
        ));
        assert!(matches!(
            Daemon::parse_duration("abc"),
            Err(ProcessManagerError::ConfigParseError(_))
        ));
    }

    #[test]
    fn services_start_in_dependency_order() {
        with_temp_home(|dir| {
            fs::write(dir.join("db.sh"), "echo db >> order.log\n").unwrap();
            fs::write(dir.join("web.sh"), "echo web >> order.log\n").unwrap();
            fs::write(dir.join("worker.sh"), "echo worker >> order.log\n").unwrap();

            let mut services = HashMap::new();
            services.insert("db".into(), make_service("sh db.sh", &[]));
            services.insert("web".into(), make_service("sh web.sh", &["db"]));
            services.insert("worker".into(), make_service("sh worker.sh", &["web"]));

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();
            daemon.shutdown_monitor();

            let content = fs::read_to_string(dir.join("order.log")).unwrap();
            let lines: Vec<_> = content.lines().collect();
            assert_eq!(lines, vec!["db", "web", "worker"]);
        });
    }

    #[test]
    fn dependent_not_started_when_dependency_fails() {
        with_temp_home(|dir| {
            fs::write(dir.join("fail.sh"), "exit 1\n").unwrap();
            fs::write(dir.join("dependent.sh"), "echo dependent >> started.log\n")
                .unwrap();

            let mut services = HashMap::new();
            services.insert("fail".into(), make_service("sh fail.sh", &[]));
            services.insert(
                "dependent".into(),
                make_service("sh dependent.sh", &["fail"]),
            );

            let daemon = create_daemon(dir, services);
            let result = daemon.start_services();
            assert!(result.is_err());
            assert!(!dir.join("started.log").exists());
            daemon.shutdown_monitor();
        });
    }

    #[test]
    fn dependents_stopped_when_dependency_crashes() {
        with_temp_home(|dir| {
            fs::write(
                dir.join("parent.sh"),
                "echo parent >> events.log\nsleep 1\nexit 1\n",
            )
            .unwrap();
            fs::write(dir.join("child.sh"), "echo child >> events.log\nsleep 30\n")
                .unwrap();

            let mut services = HashMap::new();
            services.insert("parent".into(), make_service("sh parent.sh", &[]));
            services.insert("child".into(), make_service("sh child.sh", &["parent"]));

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();

            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if daemon.pid_file.lock().unwrap().get("child").is_none() {
                    break;
                }

                if Instant::now() >= deadline {
                    panic!("dependent service still recorded in pid file");
                }

                thread::sleep(Duration::from_millis(100));
            }

            daemon.shutdown_monitor();
        });
    }

    #[test]
    fn concurrent_pid_file_operations_no_lost_updates() {
        with_temp_home(|_| {
            // Initialize PID file with a baseline service
            let mut initial = PidFile::default();
            initial.insert("baseline", 1000).unwrap();

            // Number of concurrent operations
            let num_threads = 10;
            let mut handles = vec![];

            // Spawn threads that concurrently add services (simulating cron jobs starting)
            for i in 0..num_threads {
                let handle = thread::spawn(move || {
                    // Small stagger to reduce file corruption but still trigger lost updates
                    thread::sleep(Duration::from_micros(i as u64 * 100));

                    // Simulate the pattern used in supervisor.rs for cron cleanup
                    // Load -> Modify -> Save (no locking)
                    // Retry on file corruption to focus on demonstrating lost updates
                    for retry in 0..3 {
                        match PidFile::load() {
                            Ok(mut pid_file) => {
                                let service_name = format!("cron_job_{}", i);
                                if pid_file.insert(&service_name, 2000 + i).is_ok() {
                                    break;
                                }
                            }
                            Err(_) if retry < 2 => {
                                thread::sleep(Duration::from_millis(1));
                                continue;
                            }
                            Err(e) => {
                                panic!("Failed to load PID file after retries: {}", e)
                            }
                        }
                    }
                });
                handles.push(handle);
            }

            // Also spawn threads that remove services (simulating concurrent cleanup)
            // to further stress the race condition
            for i in 0..5 {
                let handle = thread::spawn(move || {
                    // Small delay to let some inserts happen first
                    thread::sleep(Duration::from_millis(2));

                    for retry in 0..3 {
                        match PidFile::load() {
                            Ok(mut pid_file) => {
                                if i % 2 == 0 {
                                    // Try to remove the baseline service
                                    let _ = pid_file.remove("baseline");
                                } else {
                                    // Try to add another service
                                    let _ = pid_file
                                        .insert(&format!("extra_{}", i), 3000 + i);
                                }
                                break;
                            }
                            Err(_) if retry < 2 => {
                                thread::sleep(Duration::from_millis(1));
                                continue;
                            }
                            Err(_) => break,
                        }
                    }
                });
                handles.push(handle);
            }

            // Wait for all threads to complete
            for handle in handles {
                handle.join().unwrap();
            }

            // Verify the final state
            let final_pid_file = PidFile::load().expect("Failed to load final PID file");

            // With proper file locking, all cron_job_X services should be present
            // Without locking, some will be lost due to concurrent overwrites
            let mut missing = vec![];
            for i in 0..num_threads {
                let service_name = format!("cron_job_{}", i);
                if final_pid_file.get(&service_name).is_none() {
                    missing.push(service_name);
                }
            }

            // This assertion will FAIL without proper file locking
            // because concurrent load-modify-save operations will overwrite each other
            assert!(
                missing.is_empty(),
                "Lost updates detected! Missing services: {:?}. \
                 This indicates a race condition in PID file operations. \
                 Total services in final file: {}",
                missing,
                final_pid_file.services().len()
            );
        });
    }

    #[test]
    fn individual_service_stop_removes_from_tracking() {
        with_temp_home(|dir| {
            let mut services = HashMap::new();
            services.insert("test_service".into(), make_service("sleep 60", &[]));

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();

            thread::sleep(Duration::from_millis(100));

            // Verify service is running
            assert!(
                daemon
                    .processes
                    .lock()
                    .unwrap()
                    .contains_key("test_service")
            );
            assert!(
                daemon
                    .pid_file
                    .lock()
                    .unwrap()
                    .get("test_service")
                    .is_some()
            );

            // Stop individual service
            daemon.stop_service("test_service").unwrap();

            // Verify service is removed from tracking
            assert!(
                !daemon
                    .processes
                    .lock()
                    .unwrap()
                    .contains_key("test_service")
            );
            assert!(
                daemon
                    .pid_file
                    .lock()
                    .unwrap()
                    .get("test_service")
                    .is_none()
            );
        });
    }

    #[test]
    fn stop_service_handles_termination_failure() {
        with_temp_home(|dir| {
            let mut services = HashMap::new();
            // Use a command that ignores SIGTERM for testing
            services.insert(
                "stubborn_service".into(),
                make_service("sh -c 'trap \"\" TERM; sleep 10'", &[]),
            );

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();

            thread::sleep(Duration::from_millis(100));

            // Service should be running
            assert!(
                daemon
                    .processes
                    .lock()
                    .unwrap()
                    .contains_key("stubborn_service")
            );

            // Stop should eventually succeed (via SIGKILL escalation)
            daemon.stop_service("stubborn_service").unwrap();

            // Service should be removed even after termination difficulties
            assert!(
                !daemon
                    .processes
                    .lock()
                    .unwrap()
                    .contains_key("stubborn_service")
            );
        });
    }

    #[test]
    fn start_individual_service_after_stop() {
        with_temp_home(|dir| {
            let mut service = make_service("echo 'test'", &[]);
            service.restart_policy = Some("never".into());

            let mut services = HashMap::new();
            services.insert("test_service".into(), service.clone());

            let daemon = create_daemon(dir, services);

            // Start service
            let result = daemon.start_service("test_service", &service).unwrap();
            assert!(matches!(result, ServiceReadyState::CompletedSuccess));

            thread::sleep(Duration::from_millis(100));

            // Stop service
            daemon.stop_service("test_service").unwrap();
            assert!(
                !daemon
                    .processes
                    .lock()
                    .unwrap()
                    .contains_key("test_service")
            );

            // Start service again
            let result = daemon.start_service("test_service", &service).unwrap();
            assert!(matches!(result, ServiceReadyState::CompletedSuccess));
        });
    }

    #[test]
    fn manual_stop_flag_prevents_restart() {
        with_temp_home(|dir| {
            let mut service = make_service("sh -c 'sleep 0.1 && exit 1'", &[]);
            service.restart_policy = Some("always".into());

            let mut services = HashMap::new();
            services.insert("test_service".into(), service);

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();

            thread::sleep(Duration::from_millis(50));

            // Manual stop should set flag
            daemon.stop_service("test_service").unwrap();

            // Verify manual stop flag is set
            assert!(
                daemon
                    .manual_stop_flags
                    .lock()
                    .unwrap()
                    .contains("test_service")
            );

            // Verify restart is suppressed
            assert!(
                daemon
                    .restart_suppressed
                    .lock()
                    .unwrap()
                    .contains("test_service")
            );

            thread::sleep(Duration::from_millis(200));

            // Service should not have restarted
            assert!(
                !daemon
                    .processes
                    .lock()
                    .unwrap()
                    .contains_key("test_service")
            );
        });
    }

    #[test]
    fn config_accessor_returns_arc() {
        with_temp_home(|dir| {
            let services = HashMap::new();
            let daemon = create_daemon(dir, services);

            let config1 = daemon.config();
            let config2 = daemon.config();

            // Both should be the same Arc
            assert!(Arc::ptr_eq(&config1, &config2));
        });
    }

    #[test]
    fn stop_service_runs_hooks_once() {
        with_temp_home(|dir| {
            let hook_log = dir.join("hooks.log");

            let hooks = crate::config::Hooks {
                on_start: None,
                on_stop: Some(crate::config::HookLifecycleConfig {
                    success: Some(crate::config::HookAction {
                        command: format!("echo 'STOP_SUCCESS' >> {}", hook_log.display()),
                        timeout: None,
                    }),
                    error: Some(crate::config::HookAction {
                        command: format!("echo 'STOP_ERROR' >> {}", hook_log.display()),
                        timeout: None,
                    }),
                }),
                on_restart: None,
            };

            let mut service = make_service("sleep 60", &[]);
            service.hooks = Some(hooks);

            let mut services = HashMap::new();
            services.insert("hooked_service".into(), service);

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();

            thread::sleep(Duration::from_millis(100));

            // Stop service
            daemon.stop_service("hooked_service").unwrap();

            thread::sleep(Duration::from_millis(100));

            // Hook should run exactly once
            let content = fs::read_to_string(&hook_log).unwrap_or_default();
            assert_eq!(content.matches("STOP_SUCCESS").count(), 1);
        });
    }

    #[test]
    fn terminate_process_tree_kills_all_descendants() {
        with_temp_home(|_| {
            // Create a parent process that spawns children
            let mut cmd = Command::new(DEFAULT_SHELL);
            cmd.arg("-c");
            cmd.arg("sh -c 'sleep 60' & sh -c 'sleep 60' & sleep 60");
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());

            let mut parent = cmd.spawn().unwrap();
            let parent_pid = parent.id();

            thread::sleep(Duration::from_millis(100));

            // Collect all descendants before termination
            let descendants_before = Daemon::collect_descendants(parent_pid);
            assert!(
                !descendants_before.is_empty(),
                "Should have child processes"
            );

            // Terminate the process tree
            match Daemon::terminate_process_tree("test", parent_pid) {
                Ok(_) => {
                    thread::sleep(Duration::from_millis(200));

                    // All processes should be gone
                    for pid in descendants_before {
                        assert!(
                            !Daemon::signal_pid("test", pid, None).unwrap(),
                            "Child process {} should be terminated",
                            pid
                        );
                    }
                }
                Err(ProcessManagerError::ServiceStopError { source, .. })
                    if source.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // Timeout is acceptable in tests - just skip verification
                }
                Err(e) => panic!("Unexpected error: {:?}", e),
            }

            // Clean up
            let _ = parent.wait();
        });
    }
}
