//! Service management daemon.
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File},
    io::{BufRead, BufReader, ErrorKind, Read},
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
use quick_xml::{de::from_str as xml_from_str, se::to_string as xml_to_string};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize, de::Error as _};
use serde_yaml;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tracing::{debug, error, info, trace, warn};

use crate::{
    config::{
        BlueGreenDeploymentConfig, Config, DependsOnCondition, EffectiveLogsConfig,
        EnvConfig, HealthCheckConfig, HookAction, HookOutcome, HookStage, LogSink,
        ServiceConfig, SkipConfig,
    },
    constants::{
        DEFAULT_SERVICE_PATH, DEFAULT_SHELL, DaemonLock, DeploymentStrategy,
        MAX_STATUS_LOG_LINES, POST_RESTART_VERIFY_ATTEMPTS, POST_RESTART_VERIFY_DELAY,
        PROCESS_CHECK_INTERVAL, PROCESS_READY_CHECKS, SERVICE_POLL_INTERVAL,
        SERVICE_START_TIMEOUT, SESSION_SCOPED_ENV_VARS, SHELL_COMMAND_FLAG,
    },
    error::{PidFileError, ProcessManagerError, ServiceStateError},
    logs::{resolve_log_path, spawn_service_log_writers},
    opslot::OpSlot,
    runtime,
    spawn::SpawnedExit,
    state_store::StateStore,
};

/// Provides systemtime serde support.
mod systemtime_serde {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Deserializer, Serializer};

    /// Serializes this item.
    pub fn serialize<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let duration = time
            .duration_since(UNIX_EPOCH)
            .map_err(serde::ser::Error::custom)?;
        serializer.serialize_u64(duration.as_secs())
    }

    /// Handles deserialize.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<SystemTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(UNIX_EPOCH + Duration::from_secs(secs))
    }
}

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

/// Wrapper for service entries to make them XML-safe
#[derive(Debug, Serialize, Deserialize, Clone)]
struct ServiceEntry {
    name: String,
    pid: u32,
}

/// Wrapper for group entries
#[derive(Debug, Serialize, Deserialize, Clone)]
struct GroupEntry {
    name: String,
    pgid: i32,
}

/// Wrapper for parent map entries
#[derive(Debug, Serialize, Deserialize, Clone)]
struct ParentMapEntry {
    child: u32,
    parent: u32,
}

/// Wrapper for children map entries
#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChildrenMapEntry {
    parent: u32,
    children: Vec<u32>,
}

/// Wrapper for depth entries
#[derive(Debug, Serialize, Deserialize, Clone)]
struct DepthEntry {
    pid: u32,
    depth: usize,
}

/// Wrapper for metadata entries
#[derive(Debug, Serialize, Deserialize, Clone)]
struct MetadataEntry {
    pid: u32,
    metadata: PersistedSpawnChild,
}

/// PID tracking file.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct PidFile {
    /// Service name -> PID map.
    #[serde(default, rename = "services")]
    #[serde(
        serialize_with = "serialize_services",
        deserialize_with = "deserialize_services"
    )]
    services: HashMap<String, u32>,
    /// Service name -> process group ID.
    #[serde(default, rename = "service_groups")]
    #[serde(
        serialize_with = "serialize_groups",
        deserialize_with = "deserialize_groups"
    )]
    service_groups: HashMap<String, i32>,
    /// Child PID -> Parent PID mapping for spawned processes.
    #[serde(default, rename = "parent_map")]
    #[serde(
        serialize_with = "serialize_parent_map",
        deserialize_with = "deserialize_parent_map"
    )]
    parent_map: HashMap<u32, u32>,
    /// Parent PID -> list of Child PIDs for reverse lookup.
    #[serde(default, rename = "children_map")]
    #[serde(
        serialize_with = "serialize_children_map",
        deserialize_with = "deserialize_children_map"
    )]
    children_map: HashMap<u32, Vec<u32>>,
    /// PID -> spawn depth in the tree (0 = root service).
    #[serde(default, rename = "spawn_depth")]
    #[serde(
        serialize_with = "serialize_depth",
        deserialize_with = "deserialize_depth"
    )]
    spawn_depth: HashMap<u32, usize>,
    /// Additional metadata for spawned children.
    #[serde(default, rename = "spawn_metadata")]
    #[serde(
        serialize_with = "serialize_metadata",
        deserialize_with = "deserialize_metadata"
    )]
    spawn_metadata: HashMap<u32, PersistedSpawnChild>,
    /// The project state directory this file is bound to. Never serialized;
    /// re-attached after every load/reload.
    #[serde(skip)]
    store: StateStore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Represents persisted spawn child.
pub(crate) struct PersistedSpawnChild {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) command: String,
    #[serde(with = "systemtime_serde")]
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

/// Serializes services.
fn serialize_services<S>(map: &HashMap<String, u32>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(map.len()))?;
    for (k, v) in map {
        seq.serialize_element(&ServiceEntry {
            name: k.clone(),
            pid: *v,
        })?;
    }
    seq.end()
}

/// Handles deserialize services.
fn deserialize_services<'de, D>(d: D) -> Result<HashMap<String, u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries: Vec<ServiceEntry> = Vec::deserialize(d)?;
    Ok(entries.into_iter().map(|e| (e.name, e.pid)).collect())
}

/// Serializes groups.
fn serialize_groups<S>(map: &HashMap<String, i32>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(map.len()))?;
    for (k, v) in map {
        seq.serialize_element(&GroupEntry {
            name: k.clone(),
            pgid: *v,
        })?;
    }
    seq.end()
}

/// Handles deserialize groups.
fn deserialize_groups<'de, D>(d: D) -> Result<HashMap<String, i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries: Vec<GroupEntry> = Vec::deserialize(d)?;
    Ok(entries.into_iter().map(|e| (e.name, e.pgid)).collect())
}

/// Serializes parent map.
fn serialize_parent_map<S>(map: &HashMap<u32, u32>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(map.len()))?;
    for (k, v) in map {
        seq.serialize_element(&ParentMapEntry {
            child: *k,
            parent: *v,
        })?;
    }
    seq.end()
}

/// Handles deserialize parent map.
fn deserialize_parent_map<'de, D>(d: D) -> Result<HashMap<u32, u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries: Vec<ParentMapEntry> = Vec::deserialize(d)?;
    Ok(entries.into_iter().map(|e| (e.child, e.parent)).collect())
}

/// Serializes children map.
fn serialize_children_map<S>(
    map: &HashMap<u32, Vec<u32>>,
    s: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(map.len()))?;
    for (k, v) in map {
        seq.serialize_element(&ChildrenMapEntry {
            parent: *k,
            children: v.clone(),
        })?;
    }
    seq.end()
}

/// Handles deserialize children map.
fn deserialize_children_map<'de, D>(d: D) -> Result<HashMap<u32, Vec<u32>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries: Vec<ChildrenMapEntry> = Vec::deserialize(d)?;
    Ok(entries
        .into_iter()
        .map(|e| (e.parent, e.children))
        .collect())
}

/// Serializes depth.
fn serialize_depth<S>(map: &HashMap<u32, usize>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(map.len()))?;
    for (k, v) in map {
        seq.serialize_element(&DepthEntry { pid: *k, depth: *v })?;
    }
    seq.end()
}

/// Handles deserialize depth.
fn deserialize_depth<'de, D>(d: D) -> Result<HashMap<u32, usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries: Vec<DepthEntry> = Vec::deserialize(d)?;
    Ok(entries.into_iter().map(|e| (e.pid, e.depth)).collect())
}

/// Serializes metadata.
fn serialize_metadata<S>(
    map: &HashMap<u32, PersistedSpawnChild>,
    s: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(map.len()))?;
    for (k, v) in map {
        seq.serialize_element(&MetadataEntry {
            pid: *k,
            metadata: v.clone(),
        })?;
    }
    seq.end()
}

/// Handles deserialize metadata.
fn deserialize_metadata<'de, D>(
    d: D,
) -> Result<HashMap<u32, PersistedSpawnChild>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries: Vec<MetadataEntry> = Vec::deserialize(d)?;
    Ok(entries.into_iter().map(|e| (e.pid, e.metadata)).collect())
}

impl PidFile {
    /// Handles path.
    fn path(&self) -> PathBuf {
        self.store.pid_path()
    }

    /// Handles lock path.
    fn lock_path(&self) -> PathBuf {
        self.store.pid_lock_path()
    }

    /// Gets exclusive lock (auto-releases on drop).
    fn acquire_lock(&self) -> Result<File, PidFileError> {
        let lock_path = self.lock_path();
        runtime::create_private_dir(lock_path.parent().unwrap())?;

        let lock_file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        lock_file.lock_exclusive()?;

        Ok(lock_file)
    }

    /// Re-reads the on-disk file into `self`, preserving the bound store.
    ///
    /// Deserialize cannot know which project this file belongs to, so the
    /// store is re-attached after every reload — this is the single invariant
    /// that keeps a project's handle pinned to its own directory.
    fn reload_into(&mut self, path: &std::path::Path) -> Result<(), PidFileError> {
        if path.exists() {
            let contents = fs::read_to_string(path)?;
            let store = self.store.clone();
            *self = xml_from_str::<Self>(&contents)?;
            self.store = store;
        }
        Ok(())
    }

    /// Writes `self` to `path`, creating the project directory if needed.
    fn write_at(&self, path: &std::path::Path) -> Result<(), PidFileError> {
        runtime::create_private_dir(path.parent().unwrap())?;
        runtime::write_private_file(path, xml_to_string(self)?)?;
        Ok(())
    }

    /// Returns a reference to the services map.
    pub fn services(&self) -> &HashMap<String, u32> {
        &self.services
    }

    /// The project store this file is bound to.
    pub fn store(&self) -> StateStore {
        self.store.clone()
    }

    /// Binds this file to a project store.
    pub fn set_store(&mut self, store: StateStore) {
        self.store = store;
    }

    /// Loads with file locking.
    pub fn load(store: StateStore) -> Result<Self, PidFileError> {
        let mut this = Self {
            store,
            ..Self::default()
        };
        let _lock = this.acquire_lock()?;

        let path = this.path();
        if !path.exists() {
            return Ok(this);
        }
        let contents = fs::read_to_string(path)?;
        let bound = this.store.clone();
        this = xml_from_str::<Self>(&contents)?;
        this.store = bound;
        Ok(this)
    }

    /// Returns the PID for a specific service.
    pub fn pid_for(&self, service: &str) -> Option<u32> {
        self.services.get(service).copied()
    }

    /// Returns the process group ID for a specific service.
    pub fn pgid_for(&self, service: &str) -> Option<i32> {
        self.service_groups.get(service).copied()
    }

    /// Returns the service-name to process-group map for all recorded services.
    pub fn service_pgids(&self) -> &HashMap<String, i32> {
        &self.service_groups
    }

    /// Records a service PID in memory only, without persisting to disk.
    /// Intended for constructing snapshots in tests.
    pub fn insert_in_memory(&mut self, service: &str, pid: u32) {
        self.services.insert(service.to_string(), pid);
    }

    /// Records spawn metadata in memory only, without persisting to disk.
    /// Intended for constructing snapshots in tests.
    #[cfg(test)]
    pub(crate) fn record_spawn_in_memory(&mut self, metadata: PersistedSpawnChild) {
        let child_pid = metadata.pid;
        let parent_pid = metadata.parent_pid;
        let depth = metadata.depth;
        self.parent_map.insert(child_pid, parent_pid);
        self.children_map
            .entry(parent_pid)
            .or_default()
            .push(child_pid);
        self.spawn_depth.insert(child_pid, depth);
        self.spawn_metadata.insert(child_pid, metadata);
    }

    /// Reloads from disk. A missing file is treated as empty state.
    pub fn reload(store: StateStore) -> Result<Self, PidFileError> {
        let mut this = Self {
            store,
            ..Self::default()
        };
        let _lock = this.acquire_lock()?;

        let path = this.path();
        if !path.exists() {
            return Ok(this);
        }
        let contents = fs::read_to_string(&path)?;
        let bound = this.store.clone();
        this = xml_from_str::<Self>(&contents)?;
        this.store = bound;
        Ok(this)
    }

    /// Saves to disk.
    pub fn save(&self) -> Result<(), PidFileError> {
        let _lock = self.acquire_lock()?;
        let path = self.path();
        self.write_at(&path)
    }

    /// Atomically inserts PID.
    pub fn insert(&mut self, service: &str, pid: u32) -> Result<(), PidFileError> {
        self.insert_with_group(service, pid, None)
    }

    /// Atomically inserts PID and optional process group ID.
    pub fn insert_with_group(
        &mut self,
        service: &str,
        pid: u32,
        pgid: Option<i32>,
    ) -> Result<(), PidFileError> {
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

        self.services.insert(service.to_string(), pid);
        if let Some(group) = pgid {
            self.service_groups.insert(service.to_string(), group);
        }

        self.write_at(&path)
    }

    /// Atomically clears a service PID while preserving the last known process-group metadata.
    pub fn clear_pid(&mut self, service: &str) -> Result<(), PidFileError> {
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

        if self.services.remove(service).is_none() {
            return Err(PidFileError::ServiceNotFound);
        }

        self.write_at(&path)
    }

    /// Atomically removes service.
    pub fn remove(&mut self, service: &str) -> Result<(), PidFileError> {
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

        let removed_pid = match self.services.remove(service) {
            Some(pid) => pid,
            None => return Err(PidFileError::ServiceNotFound),
        };

        if self.parent_map.contains_key(&removed_pid)
            || self.children_map.contains_key(&removed_pid)
            || self.spawn_metadata.contains_key(&removed_pid)
        {
            self.remove_spawn_subtree_in_memory(removed_pid);
        }

        if let Some(children) = self.children_map.remove(&removed_pid) {
            for child in children {
                self.remove_spawn_subtree_in_memory(child);
            }
        }

        let stale_roots: Vec<u32> = self
            .spawn_metadata
            .values()
            .filter(|meta| meta.parent_pid == removed_pid)
            .map(|meta| meta.pid)
            .collect();
        for stale_pid in stale_roots {
            self.remove_spawn_subtree_in_memory(stale_pid);
        }

        if self.services.is_empty() {
            self.parent_map.clear();
            self.children_map.clear();
            self.spawn_depth.clear();
            self.spawn_metadata.clear();
        }

        let _ = self.service_groups.remove(service);

        self.write_at(&path)
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
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

        let child_pid = metadata.pid;
        let parent_pid = metadata.parent_pid;
        let depth = metadata.depth;

        self.parent_map.insert(child_pid, parent_pid);
        self.children_map
            .entry(parent_pid)
            .or_default()
            .push(child_pid);
        self.spawn_depth.insert(child_pid, depth);
        self.spawn_metadata.insert(child_pid, metadata);

        self.write_at(&path)
    }

    /// Records spawn exit.
    pub(crate) fn record_spawn_exit(
        &mut self,
        child_pid: u32,
        exit: SpawnedExit,
    ) -> Result<(), PidFileError> {
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

        if let Some(metadata) = self.spawn_metadata.get_mut(&child_pid) {
            metadata.last_exit = Some(exit.clone());
        }

        self.write_at(&path)
    }

    /// Atomically removes a spawned child process.
    pub fn remove_spawn(&mut self, child_pid: u32) -> Result<(), PidFileError> {
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

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

        self.write_at(&path)
    }

    /// Removes spawn subtree.
    pub(crate) fn remove_spawn_subtree(
        &mut self,
        root_pid: u32,
    ) -> Result<Vec<u32>, PidFileError> {
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

        let removed = self.remove_spawn_subtree_in_memory(root_pid);

        self.write_at(&path)?;

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

    /// Returns spawn metadata.
    pub(crate) fn get_spawn_metadata(&self, pid: u32) -> Option<&PersistedSpawnChild> {
        self.spawn_metadata.get(&pid)
    }

    /// Handles spawn children for parent.
    pub(crate) fn spawn_children_for_parent(
        &self,
        parent_pid: u32,
    ) -> Vec<&PersistedSpawnChild> {
        self.spawn_metadata
            .values()
            .filter(|meta| meta.parent_pid == parent_pid)
            .collect()
    }

    /// Handles spawn roots for service.
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
    use std::{collections::HashMap, env, fs, time::SystemTime};

    use tempfile::tempdir;

    use super::*;

    #[test]
    /// Removes spawn subtree in memory prunes all descendants.
    fn remove_spawn_subtree_in_memory_prunes_all_descendants() {
        let mut pid_file = PidFile {
            services: HashMap::new(),
            service_groups: HashMap::new(),
            parent_map: HashMap::from([(2, 1), (3, 2)]),
            children_map: HashMap::from([(1, vec![2]), (2, vec![3])]),
            spawn_depth: HashMap::from([(1, 0), (2, 1), (3, 2)]),
            store: StateStore::for_project("test"),
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

    #[test]
    /// Removes service prunes spawn metadata for service pid.
    fn remove_service_prunes_spawn_metadata_for_service_pid() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home directory");

        let original_home = env::var("HOME").ok();
        unsafe {
            env::set_var("HOME", &home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let store = StateStore::for_project("test");
        let pid_file = PidFile {
            store: store.clone(),
            services: HashMap::from([("svc".to_string(), 10)]),
            service_groups: HashMap::from([("svc".to_string(), 10)]),
            parent_map: HashMap::from([(11, 10), (12, 11)]),
            children_map: HashMap::from([(10, vec![11]), (11, vec![12])]),
            spawn_depth: HashMap::from([(11, 1), (12, 2)]),
            spawn_metadata: HashMap::from([
                (
                    11,
                    PersistedSpawnChild {
                        pid: 11,
                        name: "child".into(),
                        command: "cmd".into(),
                        started_at: SystemTime::now(),
                        ttl_secs: None,
                        depth: 1,
                        parent_pid: 10,
                        service_hash: None,
                        cpu_percent: None,
                        rss_bytes: None,
                        last_exit: None,
                    },
                ),
                (
                    12,
                    PersistedSpawnChild {
                        pid: 12,
                        name: "grandchild".into(),
                        command: "cmd".into(),
                        started_at: SystemTime::now(),
                        ttl_secs: None,
                        depth: 2,
                        parent_pid: 11,
                        service_hash: None,
                        cpu_percent: None,
                        rss_bytes: None,
                        last_exit: None,
                    },
                ),
            ]),
        };

        pid_file.save().expect("save pid file");
        let mut loaded = PidFile::load(store).expect("load pid file");
        let removed = loaded.remove("svc");
        assert!(removed.is_ok());
        assert!(!loaded.services.contains_key("svc"));
        assert!(!loaded.service_groups.contains_key("svc"));
        assert!(!loaded.parent_map.contains_key(&11));
        assert!(!loaded.parent_map.contains_key(&12));
        assert!(!loaded.children_map.contains_key(&10));
        assert!(!loaded.children_map.contains_key(&11));
        assert!(!loaded.spawn_depth.contains_key(&11));
        assert!(!loaded.spawn_depth.contains_key(&12));
        assert!(!loaded.spawn_metadata.contains_key(&11));
        assert!(!loaded.spawn_metadata.contains_key(&12));

        unsafe {
            if let Some(home) = original_home {
                env::set_var("HOME", home);
            } else {
                env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
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

/// Wrapper for state entries to make them XML-safe
#[derive(Debug, Serialize, Deserialize, Clone)]
struct StateEntry {
    name: String,
    state: ServiceStateEntry,
}

/// Persistent record of the last-known state for every managed service.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ServiceStateFile {
    #[serde(
        serialize_with = "serialize_state_entries",
        deserialize_with = "deserialize_state_entries"
    )]
    services: HashMap<String, ServiceStateEntry>,
    /// The project state directory this file is bound to. Never serialized;
    /// re-attached after every load.
    #[serde(skip)]
    store: StateStore,
}

/// Serializes state entries.
fn serialize_state_entries<S>(
    map: &HashMap<String, ServiceStateEntry>,
    s: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(map.len()))?;
    for (k, v) in map {
        seq.serialize_element(&StateEntry {
            name: k.clone(),
            state: v.clone(),
        })?;
    }
    seq.end()
}

/// Handles deserialize state entries.
fn deserialize_state_entries<'de, D>(
    d: D,
) -> Result<HashMap<String, ServiceStateEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries: Vec<StateEntry> = Vec::deserialize(d)?;
    Ok(entries.into_iter().map(|e| (e.name, e.state)).collect())
}

impl ServiceStateFile {
    /// Handles path.
    fn path(&self) -> PathBuf {
        self.store.state_path()
    }

    /// Loads the service state file from disk, creating an empty one if it doesn't exist.
    pub fn load(store: StateStore) -> Result<Self, ServiceStateError> {
        let path = store.state_path();
        if !path.exists() {
            return Ok(Self {
                store,
                ..Self::default()
            });
        }

        let contents = fs::read_to_string(path)?;
        let mut state = xml_from_str::<Self>(&contents)?;
        state.store = store;
        Ok(state)
    }

    /// The project store this file is bound to.
    pub fn store(&self) -> StateStore {
        self.store.clone()
    }

    /// Binds this file to a project store.
    pub fn set_store(&mut self, store: StateStore) {
        self.store = store;
    }

    /// Sets a service's state in memory only, without persisting to disk.
    /// Intended for constructing snapshots in tests.
    pub fn set_in_memory(
        &mut self,
        service_hash: &str,
        status: ServiceLifecycleStatus,
        pid: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) {
        self.services.insert(
            service_hash.to_string(),
            ServiceStateEntry {
                status,
                pid,
                exit_code,
                signal,
            },
        );
    }

    /// Acquires an exclusive lock on the state file (auto-releases on drop).
    fn acquire_lock(&self) -> Result<File, ServiceStateError> {
        let lock_path = self.store.state_lock_path();
        runtime::create_private_dir(lock_path.parent().unwrap())?;
        let lock_file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock_file.lock_exclusive()?;
        Ok(lock_file)
    }

    /// Re-reads the on-disk file into `self`, preserving the bound store.
    /// A missing file is treated as empty. Keeps concurrent writers from
    /// clobbering each other's entries: a write always merges onto current disk.
    fn reload_locked(&mut self) -> Result<(), ServiceStateError> {
        let path = self.path();
        if !path.exists() {
            return Ok(());
        }
        let contents = fs::read_to_string(&path)?;
        let store = self.store.clone();
        *self = xml_from_str::<Self>(&contents)?;
        self.store = store;
        Ok(())
    }

    /// Saves the state file to disk.
    pub fn save(&self) -> Result<(), ServiceStateError> {
        let path = self.path();
        if let Some(parent) = path.parent() {
            runtime::create_private_dir(parent)?;
        }
        runtime::write_private_file(&path, xml_to_string(self)?)?;
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
    ///
    /// Takes the file lock and reloads from disk before applying, so a service
    /// starting concurrently in the same project can't clobber another's entry.
    pub fn set(
        &mut self,
        service_hash: &str,
        status: ServiceLifecycleStatus,
        pid: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), ServiceStateError> {
        let _lock = self.acquire_lock()?;
        self.reload_locked()?;
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
        let _lock = self.acquire_lock()?;
        self.reload_locked()?;
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
    pgid: Option<libc::pid_t>,
}

#[derive(Debug, Serialize, Deserialize)]
/// Represents blue green state.
struct BlueGreenState {
    /// Index of the currently active slot (0 or 1).
    active_slot_index: usize,
}

thread_local! {
    static HELD_LOCKS: std::cell::RefCell<HashSet<DaemonLock>> = std::cell::RefCell::new(HashSet::new());
}

/// A guard that enforces lock ordering and automatically tracks held locks.
struct OrderedLockGuard<'a, T> {
    guard: std::sync::MutexGuard<'a, T>,
    lock_type: DaemonLock,
}

impl<'a, T> Drop for OrderedLockGuard<'a, T> {
    /// Handles drop.
    fn drop(&mut self) {
        HELD_LOCKS.with(|held| {
            held.borrow_mut().remove(&self.lock_type);
        });
    }
}

impl<'a, T> std::ops::Deref for OrderedLockGuard<'a, T> {
    type Target = T;

    /// Handles deref.
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'a, T> std::ops::DerefMut for OrderedLockGuard<'a, T> {
    /// Handles deref mut.
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

/// Helper function to acquire a lock with ordering enforcement.
fn acquire_lock<'a, T>(
    mutex: &'a Arc<Mutex<T>>,
    lock_type: DaemonLock,
) -> Result<OrderedLockGuard<'a, T>, ProcessManagerError> {
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

    let guard = mutex.lock()?;

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
    /// Services with a reconcile-triggered restart currently in flight, so the
    /// monitor does not spawn overlapping restart threads for the same unit.
    restart_in_flight: Arc<Mutex<HashSet<String>>>,
    /// Flag indicating whether the monitoring loop should remain active.
    running: Arc<AtomicBool>,
    /// Pipe stderr to stdout.
    pipe_stderr: Arc<AtomicBool>,
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

    /// Acquires the restart_in_flight lock with ordering enforcement.
    fn lock_restart_in_flight(
        &self,
    ) -> Result<OrderedLockGuard<'_, HashSet<String>>, ProcessManagerError> {
        acquire_lock(&self.restart_in_flight, DaemonLock::RestartInFlight)
    }

    /// Creates a cancellation token for a Linux service thread.
    #[cfg(target_os = "linux")]
    fn create_cancellation_token(&self, service_name: &str) -> Arc<AtomicBool> {
        let token = Arc::new(AtomicBool::new(false));
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
#[derive(Clone)]
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
    /// Reconcile-triggered restarts currently in flight.
    restart_in_flight: Arc<Mutex<HashSet<String>>>,
    /// Linux thread cancellation.
    #[cfg(target_os = "linux")]
    thread_cancellation_tokens: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    /// Pipe stderr to stdout.
    pipe_stderr: Arc<AtomicBool>,
    /// Ownership sentinel: `Drop` only tears down the shared monitor/threads when
    /// the LAST clone is dropped. `Daemon` is `Clone` over shared `Arc`s, so a
    /// transient clone dropped mid-operation must never shut down the live daemon.
    liveness: Arc<()>,
    /// Reports what a blocking boot step is currently waiting on.
    op_slot: OpSlot,
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
            restart_in_flight: Arc::clone(&self.restart_in_flight),
            running: Arc::clone(&self.running),
            pipe_stderr: Arc::clone(&self.pipe_stderr),
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

    /// Returns the process-group ID for `pid` if it can be resolved.
    pub(crate) fn process_group_for_pid(pid: u32) -> Option<libc::pid_t> {
        let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
        if pgid >= 0 { Some(pgid) } else { None }
    }

    /// Returns all live process IDs currently assigned to `pgid`.
    #[cfg(target_os = "linux")]
    fn collect_process_group_members(pgid: libc::pid_t) -> HashSet<u32> {
        let mut members = HashSet::new();
        let Ok(entries) = fs::read_dir("/proc") else {
            return members;
        };

        for entry in entries.filter_map(Result::ok) {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };

            let stat_path = entry.path().join("stat");
            let Ok(stat) = fs::read_to_string(stat_path) else {
                continue;
            };
            let Some(close_paren) = stat.rfind(')') else {
                continue;
            };
            let mut fields = stat[close_paren + 1..].split_whitespace();
            let state = fields.next().and_then(|raw| raw.chars().next());
            if matches!(state, Some('Z' | 'X')) {
                continue;
            }

            let _ppid = fields.next();
            let Some(process_group) = fields
                .next()
                .and_then(|raw| raw.parse::<libc::pid_t>().ok())
            else {
                continue;
            };

            if process_group == pgid {
                members.insert(pid);
            }
        }

        members
    }

    /// Returns all live process IDs currently assigned to `pgid`.
    #[cfg(not(target_os = "linux"))]
    fn collect_process_group_members(pgid: libc::pid_t) -> HashSet<u32> {
        let mut members = HashSet::new();
        let Ok(output) = Command::new("ps")
            .args(["-axo", "pid=,pgid=,stat="])
            .output()
        else {
            return members;
        };

        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut fields = line.split_whitespace();
            let Some(pid) = fields.next().and_then(|raw| raw.parse::<u32>().ok()) else {
                continue;
            };
            let Some(process_group) = fields
                .next()
                .and_then(|raw| raw.parse::<libc::pid_t>().ok())
            else {
                continue;
            };
            let state = fields.next().and_then(|raw| raw.chars().next());

            if process_group == pgid && !matches!(state, Some('Z')) {
                members.insert(pid);
            }
        }

        members
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
                if signal.is_none()
                    && matches!(Self::read_proc_state(pid), Some('Z') | Some('X'))
                {
                    Self::reap_child_if_ready(pid);
                    return Ok(false);
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

    #[cfg(unix)]
    fn reap_child_if_ready(pid: u32) {
        use nix::sys::wait::{WaitPidFlag, waitpid};

        let target = nix::unistd::Pid::from_raw(pid as i32);
        match waitpid(target, Some(WaitPidFlag::WNOHANG)) {
            Ok(_) => {}
            Err(nix::errno::Errno::ECHILD) => {}
            Err(err) => {
                debug!("Failed to reap child process {pid}: {err}");
            }
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
        parts.next()?;
        let mut name_part = parts.next()?;
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

    /// Reads the process state character from `ps` on non-Linux Unix platforms.
    #[cfg(not(target_os = "linux"))]
    fn read_proc_state(pid: u32) -> Option<char> {
        let output = Command::new("ps")
            .args(["-o", "stat=", "-p"])
            .arg(pid.to_string())
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .chars()
            .next()
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

    fn process_command_line(process: &sysinfo::Process) -> String {
        process
            .cmd()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn process_outside_project_root(
        process: &sysinfo::Process,
        project_root: &Path,
    ) -> bool {
        let Some(cwd) = process.cwd() else {
            return false;
        };

        cwd != project_root
            && cwd
                .canonicalize()
                .ok()
                .as_deref()
                .is_none_or(|canonical| canonical != project_root)
    }

    fn process_matches_service_command(
        process: &sysinfo::Process,
        service: &ServiceConfig,
    ) -> bool {
        let command_line = Self::process_command_line(process);
        if command_line == service.command {
            return true;
        }

        let args = process.cmd();
        if args.len() >= 3
            && args[1].to_string_lossy() == SHELL_COMMAND_FLAG
            && args[2].to_string_lossy() == service.command
        {
            let shell = Path::new(&args[0])
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            return shell == DEFAULT_SHELL;
        }

        false
    }

    /// Finds service processes that match the configured command and project root, including
    /// stale instances whose PID file entries were overwritten by a later start.
    fn collect_config_owned_service_roots(
        service_name: &str,
        service: &ServiceConfig,
        project_root: &Path,
    ) -> HashSet<u32> {
        let canonical_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::everything(),
        );

        system
            .processes()
            .iter()
            .filter_map(|(pid, process)| {
                let raw_pid = pid.as_u32();
                if raw_pid == std::process::id() {
                    return None;
                }
                if Self::process_outside_project_root(process, &canonical_root) {
                    return None;
                }
                if !Self::process_matches_service_command(process, service) {
                    return None;
                }

                debug!(
                    "Found config-owned stale process for '{service_name}': pid={} command={:?}",
                    raw_pid,
                    process.cmd()
                );
                Some(raw_pid)
            })
            .collect()
    }

    fn cleanup_config_owned_service_processes(
        service_name: &str,
        service: &ServiceConfig,
        project_root: &Path,
    ) -> Result<(), ProcessManagerError> {
        let roots =
            Self::collect_config_owned_service_roots(service_name, service, project_root);

        for pid in roots {
            if !Self::signal_pid(service_name, pid, None)? {
                continue;
            }

            let group = Self::process_group_for_pid(pid);
            info!(
                "Stopping stale config-owned process for '{service_name}' with pid {pid}"
            );
            match Self::terminate_process_tree(service_name, pid, group) {
                Ok(()) => {}
                Err(ProcessManagerError::ServiceStopError { source, .. })
                    if source.kind() == std::io::ErrorKind::TimedOut =>
                {
                    warn!(
                        "Timed out terminating stale process for '{service_name}' (pid {pid})"
                    );
                }
                Err(err) => return Err(err),
            }
        }

        Ok(())
    }

    /// Terminates a process and all its descendants using escalating signals. First sends SIGTERM
    /// to the entire process tree and waits for graceful shutdown. If processes don't exit within
    /// the timeout, escalates to SIGKILL. Returns an error if any processes survive after SIGKILL.
    pub(crate) fn terminate_process_tree(
        service_name: &str,
        root_pid: u32,
        group_hint: Option<libc::pid_t>,
    ) -> Result<(), ProcessManagerError> {
        use nix::sys::signal::Signal::{SIGKILL, SIGTERM};

        let mut pending = Self::collect_descendants(root_pid);
        pending.insert(root_pid);

        let supervisor_pgid = unsafe { libc::getpgid(0) };
        let group_target = if let Some(pgid) =
            group_hint.or_else(|| Self::process_group_for_pid(root_pid))
        {
            Some(pgid)
        } else {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(code) if code == libc::ESRCH => Some(root_pid as libc::pid_t),
                _ => None,
            }
        };

        let signal_group = |signal: libc::c_int| {
            if let Some(target_pgid) = group_target
                && target_pgid >= 0
                && target_pgid != supervisor_pgid
            {
                let result = unsafe { libc::killpg(target_pgid, signal) };
                if result < 0 {
                    let err = std::io::Error::last_os_error();
                    match err.raw_os_error() {
                        Some(code) if code == libc::ESRCH => {}
                        Some(code) if code == libc::EPERM => {
                            warn!(
                                "Insufficient permissions to signal process group {} for '{}'",
                                target_pgid, service_name
                            );
                        }
                        _ => {
                            warn!(
                                "Failed to signal process group {target_pgid} for '{service_name}': {err}"
                            );
                        }
                    }
                }
            }
        };

        let merge_group_members = |pending: &mut HashSet<u32>| {
            if let Some(target_pgid) = group_target
                && target_pgid >= 0
                && target_pgid != supervisor_pgid
            {
                pending.extend(Self::collect_process_group_members(target_pgid));
            }
        };

        merge_group_members(&mut pending);

        signal_group(SIGTERM as libc::c_int);
        pending = Self::send_signal_to_pids(service_name, pending, SIGTERM)?;
        pending = Self::wait_for_exit(
            service_name,
            pending,
            PROCESS_READY_CHECKS,
            PROCESS_CHECK_INTERVAL,
        )?;
        merge_group_members(&mut pending);

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
        merge_group_members(&mut pending);
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

    /// Terminates any live members still lingering in a service's previous process group before
    /// it is restarted. When a wrapper shell exits while its real worker keeps running, the worker
    /// is reparented to PID 1 but retains the original process group. Without this cleanup a restart
    /// would spawn a fresh instance alongside the surviving orphan, leaking duplicate workers.
    fn reap_orphaned_group_before_restart(
        service_name: &str,
        recorded_pgid: Option<libc::pid_t>,
    ) {
        let supervisor_pgid = unsafe { libc::getpgid(0) };
        let Some(pgid) = recorded_pgid else {
            return;
        };
        if pgid < 0 || pgid == supervisor_pgid {
            return;
        }

        let survivors = Self::collect_process_group_members(pgid);
        if survivors.is_empty() {
            return;
        }

        warn!(
            "Found {} orphaned process(es) in previous group {} for '{}'; terminating before restart",
            survivors.len(),
            pgid,
            service_name
        );

        if let Err(err) =
            Self::terminate_process_tree(service_name, pgid as u32, Some(pgid))
        {
            warn!(
                "Failed to fully terminate orphaned group {} for '{}' before restart: {}",
                pgid, service_name, err
            );
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

        let store = StateStore::for_project(&config.project.id);
        if let Ok(mut guard) = pid_file.lock() {
            guard.set_store(store.clone());
        }
        if let Ok(mut guard) = state_file.lock() {
            guard.set_store(store);
        }

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
            restart_in_flight: Arc::new(Mutex::new(HashSet::new())),
            #[cfg(target_os = "linux")]
            thread_cancellation_tokens: Arc::new(Mutex::new(HashMap::new())),
            pipe_stderr: Arc::new(AtomicBool::new(false)),
            op_slot: OpSlot::new(),
            liveness: Arc::new(()),
        }
    }

    /// Points the daemon at the supervisor's shared operation slot so blocking
    /// boot steps report what they are waiting on.
    pub fn set_op_slot(&mut self, op_slot: OpSlot) {
        self.op_slot = op_slot;
    }

    /// Convenience constructor that loads the PID file automatically.
    pub fn from_config(
        config: Config,
        detach_children: bool,
    ) -> Result<Self, ProcessManagerError> {
        let store = StateStore::for_project(&config.project.id);
        let pid_file = Arc::new(Mutex::new(PidFile::load(store.clone())?));
        let state_file = Arc::new(Mutex::new(ServiceStateFile::load(store)?));
        Ok(Self::new(config, pid_file, state_file, detach_children))
    }

    /// Sets whether to pipe stderr from services to stdout.
    pub fn set_pipe_stderr(&mut self, pipe_stderr: bool) {
        self.pipe_stderr.store(pipe_stderr, Ordering::SeqCst);
    }

    /// Returns a reference to the configuration.
    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config)
    }

    /// Returns a handle to the shared PID file so callers can inspect process IDs.
    pub fn pid_file_handle(&self) -> Arc<Mutex<PidFile>> {
        Arc::clone(&self.pid_file)
    }

    /// The project state store this daemon's files are bound to.
    pub fn store(&self) -> StateStore {
        StateStore::for_project(&self.config.project.id)
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
    /// The launched PID and resolved process-group ID if successful.
    fn launch_attached_service(
        service_name: &str,
        service_config: &ServiceConfig,
        working_dir: PathBuf,
        processes: Arc<Mutex<HashMap<String, Child>>>,
        detach_children: bool,
        pipe_stderr: bool,
        log_settings: EffectiveLogsConfig,
    ) -> Result<(u32, Option<libc::pid_t>), ProcessManagerError> {
        let command = &service_config.command;
        debug!("Launching service: '{service_name}' with command: `{command}`");

        let mut cmd = Command::new(DEFAULT_SHELL);
        cmd.arg(SHELL_COMMAND_FLAG).arg(command);
        cmd.current_dir(&working_dir);

        debug!("Executing command: {cmd:?}");

        match log_settings.sink {
            LogSink::File => {
                cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            }
            LogSink::None => {
                cmd.stdout(Stdio::null());
                if pipe_stderr {
                    cmd.stderr(Stdio::piped());
                } else {
                    cmd.stderr(Stdio::null());
                }
            }
        }

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

        let inherit_env = service_config
            .env
            .as_ref()
            .and_then(|env| env.inherit_env)
            .unwrap_or(false);
        if privilege.user.drops_privileges() && !inherit_env {
            debug!("Starting '{service_name}' from a clean environment (privilege drop)");
            cmd.env_clear();
            merged_env
                .entry("PATH".to_string())
                .or_insert_with(|| DEFAULT_SERVICE_PATH.to_string());
        }

        if !merged_env.is_empty() {
            let keys: Vec<_> = merged_env.keys().cloned().collect();
            debug!("Setting environment variables: {:?}", keys);
            for (key, value) in merged_env {
                cmd.env(key, value);
            }
        }

        let to_strip = match &service_config.env {
            Some(env_config) => env_config.vars_to_strip(),
            None => SESSION_SCOPED_ENV_VARS
                .iter()
                .map(|v| v.to_string())
                .collect(),
        };
        if !to_strip.is_empty() {
            debug!("Stripping inherited environment variables: {:?}", to_strip);
            for key in to_strip {
                cmd.env_remove(key);
            }
        }

        let privilege_clone = privilege.clone();

        unsafe {
            cmd.pre_exec(move || {
                if detach_children {
                    if libc::setsid() < 0 {
                        let err = std::io::Error::last_os_error();
                        eprintln!("systemg pre_exec: setsid failed: {:?}", err);
                        return Err(err);
                    }
                } else if libc::setpgid(0, 0) < 0 {
                    let err = std::io::Error::last_os_error();
                    eprintln!("systemg pre_exec: setpgid(0, 0) failed: {:?}", err);
                    return Err(err);
                }

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

                if pipe_stderr {
                    if let Some(err) = stderr {
                        use std::{
                            io::{self, BufRead, BufReader, Write},
                            thread,
                        };

                        let service_name_clone = service_name.to_string();
                        thread::spawn(move || {
                            let reader = BufReader::new(err);
                            let mut stdout = io::stdout();
                            for line in reader.lines().map_while(Result::ok) {
                                let _ = writeln!(
                                    stdout,
                                    "[{}:stderr] {}",
                                    service_name_clone, line
                                );
                                let _ = stdout.flush();
                            }
                        });
                    }
                } else {
                    spawn_service_log_writers(
                        service_name,
                        stdout.map(|reader| Box::new(reader) as Box<dyn Read + Send>),
                        stderr.map(|reader| Box::new(reader) as Box<dyn Read + Send>),
                        log_settings,
                    );
                }

                processes.lock()?.insert(service_name.to_string(), child);

                if let Err(err) = privilege.apply_post_spawn(pid as libc::pid_t) {
                    warn!(
                        "Failed to apply post-spawn privilege adjustments for '{service_name}': {err}"
                    );
                }
                let pgid = Self::process_group_for_pid(pid).or_else(|| {
                    debug!("Could not get pgid for {service_name} (pid {pid}), assuming pid == pgid");
                    Some(pid as libc::pid_t)
                });
                Ok((pid, pgid))
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

    /// Launches a Linux service from a dedicated lifetime thread so `PR_SET_PDEATHSIG`
    /// remains tied to a live parent until the service is explicitly stopped.
    #[cfg(target_os = "linux")]
    fn launch_service_with_lifetime_thread(
        ctx: &DaemonContext,
        service_name: String,
        service_config: ServiceConfig,
        log_settings: EffectiveLogsConfig,
    ) -> Result<u32, ProcessManagerError> {
        use std::{sync::mpsc, thread};

        let cancellation_token = ctx.create_cancellation_token(&service_name);
        let processes = Arc::clone(&ctx.processes);
        let pid_file = Arc::clone(&ctx.pid_file);
        let working_dir = ctx.project_root.clone();
        let detach_children = ctx.detach_children;
        let pipe_stderr = ctx.pipe_stderr.load(Ordering::SeqCst);
        let service_name_for_thread = service_name.clone();
        let service_name_for_cleanup = service_name.clone();

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            debug!("Starting service thread for '{service_name_for_thread}'");

            let launch_result = Daemon::launch_attached_service(
                &service_name_for_thread,
                &service_config,
                working_dir,
                Arc::clone(&processes),
                detach_children,
                pipe_stderr,
                log_settings,
            );

            match launch_result {
                Ok((pid, pgid)) => {
                    let record_result = pid_file
                        .lock()
                        .map_err(ProcessManagerError::from)
                        .and_then(|mut guard| {
                            guard
                                .insert_with_group(&service_name_for_thread, pid, pgid)
                                .map_err(ProcessManagerError::from)
                        });

                    if let Err(err) = record_result {
                        error!(
                            "Failed to record PID {pid} for service '{service_name_for_thread}': {err}"
                        );

                        if let Err(stop_err) = Self::terminate_process_tree(
                            &service_name_for_thread,
                            pid,
                            pgid,
                        ) {
                            warn!(
                                "Also failed to terminate untracked service '{service_name_for_thread}': {stop_err}"
                            );
                        }

                        if let Ok(mut guard) = processes.lock()
                            && let Some(mut child) =
                                guard.remove(&service_name_for_thread)
                        {
                            let _ = child.wait();
                        }

                        let _ = tx.send(Err(err));
                        return;
                    }

                    if tx.send(Ok(pid)).is_err() {
                        return;
                    }

                    while !cancellation_token.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_secs(1));
                    }
                    debug!(
                        "Service thread for '{service_name_for_thread}' terminated by cancellation token"
                    );
                }
                Err(err) => {
                    error!("Failed to start service '{service_name_for_thread}': {err}");
                    let _ = tx.send(Err(err));
                }
            }
        });

        match rx.recv() {
            Ok(Ok(pid)) => Ok(pid),
            Ok(Err(err)) => {
                ctx.cancel_service_thread(&service_name_for_cleanup);
                Err(err)
            }
            Err(recv_err) => {
                ctx.cancel_service_thread(&service_name_for_cleanup);
                Err(ProcessManagerError::ServiceStartError {
                    service: service_name,
                    source: std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        format!("thread failed to report launch status: {recv_err}"),
                    ),
                })
            }
        }
    }

    /// Launches a service and records its PID using the platform's supervision model.
    #[cfg(target_os = "linux")]
    fn launch_service_for_supervision(
        ctx: &DaemonContext,
        service_name: String,
        service_config: ServiceConfig,
        log_settings: EffectiveLogsConfig,
    ) -> Result<u32, ProcessManagerError> {
        Self::launch_service_with_lifetime_thread(
            ctx,
            service_name,
            service_config,
            log_settings,
        )
    }

    /// Launches a service and records its PID using the platform's supervision model.
    #[cfg(not(target_os = "linux"))]
    fn launch_service_for_supervision(
        ctx: &DaemonContext,
        service_name: String,
        service_config: ServiceConfig,
        log_settings: EffectiveLogsConfig,
    ) -> Result<u32, ProcessManagerError> {
        let (pid, pgid) = Self::launch_attached_service(
            &service_name,
            &service_config,
            ctx.project_root.clone(),
            Arc::clone(&ctx.processes),
            ctx.detach_children,
            ctx.pipe_stderr.load(Ordering::SeqCst),
            log_settings,
        )?;

        let record_result = ctx
            .pid_file
            .lock()
            .map_err(ProcessManagerError::from)
            .and_then(|mut guard| {
                guard
                    .insert_with_group(&service_name, pid, pgid)
                    .map_err(ProcessManagerError::from)
            });

        if let Err(err) = record_result {
            error!("Failed to record PID {pid} for service '{service_name}': {err}");

            if let Err(stop_err) = Self::terminate_process_tree(&service_name, pid, pgid)
            {
                warn!(
                    "Also failed to terminate untracked service '{service_name}': {stop_err}"
                );
            }

            if let Ok(mut guard) = ctx.processes.lock()
                && let Some(mut child) = guard.remove(&service_name)
            {
                let _ = child.wait();
            }

            return Err(err);
        }

        Ok(pid)
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

        {
            let mut suppressed = self.restart_suppressed.lock()?;
            suppressed.remove(name);
        }

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

        if let Some(pre_start) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.pre_start.as_ref())
        {
            if let Some(dep) = self.pre_start_duplicate_dependency(service, pre_start) {
                info!(
                    "Skipping pre-start for '{name}': identical to completed dependency '{dep}' which already ran"
                );
            } else {
                info!("Running pre-start command for '{name}': {pre_start}");
                self.op_slot
                    .detail(format!("running pre-start for '{name}'"));
                self.run_pre_start_command(name, pre_start)?;
            }
        }

        Ok(None)
    }

    /// Returns the name of a `condition: completed` dependency whose command is
    /// identical to this service's pre-start, so the shared build is not run twice.
    fn pre_start_duplicate_dependency(
        &self,
        service: &ServiceConfig,
        pre_start: &str,
    ) -> Option<String> {
        let deps = service.depends_on.as_ref()?;
        deps.iter()
            .filter(|dep| dep.condition() == DependsOnCondition::Completed)
            .find_map(|dep| {
                let dep_name = dep.service();
                let dep_config = self.config.services.get(dep_name)?;
                (dep_config.command.trim() == pre_start.trim())
                    .then(|| dep_name.to_string())
            })
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
        let mut completed_services = HashSet::new();
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
                completed_services.insert(service_name.clone());
                continue 'service_loop;
            }

            if let Some(skip_config) = &service.skip {
                match skip_config {
                    SkipConfig::Flag(true) => {
                        info!("Skipping service '{service_name}' due to skip flag");
                        healthy_services.insert(service_name.clone());
                        completed_services.insert(service_name.clone());
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
                                completed_services.insert(service_name.clone());
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
                    let dep_name = dep.service();
                    if failed_services.contains(dep_name) {
                        error!(
                            "Skipping start of '{service_name}' because dependency '{dep_name}' failed."
                        );
                        if first_error.is_none() {
                            first_error = Some(ProcessManagerError::DependencyFailed {
                                service: service_name.clone(),
                                dependency: dep_name.to_string(),
                            });
                        }
                        failed_services.insert(service_name.clone());
                        continue 'service_loop;
                    }

                    if !healthy_services.contains(dep_name) {
                        error!(
                            "Skipping start of '{service_name}' because dependency '{dep_name}' is not running."
                        );
                        if first_error.is_none() {
                            first_error = Some(ProcessManagerError::DependencyError {
                                service: service_name.clone(),
                                dependency: dep_name.to_string(),
                            });
                        }
                        failed_services.insert(service_name.clone());
                        continue 'service_loop;
                    }

                    if dep.condition() == DependsOnCondition::Completed
                        && !completed_services.contains(dep_name)
                    {
                        if let Err(err) =
                            self.wait_for_dependency_completion(&service_name, dep_name)
                        {
                            error!(
                                "Skipping start of '{service_name}' because dependency '{dep_name}' did not complete: {err}"
                            );
                            if first_error.is_none() {
                                first_error = Some(err);
                            }
                            failed_services.insert(service_name.clone());
                            continue 'service_loop;
                        }
                        completed_services.insert(dep_name.to_string());
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
                    completed_services.insert(service_name.clone());
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
        let state = Self::wait_for_ready(
            service_name,
            &self.config.project.id,
            &self.processes,
            &self.pid_file,
        )?;

        if let ServiceReadyState::Running = state
            && let Some(health_check) = self
                .config
                .services
                .get(service_name)
                .and_then(|service| service.deployment.as_ref())
                .and_then(|deployment| deployment.health_check.as_ref())
        {
            info!("Waiting for health check of '{service_name}' before marking it ready");
            self.wait_for_health_check(service_name, health_check)?;
        }

        Ok(state)
    }

    /// Internal implementation of wait_for_service_ready that accepts explicit handles for processes
    /// and PID file. This allows the function to be called from both instance methods and static contexts.
    fn wait_for_ready(
        service_name: &str,
        project: &str,
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
                        "Service '{service_name}' exited during startup (exit_code={exit_code:?}, signal={signal:?})."
                    );

                    let how = match (exit_code, signal) {
                        (Some(code), _) => format!("exited with status {code}"),
                        (None, Some(sig)) => format!("was killed by signal {sig}"),
                        (None, None) => "terminated unexpectedly".to_string(),
                    };

                    let diag = crate::diag::Diagnostic::error(
                        "SG0102",
                        format!("service `{service_name}` exited immediately at start"),
                    )
                    .note(format!("the process {how} before it finished starting"))
                    .evidence(
                        format!("last output from `{service_name}`"),
                        crate::logs::tail_service_log(service_name, 8),
                    )
                    .help_cmd(
                        "view logs",
                        format!("sysg logs -s {service_name} -p {project}"),
                    )
                    .help_cmd("check status", format!("sysg status -p {project}"))
                    .help_docs();

                    return Err(ProcessManagerError::Diag(Box::new(diag)));
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

    /// Blocks until a `condition: completed` dependency exits cleanly.
    ///
    /// Polls the dependency's process without a timeout — builds and migrations can
    /// legitimately run for minutes. Returns [`ProcessManagerError::DependencyFailed`]
    /// if the dependency exits with a non-zero status or was stopped.
    fn wait_for_dependency_completion(
        &self,
        service_name: &str,
        dep: &str,
    ) -> Result<(), ProcessManagerError> {
        info!("Waiting for dependency '{dep}' of '{service_name}' to complete");
        self.op_slot
            .detail(format!("waiting on dependency '{dep}' of '{service_name}'"));

        loop {
            match Self::probe_service_state(dep, &self.processes, &self.pid_file)? {
                ServiceProbe::Running => thread::sleep(SERVICE_POLL_INTERVAL),
                ServiceProbe::Exited(status) => {
                    if status.success() {
                        info!("Dependency '{dep}' completed successfully");
                        self.update_state(
                            dep,
                            ServiceLifecycleStatus::ExitedSuccessfully,
                            None,
                            Some(0),
                            None,
                        )?;
                        return Ok(());
                    }

                    #[cfg(unix)]
                    let signal = status.signal();
                    #[cfg(not(unix))]
                    let signal: Option<i32> = None;
                    self.update_state(
                        dep,
                        ServiceLifecycleStatus::ExitedWithError,
                        None,
                        status.code(),
                        signal,
                    )?;

                    return Err(ProcessManagerError::DependencyFailed {
                        service: service_name.to_string(),
                        dependency: dep.to_string(),
                    });
                }
                ServiceProbe::NotStarted => {
                    let status = self.get_service_hash(dep).and_then(|hash| {
                        self.state_file
                            .lock()
                            .ok()
                            .and_then(|state| state.get(&hash).map(|entry| entry.status))
                    });

                    match status {
                        Some(
                            ServiceLifecycleStatus::ExitedSuccessfully
                            | ServiceLifecycleStatus::Skipped,
                        ) => return Ok(()),
                        Some(
                            ServiceLifecycleStatus::ExitedWithError
                            | ServiceLifecycleStatus::Stopped,
                        ) => {
                            return Err(ProcessManagerError::DependencyFailed {
                                service: service_name.to_string(),
                                dependency: dep.to_string(),
                            });
                        }
                        _ => thread::sleep(SERVICE_POLL_INTERVAL),
                    }
                }
            }
        }
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
                    if let Err(err) = pid_guard.clear_pid(service_name)
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
        let mut completed_services = HashSet::new();

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
                completed_services.insert(service_name.clone());
                continue;
            }

            if let Some(deps) = &service.depends_on {
                for dep in deps {
                    let dep_name = dep.service();
                    if dep.condition() == DependsOnCondition::Completed
                        && !completed_services.contains(dep_name)
                    {
                        self.wait_for_dependency_completion(&service_name, dep_name)?;
                        completed_services.insert(dep_name.to_string());
                    }
                }
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
                    if matches!(
                        self.rolling_restart_service(&service_name, service)?,
                        ServiceReadyState::CompletedSuccess
                    ) {
                        completed_services.insert(service_name.clone());
                    }
                }
                DeploymentStrategy::Immediate => {
                    if matches!(
                        self.immediate_restart_service(&service_name, service)?,
                        ServiceReadyState::CompletedSuccess
                    ) {
                        completed_services.insert(service_name.clone());
                    }
                }
            }
        }

        self.spawn_monitor_thread()?;
        self.verify_services_running(&restarted_services, &completed_services)?;
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

        let start_state = match strategy {
            DeploymentStrategy::Rolling => self.rolling_restart_service(name, service)?,
            DeploymentStrategy::Immediate => {
                self.immediate_restart_service(name, service)?
            }
        };

        let completed_services =
            if matches!(start_state, ServiceReadyState::CompletedSuccess) {
                HashSet::from([name.to_string()])
            } else {
                HashSet::new()
            };
        self.verify_services_running(&[name.to_string()], &completed_services)?;

        Ok(())
    }

    /// Performs a rolling restart keeping the previous instance alive until the replacement is
    /// verified healthy.
    fn rolling_restart_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        if let Some(blue_green) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.blue_green.as_ref())
        {
            return self.blue_green_restart_service(name, service, blue_green);
        }

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

        Ok(start_state)
    }

    /// Performs a blue/green restart by launching a candidate on the inactive slot, switching traffic,
    /// and retiring the previous instance after optional grace-period drain.
    fn blue_green_restart_service(
        &self,
        name: &str,
        service: &ServiceConfig,
        blue_green: &BlueGreenDeploymentConfig,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        info!("Performing blue/green rolling restart for service: {name}");

        if blue_green.slots.len() != 2 {
            return Err(Self::config_error(format!(
                "blue_green.slots for '{name}' must contain exactly two entries"
            )));
        }

        let env_var = blue_green
            .env_var
            .clone()
            .unwrap_or_else(|| "PORT".to_string());
        let active_idx = self.read_blue_green_active_index(name, blue_green)?;
        let candidate_idx = if active_idx == 0 { 1 } else { 0 };
        let active_slot = blue_green.slots[active_idx].clone();
        let candidate_slot = blue_green.slots[candidate_idx].clone();

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

        let candidate_service =
            Self::service_with_env_override(service, &env_var, &candidate_slot);

        let start_state = match self.start_service(name, &candidate_service) {
            Ok(state) => state,
            Err(err) => {
                if let Err(stop_err) = self.stop_service_with_intent(name, false) {
                    warn!(
                        "Failed to stop candidate instance of '{name}' after start error: {stop_err}"
                    );
                }
                if let Some(detached) = previous.take() {
                    self.restore_detached_service(name, detached)?;
                }
                return Err(err);
            }
        };

        if matches!(start_state, ServiceReadyState::CompletedSuccess) {
            info!(
                "Candidate service '{name}' exited successfully immediately after blue/green start."
            );
        }

        if let Some(health_check) = &blue_green.candidate_health_check {
            let health_check = Self::resolve_blue_green_health_check(
                health_check,
                name,
                &active_slot,
                &candidate_slot,
            );
            self.wait_for_health_check(name, &health_check)?;
        } else if let Some(health_check) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.health_check.as_ref())
            && let Err(err) = self.wait_for_health_check(name, health_check)
        {
            if let Err(stop_err) = self.stop_service_with_intent(name, false) {
                warn!(
                    "Failed to stop candidate instance of '{name}' after health-check failure: {stop_err}"
                );
            }
            if let Some(detached) = previous.take() {
                self.restore_detached_service(name, detached)?;
            }
            return Err(err);
        }

        if let Some(command) = &blue_green.switch_command {
            if let Err(err) = self.run_blue_green_switch_command(
                name,
                command,
                &active_slot,
                &candidate_slot,
            ) {
                if let Err(stop_err) = self.stop_service_with_intent(name, false) {
                    warn!(
                        "Failed to stop candidate instance of '{name}' after switch error: {stop_err}"
                    );
                }
                if let Some(detached) = previous.take() {
                    self.restore_detached_service(name, detached)?;
                }
                return Err(err);
            }
        } else {
            return Err(Self::config_error(format!(
                "blue_green.switch_command is required for service '{name}'"
            )));
        }

        if let Some(health_check) = &blue_green.switch_verify {
            let health_check = Self::resolve_blue_green_health_check(
                health_check,
                name,
                &active_slot,
                &candidate_slot,
            );
            self.wait_for_health_check(name, &health_check)?;
        }

        if let Some(grace_period) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.grace_period.as_ref())
        {
            let duration = Self::parse_duration(grace_period)?;
            if !duration.is_zero() {
                thread::sleep(duration);
            }
        }

        if let Some(detached) = previous.take() {
            self.terminate_service(name, detached)?;
        }

        self.write_blue_green_active_index(name, blue_green, candidate_idx)?;
        Ok(start_state)
    }

    /// Performs an immediate restart by stopping and starting the service sequentially.
    fn immediate_restart_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        info!("Performing immediate restart for service: {name}");

        self.stop_service_with_intent(name, false)?;
        let start_state = self.start_service(name, service)?;

        if let ServiceReadyState::CompletedSuccess = start_state {
            info!("Service '{name}' completed successfully immediately after restart.");
        }

        Ok(start_state)
    }

    /// Runs the configured pre-start command prior to launching a replacement service instance.
    fn run_pre_start_command(
        &self,
        service_name: &str,
        command: &str,
    ) -> Result<(), ProcessManagerError> {
        use std::{
            fs::OpenOptions,
            io::{BufRead, BufReader, Write},
            process::Stdio,
            sync::{Arc, Mutex},
            thread,
        };

        let started = Instant::now();

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
        let open_sink = |kind: &str| {
            let path = resolve_log_path(service_name, kind);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            Arc::new(Mutex::new(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .ok(),
            ))
        };
        let stdout_sink = open_sink("stdout");
        let stderr_sink = open_sink("stderr");

        let write_marker = |line: &str| {
            for sink in [&stdout_sink, &stderr_sink] {
                if let Ok(mut guard) = sink.lock()
                    && let Some(file) = guard.as_mut()
                {
                    let _ = writeln!(file, "[pre_start] {line}");
                }
            }
        };
        write_marker(&format!("\u{25b6} running: {command}"));

        let tail: Arc<Mutex<std::collections::VecDeque<String>>> =
            Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let push_tail = |tail: &Arc<Mutex<std::collections::VecDeque<String>>>,
                         line: &str| {
            if let Ok(mut guard) = tail.lock() {
                if guard.len() >= 12 {
                    guard.pop_front();
                }
                guard.push_back(line.to_string());
            }
        };

        let stdout_handle = child.stdout.take().map(|stdout| {
            let service_name = service_name_owned.clone();
            let stdout_sink = Arc::clone(&stdout_sink);
            let stderr_sink = Arc::clone(&stderr_sink);
            let tail = Arc::clone(&tail);
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    info!("[{service_name} pre-start] {line}");
                    push_tail(&tail, &line);
                    for sink in [&stdout_sink, &stderr_sink] {
                        if let Ok(mut guard) = sink.lock()
                            && let Some(file) = guard.as_mut()
                        {
                            let _ = writeln!(file, "[pre_start] {line}");
                        }
                    }
                }
            })
        });

        let stderr_handle = child.stderr.take().map(|stderr| {
            let service_name = service_name_owned.clone();
            let stdout_sink = Arc::clone(&stdout_sink);
            let stderr_sink = Arc::clone(&stderr_sink);
            let tail = Arc::clone(&tail);
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    warn!("[{service_name} pre-start] {line}");
                    push_tail(&tail, &line);
                    for sink in [&stdout_sink, &stderr_sink] {
                        if let Ok(mut guard) = sink.lock()
                            && let Some(file) = guard.as_mut()
                        {
                            let _ = writeln!(file, "[pre_start] {line}");
                        }
                    }
                }
            })
        });

        let status =
            child
                .wait()
                .map_err(|source| ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source,
                })?;

        if let Some(handle) = stdout_handle {
            let _ = handle.join();
        }
        if let Some(handle) = stderr_handle {
            let _ = handle.join();
        }

        let elapsed = started.elapsed().as_secs();

        if !status.success() {
            let exit_code = status.code();
            #[cfg(unix)]
            let signal = status.signal();
            #[cfg(not(unix))]
            let signal: Option<i32> = None;

            self.record_start_failure(service_name, exit_code, signal);

            write_marker(&format!("\u{2716} failed after {elapsed}s ({status})"));

            let captured: Vec<String> = tail
                .lock()
                .map(|guard| guard.iter().cloned().collect())
                .unwrap_or_default();
            let project = self.config.project.id.clone();
            let diag = crate::diag::Diagnostic::error(
                "SG0103",
                format!("pre_start for `{service_name}` failed"),
            )
            .origin(
                format!("services.{service_name}.deployment.pre_start"),
                None,
                None,
            )
            .note(format!("`{command}` exited with {status} after {elapsed}s"))
            .note("the service was not started because its pre_start command failed")
            .evidence("pre_start output", captured)
            .help_cmd(
                "view logs",
                format!("sysg logs -s {service_name} -p {project}"),
            )
            .help_docs();
            return Err(ProcessManagerError::Diag(Box::new(diag)));
        }

        write_marker(&format!("\u{2714} completed in {elapsed}s (exit 0)"));

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
        let retry_interval = health_check
            .interval
            .as_deref()
            .map_or(Ok(Duration::from_secs(2)), Self::parse_duration)?;
        let client = if health_check.url.is_some() {
            Some(Client::builder().timeout(timeout).build().map_err(|err| {
                ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source: std::io::Error::other(err.to_string()),
                }
            })?)
        } else {
            None
        };

        let deadline = Instant::now() + timeout;

        for attempt in 1..=retries {
            self.op_slot.detail(format!(
                "health check for '{service_name}' (attempt {attempt}/{retries})"
            ));
            match self.perform_configured_health_check(
                service_name,
                health_check,
                client.as_ref(),
                timeout,
            ) {
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

        Err(ProcessManagerError::Diag(Box::new(
            self.health_check_failure_diag(service_name, health_check, timeout, retries),
        )))
    }

    /// Builds the diagnostic for a service that never became healthy: what was
    /// checked, whether the process is even alive, its last output, and the
    /// exact commands to dig further.
    fn health_check_failure_diag(
        &self,
        service_name: &str,
        health_check: &HealthCheckConfig,
        timeout: Duration,
        retries: u32,
    ) -> crate::diag::Diagnostic {
        use crate::diag::Diagnostic;

        let project = self.config.project.id.clone();
        let target = health_check
            .url
            .as_deref()
            .or(health_check.command.as_deref())
            .unwrap_or("<unconfigured>");

        let alive = self
            .pid_file
            .lock()
            .ok()
            .and_then(|guard| guard.pid_for(service_name))
            .is_some_and(|pid| {
                #[cfg(target_os = "linux")]
                {
                    !matches!(Self::read_proc_state(pid), None | Some('Z') | Some('X'))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None)
                        .is_ok()
                }
            });

        let mut diag = Diagnostic::error(
            "SG0104",
            format!("service `{service_name}` failed to become healthy"),
        )
        .note(format!(
            "{retries} health checks against {target} failed over {}s",
            timeout.as_secs()
        ));

        diag = if alive {
            diag.note(
                "the process is running but never answered the health check \
                 — it may be listening on a different address or still starting",
            )
        } else {
            diag.note(
                "the process is not running — it exited before it could become healthy",
            )
        };

        diag.evidence(
            format!("last output from `{service_name}`"),
            crate::logs::tail_service_log(service_name, 8),
        )
        .help_cmd(
            "view logs",
            format!("sysg logs -s {service_name} -p {project}"),
        )
        .help_cmd("check status", format!("sysg status -p {project}"))
        .help_docs()
    }

    /// Performs a single configured health check, using command or HTTP mode.
    fn perform_configured_health_check(
        &self,
        service_name: &str,
        health_check: &HealthCheckConfig,
        client: Option<&Client>,
        timeout: Duration,
    ) -> Result<bool, std::io::Error> {
        if let Some(command) = &health_check.command {
            self.perform_command_health_check(service_name, command, timeout)
        } else if let Some(url) = &health_check.url {
            let client = client.ok_or_else(|| {
                std::io::Error::other("HTTP health check client was not initialized")
            })?;
            self.perform_health_check(client, url)
        } else {
            Err(std::io::Error::other(
                "health check requires either a command or a url",
            ))
        }
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

    /// Executes a single command-based health check and evaluates the exit status.
    fn perform_command_health_check(
        &self,
        service_name: &str,
        command: &str,
        timeout: Duration,
    ) -> Result<bool, std::io::Error> {
        let mut child = Command::new(DEFAULT_SHELL);
        child.arg(SHELL_COMMAND_FLAG).arg(command);
        child.current_dir(&self.project_root);
        child.env("SYSG_SERVICE_NAME", service_name);
        child.stdout(Stdio::null());
        child.stderr(Stdio::null());

        let mut child = child.spawn()?;
        match wait_with_timeout(&mut child, timeout)? {
            Some(status) => Ok(status.success()),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    format!(
                        "health check command timed out after {:?}: {}",
                        timeout, command
                    ),
                ))
            }
        }
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

    /// Returns a cloned service config with a single env var overridden for candidate startup.
    fn service_with_env_override(
        service: &ServiceConfig,
        key: &str,
        value: &str,
    ) -> ServiceConfig {
        let mut cloned = service.clone();
        let mut env_cfg = cloned.env.take().unwrap_or_default();
        let mut vars = env_cfg.vars.take().unwrap_or_default();
        vars.insert(key.to_string(), value.to_string());
        env_cfg.vars = Some(vars);
        cloned.env = Some(env_cfg);
        cloned
    }

    /// Replaces supported placeholders in blue/green health-check templates.
    fn resolve_blue_green_health_check(
        health_check: &HealthCheckConfig,
        service_name: &str,
        active_slot: &str,
        candidate_slot: &str,
    ) -> HealthCheckConfig {
        let render = |value: &str| {
            value
                .replace("{slot}", candidate_slot)
                .replace("{active_slot}", active_slot)
                .replace("{candidate_slot}", candidate_slot)
                .replace("{service_name}", service_name)
        };

        HealthCheckConfig {
            url: health_check.url.as_deref().map(render),
            command: health_check.command.as_deref().map(render),
            interval: health_check.interval.clone(),
            timeout: health_check.timeout.clone(),
            retries: health_check.retries,
        }
    }

    /// Resolves the persisted state path used to track the active blue/green slot.
    fn blue_green_state_path(
        &self,
        service_name: &str,
        blue_green: &BlueGreenDeploymentConfig,
    ) -> PathBuf {
        if let Some(raw_path) = &blue_green.state_path {
            let path = PathBuf::from(raw_path);
            if path.is_absolute() {
                path
            } else {
                self.project_root.join(path)
            }
        } else {
            runtime::state_dir().join(format!("blue_green_{}.json", service_name))
        }
    }

    /// Loads the active slot index from disk, defaulting to slot 0 when no state exists.
    fn read_blue_green_active_index(
        &self,
        service_name: &str,
        blue_green: &BlueGreenDeploymentConfig,
    ) -> Result<usize, ProcessManagerError> {
        let path = self.blue_green_state_path(service_name, blue_green);
        if !path.exists() {
            return Ok(0);
        }

        let content = fs::read_to_string(&path).map_err(|source| {
            ProcessManagerError::ConfigParseError(serde_yaml::Error::custom(format!(
                "Failed reading blue/green state '{}': {}",
                path.display(),
                source
            )))
        })?;

        let state: BlueGreenState = xml_from_str(&content).map_err(|source| {
            ProcessManagerError::ConfigParseError(serde_yaml::Error::custom(format!(
                "Failed parsing blue/green state '{}': {}",
                path.display(),
                source
            )))
        })?;

        if state.active_slot_index > 1 {
            return Err(Self::config_error(format!(
                "blue/green state for '{}' contains invalid slot index {}",
                service_name, state.active_slot_index
            )));
        }

        Ok(state.active_slot_index)
    }

    /// Persists the active slot index after a successful blue/green cutover.
    fn write_blue_green_active_index(
        &self,
        service_name: &str,
        blue_green: &BlueGreenDeploymentConfig,
        active_slot_index: usize,
    ) -> Result<(), ProcessManagerError> {
        let path = self.blue_green_state_path(service_name, blue_green);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content =
            xml_to_string(&BlueGreenState { active_slot_index }).map_err(|source| {
                ProcessManagerError::ConfigParseError(serde_yaml::Error::custom(
                    source.to_string(),
                ))
            })?;
        fs::write(path, content)?;
        Ok(())
    }

    /// Executes the configured traffic-switch command with slot placeholders and env vars populated.
    fn run_blue_green_switch_command(
        &self,
        service_name: &str,
        command: &str,
        active_slot: &str,
        candidate_slot: &str,
    ) -> Result<(), ProcessManagerError> {
        let rendered_command = command
            .replace("{active_slot}", active_slot)
            .replace("{candidate_slot}", candidate_slot)
            .replace("{service_name}", service_name);

        let mut cmd = Command::new(DEFAULT_SHELL);
        cmd.arg(SHELL_COMMAND_FLAG).arg(rendered_command);
        cmd.current_dir(&self.project_root);
        cmd.env("SYSG_ACTIVE_SLOT", active_slot);
        cmd.env("SYSG_CANDIDATE_SLOT", candidate_slot);
        cmd.env("SYSG_SERVICE_NAME", service_name);

        let output = cmd.output()?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source: std::io::Error::other(format!(
                    "blue/green switch command failed: {stderr}"
                )),
            })
        }
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
        completed_services: &HashSet<String>,
    ) -> Result<(), ProcessManagerError> {
        let mut failed = Vec::new();

        for service_name in services {
            if completed_services.contains(service_name) {
                continue;
            }

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
            let (pid, mut pgid) = {
                let guard = self.pid_file.lock()?;
                (
                    guard.pid_for(service_name).unwrap_or(child.id()),
                    guard.pgid_for(service_name).map(|id| id as libc::pid_t),
                )
            };
            if pgid.is_none() {
                pgid = Self::process_group_for_pid(pid);
            }

            Ok(Some(DetachedService { child, pid, pgid }))
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

        self.pid_file.lock()?.insert_with_group(
            service_name,
            detached.pid,
            detached.pgid,
        )?;

        info!("Restored original instance of '{service_name}' after restart failure.");

        Ok(())
    }

    /// Terminates the detached instance once the replacement is known healthy
    /// and waits for the old child handle long enough to reap it when
    /// possible.
    fn terminate_service(
        &self,
        service_name: &str,
        mut detached: DetachedService,
    ) -> Result<(), ProcessManagerError> {
        let pid = detached.pid;
        Self::terminate_process_tree(service_name, pid, detached.pgid)?;

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

    /// Starts a service on Unix and macOS using the shared startup path, then
    /// waits for the launch thread to report the initial PID registration
    /// result before performing readiness checks.
    #[cfg(not(target_os = "linux"))]
    pub fn start_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        if let Some(state) = self.start_service_common(name, service)? {
            return Ok(state);
        }

        let processes = Arc::clone(&self.processes);
        let service_config = service.clone();
        let service_name = name.to_string();
        let pid_file = Arc::clone(&self.pid_file);
        let detach_children = self.detach_children;
        let working_dir = self.project_root.clone();
        let pipe_stderr = self.pipe_stderr.load(Ordering::SeqCst);
        let log_settings = service.effective_logs(&self.config.logs);

        let handle = thread::spawn(move || {
            debug!("Starting service thread for '{service_name}'");

            match Daemon::launch_attached_service(
                &service_name,
                &service_config,
                working_dir.clone(),
                processes.clone(),
                detach_children,
                pipe_stderr,
                log_settings,
            ) {
                Ok((pid, pgid)) => {
                    let mut pid_guard = pid_file.lock()?;
                    pid_guard.insert_with_group(&service_name, pid, pgid)?;
                    Ok(pid)
                }
                Err(e) => {
                    error!("Failed to start service '{service_name}': {e}");
                    Err(e)
                }
            }
        });

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

    /// Starts a service on Linux using the shared startup path and keeps the
    /// launcher thread alive so `PR_SET_PDEATHSIG` remains tied to a live
    /// parent until cancellation.
    #[cfg(target_os = "linux")]
    pub fn start_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        if let Some(state) = self.start_service_common(name, service)? {
            return Ok(state);
        }

        let ctx = self.context();
        let log_settings = service.effective_logs(&self.config.logs);
        let launch_result = Self::launch_service_with_lifetime_thread(
            &ctx,
            name.to_string(),
            service.clone(),
            log_settings,
        );

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
                    ctx.cancel_service_thread(name);
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
                ctx.cancel_service_thread(name);
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

    /// Shared stop implementation that accepts explicit handles, making it
    /// reusable from helpers that already hold references to the daemon's
    /// shared state. It resolves both PID and process-group metadata before
    /// tearing down the process tree so leaked descendants can still be
    /// terminated when the root leader has already disappeared.
    fn stop_service_with_handles(
        service_name: &str,
        processes: &Arc<Mutex<HashMap<String, Child>>>,
        pid_file: &Arc<Mutex<PidFile>>,
        state_file: &Arc<Mutex<ServiceStateFile>>,
        config: &Arc<Config>,
    ) -> Result<(), ProcessManagerError> {
        let (pid, service_group_id) = {
            let mut processes_guard = processes.lock()?;
            let persisted_group = pid_file
                .lock()
                .ok()
                .and_then(|guard| guard.pgid_for(service_name))
                .map(|id| id as libc::pid_t);

            if let Some(child) = processes_guard.get_mut(service_name) {
                let process_id = child.id();
                let group_id =
                    persisted_group.or_else(|| Self::process_group_for_pid(process_id));
                (Some(process_id), group_id)
            } else {
                let guard = pid_file.lock()?;
                let stored_pid = guard.get(service_name);
                let mut group_id = persisted_group;

                if stored_pid.is_some() && group_id.is_none() {
                    group_id = stored_pid.and_then(Self::process_group_for_pid);
                }

                (stored_pid, group_id)
            }
        };

        if let Some(process_id) = pid {
            match Self::terminate_process_tree(service_name, process_id, service_group_id)
            {
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
        } else if let Some(group_id) = service_group_id {
            let result = unsafe { libc::killpg(group_id, libc::SIGTERM) };
            if result < 0 {
                let err = std::io::Error::last_os_error();
                if !matches!(err.raw_os_error(), Some(libc::ESRCH)) {
                    warn!(
                        "Failed to signal process group {group_id} for '{service_name}': {err}"
                    );
                }
            }
            thread::sleep(PROCESS_CHECK_INTERVAL);
            let result = unsafe { libc::killpg(group_id, libc::SIGKILL) };
            if result < 0 {
                let err = std::io::Error::last_os_error();
                if !matches!(err.raw_os_error(), Some(libc::ESRCH)) {
                    warn!(
                        "Failed to force-kill process group {group_id} for '{service_name}': {err}"
                    );
                }
            }
        }

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

        if result.is_ok()
            && let Some(service) = self.config.services.get(service_name)
        {
            Self::cleanup_config_owned_service_processes(
                service_name,
                service,
                &self.project_root,
            )?;
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
                } else if let Err(err) = guard.save() {
                    warn!(
                        "Failed to save PID file after removing dependent '{service}': {err}"
                    );
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
        let mut cleaned_services = HashSet::new();

        for service in services {
            if let Err(e) = self.stop_service(&service) {
                error!("Failed to stop service '{service}': {e}");
            }
            cleaned_services.insert(service);
        }

        for (service_name, service) in &self.config.services {
            if cleaned_services.contains(service_name) {
                continue;
            }
            if let Err(err) = Self::cleanup_config_owned_service_processes(
                service_name,
                service,
                &self.project_root,
            ) {
                error!(
                    "Failed to clean up stale config-owned process for '{service_name}': {err}"
                );
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
            let mut restarted_services: Vec<(String, Option<libc::pid_t>)> = Vec::new();
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
                    #[cfg(target_os = "linux")]
                    ctx.cancel_service_thread(&name);

                    let recorded_pgid = pid_file_guard.pgid_for(&name);

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
                        failed_services.push(name.clone());
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
                            restarted_services.push((name.clone(), recorded_pgid));
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
                        if let Err(e) = pid_file_guard.clear_pid(&name) {
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

            let mut reconciled = Self::reconcile_lost_services(&ctx);
            restarted_services.append(&mut reconciled);

            for (name, recorded_pgid) in restarted_services {
                Self::reap_orphaned_group_before_restart(&name, recorded_pgid);
                if let Some(service) = ctx.config.services.get(&name) {
                    Self::handle_restart(&name, service, ctx.clone());
                }
            }

            thread::sleep(Duration::from_secs(2));
        }

        debug!("Monitor loop terminating.");
    }

    /// Restarts services that died during startup and were reaped out of the
    /// process map before the monitor could observe the exit.
    ///
    /// A service that fails to become ready (e.g. a port conflict at boot) is
    /// removed from `processes` by the readiness probe, so `monitor_loop`'s
    /// `try_wait` sweep never sees it crash. This reconciler re-detects such
    /// units — configured, long-running, absent from the process map, not
    /// manually stopped or suppressed — and feeds them back into the restart
    /// path when their policy allows. A non-empty `restart_counts` entry acts as
    /// the in-flight guard so a restart is only triggered once per failure.
    fn reconcile_lost_services(
        ctx: &DaemonContext,
    ) -> Vec<(String, Option<libc::pid_t>)> {
        let mut to_restart = Vec::new();

        let tracked: HashSet<String> = match ctx.lock_processes() {
            Ok(guard) => guard.keys().cloned().collect(),
            Err(_) => return to_restart,
        };

        for (name, service) in &ctx.config.services {
            if tracked.contains(name) {
                continue;
            }
            if !Self::should_verify_service(service) {
                continue;
            }

            let policy = service.restart_policy.as_deref();
            if policy != Some("always") && policy != Some("on-failure") {
                continue;
            }

            let manually_stopped = ctx
                .lock_manual_stop_flags()
                .map(|guard| guard.contains(name))
                .unwrap_or(false);
            if manually_stopped {
                continue;
            }

            let suppressed = ctx
                .lock_restart_suppressed()
                .map(|guard| guard.contains(name))
                .unwrap_or(false);
            if suppressed {
                continue;
            }

            let in_flight = ctx
                .lock_restart_in_flight()
                .map(|guard| guard.contains(name))
                .unwrap_or(true);
            if in_flight {
                continue;
            }

            if let Ok(mut guard) = ctx.lock_restart_in_flight() {
                guard.insert(name.clone());
            }
            warn!(
                "Service '{name}' is not running and was not manually stopped; restarting per its restart policy."
            );
            to_restart.push((name.clone(), None));
        }

        to_restart
    }

    /// Handles restarting a service if its restart policy allows.
    fn handle_restart(name: &str, service: &ServiceConfig, ctx: DaemonContext) {
        let name = name.to_string();
        let service_clone = service.clone();
        let hooks = service.hooks.clone();
        let max_restarts = service.max_restarts;
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
                if let Ok(mut guard) = ctx.lock_restart_in_flight() {
                    guard.remove(&name);
                }
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
            let _in_flight = InFlightGuard::new(&ctx.restart_in_flight, name.clone());
            warn!("Restarting '{name}' after {backoff} seconds...");
            thread::sleep(Duration::from_secs(backoff));

            if ctx
                .restart_suppressed
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

            if ctx
                .manual_stop_flags
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

            let restart_result = Self::launch_service_for_supervision(
                &ctx,
                name.clone(),
                service_clone.clone(),
                service_clone.effective_logs(&ctx.config.logs),
            );

            match restart_result {
                Ok(pid) => {
                    match Self::wait_for_ready(
                        &name,
                        &ctx.config.project.id,
                        &ctx.processes,
                        &ctx.pid_file,
                    ) {
                        Ok(ServiceReadyState::Running) => {
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
                                && let Some(action) = hooks_cfg
                                    .action(HookStage::OnStart, HookOutcome::Success)
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
                                && let Some(action) = hooks_cfg
                                    .action(HookStage::OnRestart, HookOutcome::Success)
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
                                && let Ok(latest) =
                                    PidFile::reload(pid_file_guard.store())
                            {
                                *pid_file_guard = latest;
                            }
                        }
                        Ok(ServiceReadyState::CompletedSuccess) => {
                            #[cfg(target_os = "linux")]
                            ctx.cancel_service_thread(&name);

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
                                && let Some(action) = hooks_cfg
                                    .action(HookStage::OnStart, HookOutcome::Success)
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
                                && let Some(action) = hooks_cfg
                                    .action(HookStage::OnRestart, HookOutcome::Success)
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
                            #[cfg(target_os = "linux")]
                            ctx.cancel_service_thread(&name);

                            error!(
                                "Service '{name}' failed to become ready after restart: {err}"
                            );

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) = hooks_cfg
                                    .action(HookStage::OnStart, HookOutcome::Error)
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
                                && let Some(action) = hooks_cfg
                                    .action(HookStage::OnRestart, HookOutcome::Error)
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
        });
    }
}

/// Clears a service's `restart_in_flight` entry when the restart thread ends,
/// on every exit path including early returns.
struct InFlightGuard {
    set: Arc<Mutex<HashSet<String>>>,
    name: String,
}

impl InFlightGuard {
    fn new(set: &Arc<Mutex<HashSet<String>>>, name: String) -> Self {
        Self {
            set: Arc::clone(set),
            name,
        }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.set.lock() {
            guard.remove(&self.name);
        }
    }
}

impl Drop for Daemon {
    /// Tears down the monitor and cancels service threads only when this is the
    /// final clone. `Daemon` shares its runtime through `Arc`s, so dropping a
    /// transient clone must not shut down a daemon other clones still use.
    fn drop(&mut self) {
        if Arc::strong_count(&self.liveness) > 1 {
            return;
        }
        self.shutdown_monitor();
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
                Some(
                    deps.iter()
                        .map(|d| crate::config::DependsOn::from(*d))
                        .collect(),
                )
            },
            deployment: None,
            hooks: None,
            cron: None,
            skip: None,
            spawn: None,
            logs: None,
            project_scope: None,
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
            version: crate::config::Version::V1,
            project: crate::config::ProjectConfig::default(),
            services,
            project_dir: Some(dir.to_string_lossy().to_string()),
            env: None,
            metrics: crate::config::MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };
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
    fn restart_services_allows_successful_one_shot_without_restart_policy() {
        with_temp_home(|dir| {
            fs::write(dir.join("check.sh"), "exit 0\n").unwrap();
            fs::write(
                dir.join("app.sh"),
                "trap 'exit 0' TERM\nwhile true; do sleep 1; done\n",
            )
            .unwrap();

            let mut services = HashMap::new();
            services.insert("check".into(), make_service("sh check.sh", &[]));
            services.insert("app".into(), make_service("sh app.sh", &["check"]));

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();

            daemon.restart_services().unwrap();

            daemon.stop_services().ok();
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

    #[cfg(target_os = "linux")]
    #[test]
    fn automatic_restart_keeps_restarted_service_alive() {
        with_temp_home(|dir| {
            let restarted_pid_path = dir.join("restarted.pid");
            fs::write(
                dir.join("flaky.sh"),
                format!(
                    r#"
if [ ! -f first-run.done ]; then
  touch first-run.done
  sleep 0.3
  exit 1
fi
echo $$ > "{}"
sleep 30
"#,
                    restarted_pid_path.display()
                ),
            )
            .unwrap();

            let mut service = make_service("sh flaky.sh", &[]);
            service.restart_policy = Some("always".into());
            service.backoff = Some("0s".into());

            let mut services = HashMap::new();
            services.insert("flaky".into(), service);

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();

            let deadline = Instant::now() + Duration::from_secs(5);
            let restarted_pid = loop {
                if let Ok(contents) = fs::read_to_string(&restarted_pid_path)
                    && let Ok(pid) = contents.trim().parse::<u32>()
                {
                    break pid;
                }

                if Instant::now() >= deadline {
                    panic!("automatic restart did not write replacement PID");
                }

                thread::sleep(Duration::from_millis(50));
            };

            thread::sleep(Duration::from_secs(2));

            assert!(
                Daemon::signal_pid("flaky", restarted_pid, None).unwrap(),
                "automatic restart replacement process {restarted_pid} should still be alive"
            );

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
            let mut initial = PidFile {
                store: StateStore::for_project("test"),
                ..PidFile::default()
            };
            initial.insert("baseline", 1000).unwrap();
            let num_threads = 10;
            let mut handles = vec![];
            for i in 0..num_threads {
                let handle = thread::spawn(move || {
                    thread::sleep(Duration::from_micros(i as u64 * 100));
                    for retry in 0..3 {
                        match PidFile::load(StateStore::for_project("test")) {
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
            for i in 0..5 {
                let handle = thread::spawn(move || {
                    thread::sleep(Duration::from_millis(2));

                    for retry in 0..3 {
                        match PidFile::load(StateStore::for_project("test")) {
                            Ok(mut pid_file) => {
                                if i % 2 == 0 {
                                    let _ = pid_file.remove("baseline");
                                } else {
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
            for handle in handles {
                handle.join().unwrap();
            }
            let final_pid_file = PidFile::load(StateStore::for_project("test"))
                .expect("Failed to load final PID file");
            let mut missing = vec![];
            for i in 0..num_threads {
                let service_name = format!("cron_job_{}", i);
                if final_pid_file.get(&service_name).is_none() {
                    missing.push(service_name);
                }
            }
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
            daemon.stop_service("test_service").unwrap();
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
            services.insert(
                "stubborn_service".into(),
                make_service("sh -c 'trap \"\" TERM; sleep 10'", &[]),
            );

            let daemon = create_daemon(dir, services);
            daemon.start_services().unwrap();

            thread::sleep(Duration::from_millis(100));
            assert!(
                daemon
                    .processes
                    .lock()
                    .unwrap()
                    .contains_key("stubborn_service")
            );
            daemon.stop_service("stubborn_service").unwrap();
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
            let result = daemon.start_service("test_service", &service).unwrap();
            assert!(matches!(result, ServiceReadyState::CompletedSuccess));

            thread::sleep(Duration::from_millis(100));
            daemon.stop_service("test_service").unwrap();
            assert!(
                !daemon
                    .processes
                    .lock()
                    .unwrap()
                    .contains_key("test_service")
            );
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
            daemon.stop_service("test_service").unwrap();
            assert!(
                daemon
                    .manual_stop_flags
                    .lock()
                    .unwrap()
                    .contains("test_service")
            );
            assert!(
                daemon
                    .restart_suppressed
                    .lock()
                    .unwrap()
                    .contains("test_service")
            );

            thread::sleep(Duration::from_millis(200));
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
            daemon.stop_service("hooked_service").unwrap();

            thread::sleep(Duration::from_millis(100));
            let content = fs::read_to_string(&hook_log).unwrap_or_default();
            assert_eq!(content.matches("STOP_SUCCESS").count(), 1);
        });
    }

    #[test]
    fn terminate_process_tree_kills_all_descendants() {
        with_temp_home(|_| {
            let mut cmd = Command::new(DEFAULT_SHELL);
            cmd.arg("-c");
            cmd.arg("sh -c 'sleep 60' & sh -c 'sleep 60' & sleep 60");
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());

            let mut parent = cmd.spawn().unwrap();
            let parent_pid = parent.id();

            thread::sleep(Duration::from_millis(100));
            let descendants_before = Daemon::collect_descendants(parent_pid);
            assert!(
                !descendants_before.is_empty(),
                "Should have child processes"
            );
            match Daemon::terminate_process_tree("test", parent_pid, None) {
                Ok(_) => {
                    thread::sleep(Duration::from_millis(200));
                    for pid in descendants_before {
                        assert!(
                            !Daemon::signal_pid("test", pid, None).unwrap(),
                            "Child process {} should be terminated",
                            pid
                        );
                    }
                }
                Err(ProcessManagerError::ServiceStopError { source, .. })
                    if source.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => panic!("Unexpected error: {:?}", e),
            }
            let _ = parent.wait();
        });
    }
}
