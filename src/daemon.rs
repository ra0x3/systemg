//! Service management daemon.
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs::{self, File},
    io::{BufRead, BufReader, ErrorKind, Read},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    str::FromStr,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use fs2::FileExt;
use quick_xml::{de::from_str as xml_from_str, se::to_string as xml_to_string};
use regex::Regex;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize, de::Error as _};
use serde_yaml;
use sysinfo::{ProcessesToUpdate, System};
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
        PRE_START_TIMEOUT, PROCESS_CHECK_INTERVAL, PROCESS_READY_CHECKS,
        SERVICE_POLL_INTERVAL, SERVICE_START_STABILITY, SERVICE_START_TIMEOUT,
        SESSION_SCOPED_ENV_VARS, SHELL_COMMAND_FLAG, STOP_VERIFY_TIMEOUT,
    },
    error::{PidFileError, ProcessManagerError, ServiceStateError},
    logs::{resolve_log_path, spawn_managed_service_log_writers},
    opslot::OpSlot,
    runtime,
    spawn::SpawnedExit,
    state_store::StateStore,
    upgrade::{HandoffDaemonState, HandoffProcess},
};

/// Capacity of the one-result health-check worker channel.
const HEALTH_RESULT_CAPACITY: usize = 1;
/// Delay before retrying monitor state after a lock failure.
const MONITOR_RETRY_DELAY: Duration = Duration::from_secs(2);
/// Delay used when a service does not declare restart backoff.
const DEFAULT_RESTART_BACKOFF: Duration = Duration::from_secs(5);
/// Thread name for service launch workers.
const SERVICE_LAUNCH_THREAD: &str = "sysg-service-launch";
/// Thread name for foreground stderr forwarding.
const SERVICE_STDERR_THREAD: &str = "sysg-service-stderr";
/// Thread name for captured stdout readers.
const OUTPUT_STDOUT_THREAD: &str = "sysg-output-stdout";
/// Thread name for captured stderr readers.
const OUTPUT_STDERR_THREAD: &str = "sysg-output-stderr";
/// Maximum pre-start output lines retained for failure diagnostics.
const PRE_START_TAIL_LINES: usize = 12;
/// Poll interval while waiting for bounded helper commands to exit.
const COMMAND_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Maximum bytes retained from each helper-command output stream.
const COMMAND_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
/// Read buffer size used while draining helper-command output streams.
const COMMAND_OUTPUT_READ_CHUNK_BYTES: usize = 8 * 1024;
/// Zero-based field containing process start time after the executable name in
/// `/proc/<pid>/stat`.
#[cfg(target_os = "linux")]
const LINUX_PROC_START_TIME_INDEX: usize = 19;
/// Canonical operating-system text for a port collision.
const PORT_IN_USE_TEXT: &str = "address already in use";
/// Compact errno name used by some runtimes for a port collision.
const ADDR_IN_USE_TEXT: &str = "addrinuse";
/// macOS/BSD errno text for a port collision.
const MACOS_PORT_IN_USE_TEXT: &str = "os error 48";
/// Linux errno text for a port collision.
const LINUX_PORT_IN_USE_TEXT: &str = "os error 98";
/// Output token that introduces an explicit port number.
const PORT_TOKEN: &str = "port";
/// Output token used when binding a listening socket.
const BIND_TOKEN: &str = "bind";
/// Output token used when opening a listening socket.
const LISTEN_TOKEN: &str = "listen";

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

#[derive(Debug, Serialize, Deserialize, Clone)]
struct StartEntry {
    name: String,
    started: u64,
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
    /// Service name -> process start time.
    #[serde(default, rename = "service_starts")]
    #[serde(
        serialize_with = "serialize_starts",
        deserialize_with = "deserialize_starts"
    )]
    service_starts: HashMap<String, u64>,
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

fn serialize_starts<S>(map: &HashMap<String, u64>, s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(map.len()))?;
    for (name, started) in map {
        seq.serialize_element(&StartEntry {
            name: name.clone(),
            started: *started,
        })?;
    }
    seq.end()
}

fn deserialize_starts<'de, D>(d: D) -> Result<HashMap<String, u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries: Vec<StartEntry> = Vec::deserialize(d)?;
    Ok(entries
        .into_iter()
        .map(|entry| (entry.name, entry.started))
        .collect())
}

/// Returns the kernel start-time identity for a Linux process.
#[cfg(target_os = "linux")]
pub(crate) fn process_start_time(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    stat[close + 1..]
        .split_whitespace()
        .nth(LINUX_PROC_START_TIME_INDEX)?
        .parse()
        .ok()
}

/// Returns the kernel start-time identity for a macOS process.
#[cfg(target_os = "macos")]
pub(crate) fn process_start_time(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut libc::proc_bsdinfo as *mut libc::c_void,
            size,
        )
    };
    (read == size).then(|| {
        info.pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec)
    })
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

    fn start_for(&self, service: &str) -> Option<u64> {
        self.service_starts.get(service).copied()
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
        let started = process_start_time(pid);
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

        self.services.insert(service.to_string(), pid);
        if let Some(group) = pgid {
            self.service_groups.insert(service.to_string(), group);
        }
        if let Some(started) = started {
            self.service_starts.insert(service.to_string(), started);
        } else {
            self.service_starts.remove(service);
        }

        self.write_at(&path)
    }

    /// Atomically clears a service PID while preserving group ownership metadata.
    pub fn clear_pid(&mut self, service: &str) -> Result<(), PidFileError> {
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

        if self.services.remove(service).is_none() {
            return Err(PidFileError::ServiceNotFound);
        }

        self.write_at(&path)
    }

    /// Clears a service PID only when it still names the supplied process.
    pub(crate) fn clear_pid_if_matches(
        &mut self,
        service: &str,
        pid: u32,
    ) -> Result<bool, PidFileError> {
        let _lock = self.acquire_lock()?;
        let path = self.path();
        self.reload_into(&path)?;
        if self.services.get(service).copied() != Some(pid) {
            return Ok(false);
        }
        self.services.remove(service);
        self.write_at(&path)?;
        Ok(true)
    }

    /// Atomically removes service.
    pub fn remove(&mut self, service: &str) -> Result<(), PidFileError> {
        let _lock = self.acquire_lock()?;

        let path = self.path();
        self.reload_into(&path)?;

        let removed_pid = self.services.remove(service);
        let removed_group = self.service_groups.get(service).copied();
        let known = removed_pid.is_some()
            || removed_group.is_some()
            || self.service_starts.contains_key(service);
        if !known {
            return Err(PidFileError::ServiceNotFound);
        }

        if let Some(root_pid) = removed_pid.or_else(|| {
            removed_group
                .filter(|pgid| *pgid > 0)
                .map(|pgid| pgid as u32)
        }) {
            if self.parent_map.contains_key(&root_pid)
                || self.children_map.contains_key(&root_pid)
                || self.spawn_metadata.contains_key(&root_pid)
            {
                self.remove_spawn_subtree_in_memory(root_pid);
            }

            if let Some(children) = self.children_map.remove(&root_pid) {
                for child in children {
                    self.remove_spawn_subtree_in_memory(child);
                }
            }

            let stale_roots: Vec<u32> = self
                .spawn_metadata
                .values()
                .filter(|meta| meta.parent_pid == root_pid)
                .map(|meta| meta.pid)
                .collect();
            for stale_pid in stale_roots {
                self.remove_spawn_subtree_in_memory(stale_pid);
            }
        }

        let _ = self.service_groups.remove(service);
        let _ = self.service_starts.remove(service);

        if self.services.is_empty() && self.service_groups.is_empty() {
            self.parent_map.clear();
            self.children_map.clear();
            self.spawn_depth.clear();
            self.spawn_metadata.clear();
        }

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
            service_starts: HashMap::new(),
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
            service_starts: HashMap::new(),
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
    cancel: Option<(&AtomicU64, &AtomicBool)>,
) {
    let hook_label = format!("{}.{}", stage.as_ref(), outcome.as_ref());
    debug!(
        "Running {} hook for '{}': `{}`",
        hook_label, service_name, action.command
    );

    let mut cmd = Command::new(DEFAULT_SHELL);
    cmd.arg(SHELL_COMMAND_FLAG).arg(&action.command);
    cmd.current_dir(project_root);

    for (key, value) in collect_service_env(env, project_root, service_name) {
        cmd.env(key, value);
    }

    let timeout = match action.timeout.as_deref() {
        Some(raw_timeout) => match Daemon::parse_duration(raw_timeout) {
            Ok(duration) => duration,
            Err(err) => {
                error!(
                    "Invalid timeout '{}' for hook {} on '{}': {}",
                    raw_timeout, hook_label, service_name, err
                );
                command_timeout()
            }
        },
        None => command_timeout(),
    };

    if cancel.is_some_and(|(_, cancelled)| cancelled.load(Ordering::SeqCst)) {
        return;
    }

    match spawn_session(&mut cmd) {
        Ok(mut child) => {
            let epoch = cancel.map(|(current, cancelled)| {
                (current, current.load(Ordering::SeqCst), cancelled)
            });
            let wait_result = wait_with_epoch(&mut child, timeout, epoch);

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
                    warn!(
                        "{} hook for '{}' was cancelled or timed out after {:?}. Terminating hook process.",
                        hook_label, service_name, timeout
                    );
                    let pid = child.id();
                    let _ = Daemon::terminate_process_tree(
                        service_name,
                        pid,
                        Some(pid as libc::pid_t),
                    );
                    let _ = child.wait();
                }
                Err(err) => {
                    let pid = child.id();
                    let _ = Daemon::terminate_process_tree(
                        service_name,
                        pid,
                        Some(pid as libc::pid_t),
                    );
                    let _ = child.wait();
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
/// Whether captured service output shows a port-bind collision. Matches the OS
/// message plus the errno spellings that differ by platform (48 on macOS/BSD,
/// 98 on Linux), so the classification holds wherever sysg runs.
pub fn output_indicates_port_conflict(lines: &[String]) -> bool {
    let lower = lines.join("\n").to_ascii_lowercase();
    lower.contains(PORT_IN_USE_TEXT)
        || lower.contains(ADDR_IN_USE_TEXT)
        || lower.contains(MACOS_PORT_IN_USE_TEXT)
        || lower.contains(LINUX_PORT_IN_USE_TEXT)
}

/// Parses a nonzero TCP/UDP port while rejecting values outside `u16`.
fn parse_port(value: &str) -> Option<u16> {
    let port = value.parse::<u16>().ok()?;
    (port != 0).then_some(port)
}

/// Best-effort port number from captured output, so the diagnostic can name the
/// contested port. Returns `None` when nothing port-shaped is present — the
/// diagnostic still stands without it.
fn port_from_output(lines: &[String]) -> Option<u16> {
    let pattern = Regex::new(
        r#"(?i)(?:\bport(?:\s+|=)|(?:^|[\s("'])(?:(?:localhost|\d{1,3}(?:\.\d{1,3}){3}|\[[0-9a-f:]+\]))?:)([0-9]{1,5})\b"#,
    )
    .expect("valid port pattern");
    for line in lines.iter().rev() {
        let lower = line.to_ascii_lowercase();
        let mentions_port = lower
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|word| word == PORT_TOKEN);
        if !lower.contains(BIND_TOKEN) && !lower.contains(LISTEN_TOKEN) && !mentions_port
        {
            continue;
        }
        let Some(captures) = pattern.captures(line) else {
            continue;
        };
        if let Some(port) = parse_port(&captures[1]) {
            return Some(port);
        }
    }
    None
}

fn port_from_command(command: Option<&str>) -> Option<u16> {
    let command = command?;
    let pattern = Regex::new(
        r#"(?ix)(?:--?port(?:\s+|=)|\bhttp\.server\s+|(?:localhost|\d{1,3}(?:\.\d{1,3}){3}|\[[0-9a-f:]+\]):)([0-9]{1,5})\b"#,
    )
    .expect("valid command port pattern");
    let captures = pattern.captures(command)?;
    parse_port(&captures[1])
}

fn wait_with_epoch(
    child: &mut Child,
    timeout: Duration,
    epoch: Option<(&AtomicU64, u64, &AtomicBool)>,
) -> std::io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;

    loop {
        if epoch.is_some_and(|(current, expected, cancelled)| {
            cancelled.load(Ordering::SeqCst) || current.load(Ordering::SeqCst) != expected
        }) {
            return Ok(None);
        }
        match child.try_wait()? {
            Some(status) => return Ok(Some(status)),
            None => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                thread::sleep(COMMAND_WAIT_POLL_INTERVAL);
            }
        }
    }
}

fn spawn_session(command: &mut Command) -> std::io::Result<Child> {
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command.spawn()
}

fn command_timeout() -> Duration {
    std::env::var("SYSG_PRE_START_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(PRE_START_TIMEOUT)
}

/// Drains an output stream while retaining only its most recent bounded tail.
fn read_bounded_output(reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut reader = BufReader::new(reader);
    let mut output = VecDeque::with_capacity(COMMAND_OUTPUT_LIMIT_BYTES);
    let mut chunk = [0_u8; COMMAND_OUTPUT_READ_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        output.extend(&chunk[..read]);
        let overflow = output.len().saturating_sub(COMMAND_OUTPUT_LIMIT_BYTES);
        output.drain(..overflow);
    }
    Ok(output.into_iter().collect())
}

fn output_with_timeout(
    command: &mut Command,
    timeout: Duration,
    label: &str,
    epoch: Option<(&AtomicU64, u64, &AtomicBool)>,
) -> std::io::Result<std::process::Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn_session(command)?;
    let pid = child.id();
    let stdout = child.stdout.take().ok_or_else(|| {
        std::io::Error::other(format!("failed to capture stdout for {label}"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        std::io::Error::other(format!("failed to capture stderr for {label}"))
    })?;
    let stdout_thread = match thread::Builder::new()
        .name(OUTPUT_STDOUT_THREAD.into())
        .spawn(move || read_bounded_output(stdout))
    {
        Ok(handle) => handle,
        Err(err) => {
            let _ = Daemon::terminate_process_tree(label, pid, Some(pid as libc::pid_t));
            let _ = child.wait();
            return Err(err);
        }
    };
    let stderr_thread = match thread::Builder::new()
        .name(OUTPUT_STDERR_THREAD.into())
        .spawn(move || read_bounded_output(stderr))
    {
        Ok(handle) => handle,
        Err(err) => {
            let _ = Daemon::terminate_process_tree(label, pid, Some(pid as libc::pid_t));
            let _ = child.wait();
            let _ = stdout_thread.join();
            return Err(err);
        }
    };

    let status = match wait_with_epoch(&mut child, timeout, epoch) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = Daemon::terminate_process_tree(label, pid, Some(pid as libc::pid_t));
            let _ = child.wait();
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                format!("{label} timed out after {}s", timeout.as_secs()),
            ));
        }
        Err(err) => {
            let _ = Daemon::terminate_process_tree(label, pid, Some(pid as libc::pid_t));
            let _ = child.wait();
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(err);
        }
    };
    let stdout = stdout_thread.join().map_err(|_| {
        std::io::Error::other(format!("stdout reader failed for {label}"))
    })??;
    let stderr = stderr_thread.join().map_err(|_| {
        std::io::Error::other(format!("stderr reader failed for {label}"))
    })??;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Snapshot of the currently observed state for a managed service during readiness probing.
#[derive(Debug)]
enum ServiceProbe {
    NotStarted,
    Running,
    Exited(ExitStatus),
}

/// Classifies why a single health check probe failed, so the final failure can
/// carry the right diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthProbeOutcome {
    /// The check ran and reported non-success (HTTP non-2xx or non-zero exit).
    Unhealthy,
    /// The probe never reached the check (connection refused, DNS fail, spawn error).
    Unreachable,
    /// The probe exceeded the per-attempt timeout.
    Timeout,
}

/// Indicates when a service is considered ready for dependents or has already completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceReadyState {
    /// Service is running and ready for dependents.
    Running,
    /// Service completed successfully (for oneshot/cron services).
    CompletedSuccess,
}

/// Waitable service process retained either as an originating `Child` or as a
/// PID adopted by the same supervisor process after `exec`.
#[derive(Debug)]
struct ManagedChild {
    /// Stable process identifier.
    pid: u32,
    /// Standard-library handle available before the first supervisor re-exec.
    child: Option<Child>,
}

impl ManagedChild {
    /// Reconstructs a waitable handle after same-PID supervisor re-execution.
    fn adopt(pid: u32) -> Self {
        Self { pid, child: None }
    }

    /// Returns the managed process identifier.
    fn id(&self) -> u32 {
        self.pid
    }

    /// Checks for process completion without blocking.
    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if let Some(child) = self.child.as_mut() {
            return child.try_wait();
        }
        self.wait_with_flags(libc::WNOHANG)
    }

    /// Waits until the managed process exits and returns its status.
    fn wait(&mut self) -> std::io::Result<ExitStatus> {
        if let Some(child) = self.child.as_mut() {
            return child.wait();
        }
        self.wait_with_flags(0)?.ok_or_else(|| {
            std::io::Error::other("blocking wait returned without a process status")
        })
    }

    /// Calls `waitpid` for an adopted child using the supplied flags.
    fn wait_with_flags(&self, flags: libc::c_int) -> std::io::Result<Option<ExitStatus>> {
        let mut status = 0;
        let waited =
            unsafe { libc::waitpid(self.pid as libc::pid_t, &mut status, flags) };
        if waited == 0 {
            Ok(None)
        } else if waited == self.pid as libc::pid_t {
            Ok(Some(ExitStatus::from_raw(status)))
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

impl From<Child> for ManagedChild {
    /// Wraps a newly spawned service process.
    fn from(child: Child) -> Self {
        Self {
            pid: child.id(),
            child: Some(child),
        }
    }
}

/// Represents an existing service instance that has been temporarily detached while a
/// replacement is brought up during a rolling restart.
struct DetachedService {
    /// Child handle retained while the replacement is prepared.
    child: ManagedChild,
    /// Process ID of the detached service leader.
    pid: u32,
    /// Process group containing the detached service tree.
    pgid: Option<libc::pid_t>,
}

/// Removes a service from the replacement set when its deployment finishes.
struct ReplacementGuard {
    /// Shared set of services undergoing explicit replacement.
    services: Arc<Mutex<HashSet<String>>>,
    /// Service removed when the guard is dropped.
    name: String,
}

impl Drop for ReplacementGuard {
    /// Releases the service's replacement claim.
    fn drop(&mut self) {
        self.services
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.name);
    }
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
    /// Held mutex guard.
    guard: std::sync::MutexGuard<'a, T>,
    /// Lock class removed from thread-local tracking on drop.
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
    let violation = HELD_LOCKS.with(|held| {
        let held_locks = held.borrow();
        held_locks
            .iter()
            .find(|existing| lock_type <= **existing)
            .copied()
    });
    if let Some(existing) = violation {
        return Err(ProcessManagerError::MutexPoisonError(format!(
            "lock order violation: {lock_type:?} ({}) after {existing:?} ({})",
            lock_type.priority(),
            existing.priority()
        )));
    }

    let guard = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

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
    processes: Arc<Mutex<HashMap<String, ManagedChild>>>,
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
    /// Dependents stopped as casualties of a crashed dependency, mapped to the
    /// set of dependencies that felled them. A dependent is revived only once
    /// every felling dependency has recovered.
    stopped_for_dependency: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    /// Flag indicating whether the monitoring loop should remain active.
    running: Arc<AtomicBool>,
    /// Weak access to the monitor handle without creating a thread ownership cycle.
    monitor_handle: Weak<Mutex<Option<thread::JoinHandle<()>>>>,
    /// Pipe stderr to stdout.
    pipe_stderr: Arc<AtomicBool>,
    /// Active boot generation shared with cancellation-aware lifecycle gates.
    boot_epoch: Arc<AtomicU64>,
    /// Whether the active project boot was explicitly cancelled.
    boot_cancelled: Arc<AtomicBool>,
    /// Weak runtime ownership used by temporary daemon views in restart workers.
    liveness: Weak<()>,
    /// Operation reporter used by pre-start and health-check waits.
    op_slot: OpSlot,
    /// Services currently being replaced through an explicit deployment strategy.
    replacements: Arc<Mutex<HashSet<String>>>,
    /// Cancellation tokens for Linux service threads (service_name -> cancel_token)
    #[cfg(target_os = "linux")]
    thread_cancellation_tokens: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl DaemonContext {
    /// Acquires the processes lock with ordering enforcement.
    fn lock_processes(
        &self,
    ) -> Result<OrderedLockGuard<'_, HashMap<String, ManagedChild>>, ProcessManagerError>
    {
        acquire_lock(&self.processes, DaemonLock::Processes)
    }

    /// Acquires the pid_file lock with ordering enforcement.
    fn lock_pid_file(
        &self,
    ) -> Result<OrderedLockGuard<'_, PidFile>, ProcessManagerError> {
        acquire_lock(&self.pid_file, DaemonLock::PidFile)
    }

    /// Acquires the state_file lock with ordering enforcement.
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

    /// Acquires the stopped_for_dependency lock with ordering enforcement.
    fn lock_stopped_for_dependency(
        &self,
    ) -> Result<OrderedLockGuard<'_, HashMap<String, HashSet<String>>>, ProcessManagerError>
    {
        acquire_lock(
            &self.stopped_for_dependency,
            DaemonLock::StoppedForDependency,
        )
    }

    /// Creates a cancellation token for a Linux service thread.
    #[cfg(target_os = "linux")]
    fn create_cancellation_token(&self, service_name: &str) -> Arc<AtomicBool> {
        let token = Arc::new(AtomicBool::new(false));
        let mut tokens = self
            .thread_cancellation_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tokens.insert(service_name.to_string(), Arc::clone(&token));
        token
    }

    /// Signals a Linux service thread to stop.
    #[cfg(target_os = "linux")]
    fn cancel_service_thread(&self, service_name: &str) {
        let mut tokens = self
            .thread_cancellation_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(token) = tokens.remove(service_name) {
            token.store(true, Ordering::SeqCst);
        }
    }

    /// Cleans up all Linux service thread cancellation tokens.
    #[cfg(target_os = "linux")]
    fn cancel_all_service_threads(&self) {
        let mut tokens = self
            .thread_cancellation_tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, token) in tokens.drain() {
            token.store(true, Ordering::SeqCst);
        }
    }
}

/// Service manager daemon.
#[derive(Clone)]
pub struct Daemon {
    /// Running processes.
    processes: Arc<Mutex<HashMap<String, ManagedChild>>>,
    /// Service config.
    config: Arc<std::sync::Mutex<Arc<Config>>>,
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
    /// Dependents stopped as casualties of a crashed dependency.
    stopped_for_dependency: Arc<Mutex<HashMap<String, HashSet<String>>>>,
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
    boot_epoch: Arc<AtomicU64>,
    boot_cancelled: Arc<AtomicBool>,
    replacements: Arc<Mutex<HashSet<String>>>,
}

impl Daemon {
    /// Returns the current config snapshot.
    fn cfg(&self) -> Arc<Config> {
        Arc::clone(
            &self
                .config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Applies one service's resolved environment to a child command.
    fn set_service_env(&self, command: &mut Command, service_name: &str) {
        let config = self.cfg();
        let service = config.services.get(service_name);
        let env = service.and_then(|service| service.env.as_ref());
        for (key, value) in
            collect_service_env(&env.cloned(), &self.project_root, service_name)
        {
            command.env(key, value);
        }
        let strip = env.map(EnvConfig::vars_to_strip).unwrap_or_else(|| {
            SESSION_SCOPED_ENV_VARS
                .iter()
                .map(|v| v.to_string())
                .collect()
        });
        for key in strip {
            command.env_remove(key);
        }
        command.env("SYSG_SERVICE_NAME", service_name);
    }

    /// Creates context snapshot.
    fn context(&self) -> DaemonContext {
        DaemonContext {
            processes: Arc::clone(&self.processes),
            pid_file: Arc::clone(&self.pid_file),
            state_file: Arc::clone(&self.state_file),
            config: Arc::clone(&self.cfg()),
            project_root: self.project_root.clone(),
            detach_children: self.detach_children,
            restart_counts: Arc::clone(&self.restart_counts),
            manual_stop_flags: Arc::clone(&self.manual_stop_flags),
            restart_suppressed: Arc::clone(&self.restart_suppressed),
            restart_in_flight: Arc::clone(&self.restart_in_flight),
            stopped_for_dependency: Arc::clone(&self.stopped_for_dependency),
            running: Arc::clone(&self.running),
            monitor_handle: Arc::downgrade(&self.monitor_handle),
            pipe_stderr: Arc::clone(&self.pipe_stderr),
            boot_epoch: Arc::clone(&self.boot_epoch),
            boot_cancelled: Arc::clone(&self.boot_cancelled),
            liveness: Arc::downgrade(&self.liveness),
            op_slot: self.op_slot.clone(),
            replacements: Arc::clone(&self.replacements),
            #[cfg(target_os = "linux")]
            thread_cancellation_tokens: Arc::clone(&self.thread_cancellation_tokens),
        }
    }

    /// Reconstructs a non-owning daemon view for a monitor restart worker.
    ///
    /// Returns `None` after the owning daemon has been dropped. Weak ownership
    /// avoids making the monitor thread keep its own join handle alive forever.
    fn from_context(ctx: &DaemonContext) -> Option<Self> {
        Some(Self {
            processes: Arc::clone(&ctx.processes),
            config: Arc::new(std::sync::Mutex::new(Arc::clone(&ctx.config))),
            pid_file: Arc::clone(&ctx.pid_file),
            state_file: Arc::clone(&ctx.state_file),
            detach_children: ctx.detach_children,
            project_root: ctx.project_root.clone(),
            running: Arc::clone(&ctx.running),
            monitor_handle: ctx.monitor_handle.upgrade()?,
            restart_counts: Arc::clone(&ctx.restart_counts),
            manual_stop_flags: Arc::clone(&ctx.manual_stop_flags),
            restart_suppressed: Arc::clone(&ctx.restart_suppressed),
            restart_in_flight: Arc::clone(&ctx.restart_in_flight),
            stopped_for_dependency: Arc::clone(&ctx.stopped_for_dependency),
            #[cfg(target_os = "linux")]
            thread_cancellation_tokens: Arc::clone(&ctx.thread_cancellation_tokens),
            pipe_stderr: Arc::clone(&ctx.pipe_stderr),
            liveness: ctx.liveness.upgrade()?,
            op_slot: ctx.op_slot.clone(),
            boot_epoch: Arc::clone(&ctx.boot_epoch),
            boot_cancelled: Arc::clone(&ctx.boot_cancelled),
            replacements: Arc::clone(&ctx.replacements),
        })
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
                && target_pgid > 0
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
                && target_pgid > 0
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
        if pgid <= 0 || pgid == supervisor_pgid {
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

    /// Persists service state to the state file using the service's composite
    /// state key (`{version}:{project}:{service}`) as the key. This is the
    /// low-level function that writes state directly to disk.
    fn persist_service_state(
        config: &Arc<Config>,
        state_file: &Arc<Mutex<ServiceStateFile>>,
        service_name: &str,
        status: ServiceLifecycleStatus,
        pid: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), ProcessManagerError> {
        if config.services.contains_key(service_name) {
            let key = config.state_key(service_name);
            let mut state_guard = state_file.lock()?;
            state_guard.set(&key, status, pid, exit_code, signal)?;
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
            config: Arc::new(std::sync::Mutex::new(Arc::new(config))),
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
            stopped_for_dependency: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(target_os = "linux")]
            thread_cancellation_tokens: Arc::new(Mutex::new(HashMap::new())),
            pipe_stderr: Arc::new(AtomicBool::new(false)),
            op_slot: OpSlot::new(),
            liveness: Arc::new(()),
            boot_epoch: Arc::new(AtomicU64::new(0)),
            boot_cancelled: Arc::new(AtomicBool::new(false)),
            replacements: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Points the daemon at the supervisor's shared operation slot so blocking
    /// boot steps report what they are waiting on.
    pub fn set_op_slot(&mut self, op_slot: OpSlot) {
        self.op_slot = op_slot;
    }

    /// Starts a new project boot epoch and returns its cancellation token.
    pub(crate) fn begin_boot(&self) -> u64 {
        self.boot_cancelled.store(false, Ordering::SeqCst);
        self.boot_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Cancels the current boot epoch and wakes cancellation-aware waits.
    pub(crate) fn cancel_boot(&self) {
        self.boot_cancelled.store(true, Ordering::SeqCst);
        self.boot_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Returns whether `epoch` still owns the active project boot.
    pub(crate) fn boot_active(&self, epoch: u64) -> bool {
        self.boot_epoch.load(Ordering::SeqCst) == epoch
    }

    /// Returns whether the current project boot was explicitly cancelled.
    pub(crate) fn boot_cancelled(&self) -> bool {
        self.boot_cancelled.load(Ordering::SeqCst)
    }

    /// Builds the lifecycle error returned when project boot is cancelled.
    fn interrupted(service: &str) -> ProcessManagerError {
        ProcessManagerError::ServiceStartError {
            service: service.to_string(),
            source: std::io::Error::new(
                ErrorKind::Interrupted,
                "project start was cancelled",
            ),
        }
    }

    /// Claims replacement ownership for a service until the returned guard drops.
    fn replacement(&self, name: &str) -> ReplacementGuard {
        self.replacements
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.to_string());
        ReplacementGuard {
            services: Arc::clone(&self.replacements),
            name: name.to_string(),
        }
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
        self.cfg()
    }

    /// Returns whether any configured non-cron service still needs to start.
    pub(crate) fn needs_start(&self) -> bool {
        let config = self.cfg();
        let pids = match self.pid_file.lock() {
            Ok(pids) => pids,
            Err(_) => return true,
        };
        let states = match self.state_file.lock() {
            Ok(states) => states,
            Err(_) => return true,
        };
        config.services.iter().any(|(name, service)| {
            if service.cron.is_some() {
                return false;
            }
            let running = pids.get(name).is_some_and(|pid| {
                pids.start_for(name).is_some_and(|started| {
                    Self::pid_is_alive(pid) && process_start_time(pid) == Some(started)
                })
            });
            if running {
                return false;
            }
            let status = states
                .get(&config.state_key(name))
                .map(|entry| entry.status);
            match status {
                Some(ServiceLifecycleStatus::Skipped) => {
                    matches!(service.skip, Some(SkipConfig::Command(_)))
                }
                Some(ServiceLifecycleStatus::ExitedSuccessfully) => {
                    service.restarts_after_success()
                }
                _ => true,
            }
        })
    }

    /// Swaps the daemon's live config for a live reconcile.
    pub fn set_config(&self, config: Config) {
        *self
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(config);
    }

    /// Returns a handle to the shared PID file so callers can inspect process IDs.
    pub fn pid_file_handle(&self) -> Arc<Mutex<PidFile>> {
        Arc::clone(&self.pid_file)
    }

    /// Captures verified kernel identities for every process currently owned by
    /// this daemon without reaping or otherwise disturbing them.
    pub(crate) fn handoff_processes(
        &self,
    ) -> Result<Vec<HandoffProcess>, ProcessManagerError> {
        let processes = self.processes.lock()?;
        let pids = self.pid_file.lock()?;
        let mut snapshot = Vec::with_capacity(processes.len());
        for (service, child) in processes.iter() {
            let pid = child.id();
            if pids.get(service) != Some(pid) {
                return Err(Self::handoff_identity_error(
                    service,
                    format!("PID file does not own process {pid}"),
                ));
            }
            let started = pids.start_for(service).ok_or_else(|| {
                Self::handoff_identity_error(service, "process start identity is missing")
            })?;
            if !Self::pid_is_alive(pid) || process_start_time(pid) != Some(started) {
                return Err(Self::handoff_identity_error(
                    service,
                    format!("process {pid} is no longer the recorded instance"),
                ));
            }
            let pgid = pids.pgid_for(service).ok_or_else(|| {
                Self::handoff_identity_error(service, "process group identity is missing")
            })?;
            if Self::process_group_for_pid(pid) != Some(pgid as libc::pid_t) {
                return Err(Self::handoff_identity_error(
                    service,
                    format!("process {pid} no longer belongs to group {pgid}"),
                ));
            }
            snapshot.push(HandoffProcess {
                service: service.clone(),
                pid,
                pgid,
                started,
            });
        }
        for (service, pid) in pids.services() {
            if Self::pid_is_alive(*pid) && !processes.contains_key(service) {
                return Err(Self::handoff_identity_error(
                    service,
                    format!("live process {pid} has no waitable supervisor handle"),
                ));
            }
        }
        snapshot.sort_unstable_by(|left, right| left.service.cmp(&right.service));
        Ok(snapshot)
    }

    /// Reconstructs waitable service handles after same-PID supervisor re-exec,
    /// rejecting any process whose persisted kernel identity changed.
    pub(crate) fn adopt_handoff_processes(
        &self,
        expected: &[HandoffProcess],
    ) -> Result<(), ProcessManagerError> {
        let mut processes = self.processes.lock()?;
        if !processes.is_empty() {
            return Err(Self::handoff_identity_error(
                "supervisor",
                "process map was not empty before handoff adoption",
            ));
        }
        let pids = self.pid_file.lock()?;
        for process in expected {
            if !self.cfg().services.contains_key(&process.service) {
                return Err(Self::handoff_identity_error(
                    &process.service,
                    "service is absent from the handed-off manifest",
                ));
            }
            if pids.get(&process.service) != Some(process.pid)
                || pids.start_for(&process.service) != Some(process.started)
                || pids.pgid_for(&process.service) != Some(process.pgid)
                || !Self::pid_is_alive(process.pid)
                || process_start_time(process.pid) != Some(process.started)
                || Self::process_group_for_pid(process.pid)
                    != Some(process.pgid as libc::pid_t)
            {
                return Err(Self::handoff_identity_error(
                    &process.service,
                    format!(
                        "process {} changed before the replacement supervisor resumed",
                        process.pid
                    ),
                ));
            }
            if processes
                .insert(process.service.clone(), ManagedChild::adopt(process.pid))
                .is_some()
            {
                return Err(Self::handoff_identity_error(
                    &process.service,
                    "handoff contains the service more than once",
                ));
            }
        }
        Ok(())
    }

    /// Captures all daemon bookkeeping needed to continue supervising the same
    /// workload after re-exec.
    pub(crate) fn handoff_state(
        &self,
    ) -> Result<HandoffDaemonState, ProcessManagerError> {
        let replacements = self
            .replacements
            .lock()
            .map_err(ProcessManagerError::from)?;
        if let Some(service) = replacements.iter().next() {
            return Err(Self::handoff_identity_error(
                service,
                "a deployment replacement is still active",
            ));
        }
        drop(replacements);
        let in_flight = self
            .restart_in_flight
            .lock()
            .map_err(ProcessManagerError::from)?;
        if let Some(service) = in_flight.iter().next() {
            return Err(Self::handoff_identity_error(
                service,
                "an automatic restart is still active",
            ));
        }
        drop(in_flight);
        let mut manual_stops = self
            .manual_stop_flags
            .lock()
            .map_err(ProcessManagerError::from)?
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        manual_stops.sort_unstable();
        let mut restart_suppressed = self
            .restart_suppressed
            .lock()
            .map_err(ProcessManagerError::from)?
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        restart_suppressed.sort_unstable();
        let restart_counts = self
            .restart_counts
            .lock()
            .map_err(ProcessManagerError::from)?
            .iter()
            .map(|(service, count)| (service.clone(), *count))
            .collect::<BTreeMap<_, _>>();
        let stopped_for_dependency = self
            .stopped_for_dependency
            .lock()
            .map_err(ProcessManagerError::from)?
            .iter()
            .map(|(service, dependencies)| {
                let mut dependencies = dependencies.iter().cloned().collect::<Vec<_>>();
                dependencies.sort_unstable();
                (service.clone(), dependencies)
            })
            .collect::<BTreeMap<_, _>>();
        Ok(HandoffDaemonState {
            processes: self.handoff_processes()?,
            manual_stops,
            restart_suppressed,
            restart_counts,
            stopped_for_dependency,
        })
    }

    /// Restores daemon bookkeeping and waitable process ownership after re-exec.
    pub(crate) fn adopt_handoff_state(
        &self,
        state: &HandoffDaemonState,
    ) -> Result<(), ProcessManagerError> {
        self.adopt_handoff_processes(&state.processes)?;
        *self
            .manual_stop_flags
            .lock()
            .map_err(ProcessManagerError::from)? =
            state.manual_stops.iter().cloned().collect();
        *self
            .restart_suppressed
            .lock()
            .map_err(ProcessManagerError::from)? =
            state.restart_suppressed.iter().cloned().collect();
        *self
            .restart_counts
            .lock()
            .map_err(ProcessManagerError::from)? = state
            .restart_counts
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        *self
            .stopped_for_dependency
            .lock()
            .map_err(ProcessManagerError::from)? = state
            .stopped_for_dependency
            .iter()
            .map(|(service, dependencies)| {
                (service.clone(), dependencies.iter().cloned().collect())
            })
            .collect();
        Ok(())
    }

    /// Builds a process-management error for an unverifiable handoff identity.
    fn handoff_identity_error(
        service: &str,
        message: impl Into<String>,
    ) -> ProcessManagerError {
        ProcessManagerError::ServiceStartError {
            service: service.to_string(),
            source: std::io::Error::other(message.into()),
        }
    }

    /// The project state store this daemon's files are bound to.
    pub fn store(&self) -> StateStore {
        StateStore::for_project(&self.cfg().project.id)
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
        self.cfg().get_service_hash(service_name)
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
        let config = self.cfg();
        if !config.services.contains_key(service) {
            warn!("Service '{service}' not found in config, skipping state update");
            return Ok(());
        }
        Self::persist_service_state(
            &config,
            &self.state_file,
            service,
            status,
            pid,
            exit_code,
            signal,
        )
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
    #[allow(clippy::too_many_arguments)]
    fn launch_attached_service(
        project: &str,
        service_name: &str,
        service_config: &ServiceConfig,
        working_dir: PathBuf,
        processes: Arc<Mutex<HashMap<String, ManagedChild>>>,
        // Vestigial: every service now leads its own session unconditionally, so
        // this flag no longer changes spawn behaviour. Kept until the plumbing
        // is removed.
        _detach_children: bool,
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
                // Every service leads its own session. This detaches it from the
                // supervisor's session and thread lifecycle so tearing down one
                // service can never signal a sibling. setsid also makes the
                // process its own group leader, giving it a private process
                // group for targeted, scoped termination.
                //
                // We deliberately do NOT set PR_SET_PDEATHSIG: on Linux it fires
                // on parent-*thread* death, and the per-service launcher threads
                // come and go during stop/restart, which cascaded SIGTERM across
                // sibling services. Orphaned services (if the supervisor dies)
                // are recoverable — reconciled and reaped from the pid files on
                // restart — whereas a wrongly-killed sibling is not.
                if libc::setsid() < 0 {
                    let err = std::io::Error::last_os_error();
                    eprintln!("systemg pre_exec: setsid failed: {:?}", err);
                    return Err(err);
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

                let log_result = if pipe_stderr {
                    if let Some(err) = stderr {
                        use std::io::{self, BufRead, BufReader, Write};

                        let service_name_clone = service_name.to_string();
                        thread::Builder::new()
                            .name(SERVICE_STDERR_THREAD.into())
                            .spawn(move || {
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
                            })
                            .map(|_| ())
                    } else {
                        Ok(())
                    }
                } else {
                    spawn_managed_service_log_writers(
                        project,
                        service_name,
                        stdout,
                        stderr,
                        log_settings,
                    )
                };
                if let Err(source) = log_result {
                    let _ = Self::terminate_process_tree(
                        service_name,
                        pid,
                        Some(pid as libc::pid_t),
                    );
                    let _ = child.wait();
                    return Err(ProcessManagerError::ServiceStartError {
                        service: service_name.to_string(),
                        source,
                    });
                }

                processes
                    .lock()?
                    .insert(service_name.to_string(), child.into());

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
        let project_id = ctx.config.project.id.clone();
        let service_name_for_thread = service_name.clone();
        let service_name_for_cleanup = service_name.clone();

        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name(SERVICE_LAUNCH_THREAD.into())
            .spawn(move || {
            debug!("Starting service thread for '{service_name_for_thread}'");

            let launch_result = Daemon::launch_attached_service(
                &project_id,
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
            })
            .map_err(|source| ProcessManagerError::ServiceStartError {
                service: service_name_for_cleanup.clone(),
                source,
            })?;

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

    /// Common logic for service startup that's shared between Linux and non-Linux platforms.
    /// Returns Ok(Some(state)) if the service was skipped or completed immediately,
    /// Ok(None) if the service should continue with platform-specific startup.
    fn start_service_common(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<Option<ServiceReadyState>, ProcessManagerError> {
        if self.boot_cancelled() {
            return Err(Self::interrupted(name));
        }
        let config = self.cfg();
        match Self::probe_service_state_recording(
            name,
            &self.processes,
            &self.pid_file,
            Some((&self.state_file, &config)),
        )? {
            ServiceProbe::Running => {
                let pid = self.processes.lock()?.get(name).map(ManagedChild::id);
                if let Some(pid) = pid {
                    let pgid = Self::process_group_for_pid(pid);
                    self.pid_file.lock()?.insert_with_group(name, pid, pgid)?;
                    self.mark_running(name, pid)?;
                }
                return Ok(Some(ServiceReadyState::Running));
            }
            ServiceProbe::Exited(_) | ServiceProbe::NotStarted => {}
        }

        let replacing = self
            .replacements
            .lock()
            .map(|services| services.contains(name))
            .unwrap_or(false);
        if !replacing {
            let (pid, pgid, started) = {
                let pids = self.pid_file.lock()?;
                (pids.get(name), pids.pgid_for(name), pids.start_for(name))
            };
            if let Some(pid) = pid
                && Self::pid_is_alive(pid)
            {
                if started
                    .is_some_and(|expected| process_start_time(pid) == Some(expected))
                {
                    self.mark_running(name, pid)?;
                    return Ok(Some(ServiceReadyState::Running));
                }
                return Err(ProcessManagerError::ServiceStartError {
                    service: name.to_string(),
                    source: std::io::Error::other(format!(
                        "refusing to replace unverified live pid {pid}"
                    )),
                });
            }
            if pgid.is_some_and(Self::process_group_is_alive) {
                let Some(expected) = started else {
                    return Err(ProcessManagerError::ServiceStartError {
                        service: name.to_string(),
                        source: std::io::Error::other(
                            "refusing to replace an unverified live process group",
                        ),
                    });
                };
                let root = pid.unwrap_or(pgid.unwrap_or_default() as u32);
                if process_start_time(root).is_some_and(|actual| actual != expected) {
                    return Err(ProcessManagerError::ServiceStartError {
                        service: name.to_string(),
                        source: std::io::Error::other(
                            "refusing to replace a process group with reused ownership",
                        ),
                    });
                }
                Self::terminate_process_tree(
                    name,
                    root,
                    pgid.map(|value| value as libc::pid_t),
                )?;
            }
            if pid.is_some() || pgid.is_some() {
                let mut pids = self.pid_file.lock()?;
                if let Err(err) = pids.remove(name)
                    && !matches!(err, PidFileError::ServiceNotFound)
                {
                    return Err(err.into());
                }
            }
        }
        info!("Starting service: {name}");

        {
            let mut suppressed = self.restart_suppressed.lock()?;
            suppressed.remove(name);
        }
        {
            let mut stopped = self.manual_stop_flags.lock()?;
            stopped.remove(name);
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
                            return Err(err);
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
                self.op_slot.detail_for(
                    &self.cfg().project.id.clone(),
                    format!("running pre-start for '{name}'"),
                );
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
                let config = self.cfg();
                let dep_config = config.services.get(dep_name)?;
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

        let config = self.cfg();
        let order = config.service_start_order()?;
        let mut healthy_services = HashSet::new();
        let mut completed_services = HashSet::new();
        let mut failed_services = HashSet::new();
        // A skipped service is NOT a satisfied dependency — a service that
        // depends on it must not start, or `skip` silently leaks the dependent
        // into running against a dependency that never came up.
        let mut skipped_services = HashSet::new();
        let mut first_error: Option<ProcessManagerError> = None;

        'service_loop: for service_name in order {
            let service = match config.services.get(&service_name) {
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
                        self.mark_skipped(&service_name)?;
                        skipped_services.insert(service_name.clone());
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
                                self.mark_skipped(&service_name)?;
                                skipped_services.insert(service_name.clone());
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
                    if skipped_services.contains(dep_name) {
                        // A dependency that was skipped can never be satisfied, so
                        // the dependent is skipped too — never run it against a
                        // dependency that never came up.
                        info!(
                            "Skipping start of '{service_name}' because its dependency '{dep_name}' is skipped"
                        );
                        self.mark_skipped(&service_name)?;
                        skipped_services.insert(service_name.clone());
                        continue 'service_loop;
                    }
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

            let mut service_to_start = service.clone();
            service_to_start.skip = None;
            match self.start_service(&service_name, &service_to_start) {
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
        if self.boot_cancelled() {
            return Err(Self::interrupted(service_name));
        }
        debug!("Evaluating skip condition for '{service_name}': `{skip_command}`");

        let mut cmd = Command::new(DEFAULT_SHELL);
        cmd.arg(SHELL_COMMAND_FLAG).arg(skip_command);
        cmd.current_dir(&self.project_root);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        self.set_service_env(&mut cmd, service_name);

        let mut child = spawn_session(&mut cmd).map_err(|source| {
            ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source,
            }
        })?;
        let child_pid = child.id();
        let timeout = command_timeout();
        let epoch = self.boot_epoch.load(Ordering::SeqCst);
        match wait_with_epoch(
            &mut child,
            timeout,
            Some((&self.boot_epoch, epoch, &self.boot_cancelled)),
        ) {
            Ok(Some(status)) => {
                let should_skip = status.success();
                debug!(
                    "Skip condition for '{service_name}' evaluated to: {should_skip} (exit code: {:?})",
                    status.code()
                );
                Ok(should_skip)
            }
            Ok(None) => {
                let _ = Self::terminate_process_tree(
                    service_name,
                    child_pid,
                    Some(child_pid as libc::pid_t),
                );
                let _ = child.wait();
                Err(ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source: std::io::Error::new(
                        ErrorKind::TimedOut,
                        format!(
                            "skip condition for '{service_name}' {}",
                            if self.boot_active(epoch) {
                                format!("exceeded {}s", timeout.as_secs())
                            } else {
                                "was cancelled".to_string()
                            }
                        ),
                    ),
                })
            }
            Err(source) => {
                let _ = Self::terminate_process_tree(
                    service_name,
                    child_pid,
                    Some(child_pid as libc::pid_t),
                );
                let _ = child.wait();
                error!("Failed to execute skip condition for '{service_name}': {source}");
                Err(ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source,
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
        let config = self.cfg();
        let epoch = self.boot_epoch.load(Ordering::SeqCst);
        let command = config
            .services
            .get(service_name)
            .map(|service| service.command.as_str());
        let state = Self::wait_for_ready(
            service_name,
            &config.project.id,
            &self.processes,
            &self.pid_file,
            Some((&self.state_file, &config)),
            command,
            Some((&self.boot_epoch, epoch, &self.boot_cancelled)),
        )?;

        if let ServiceReadyState::Running = state
            && let Some(health_check) = config
                .services
                .get(service_name)
                .and_then(|service| service.deployment.as_ref())
                .and_then(|deployment| deployment.health_check.as_ref())
        {
            info!("Waiting for health check of '{service_name}' before marking it ready");
            if let Err(err) = self.wait_for_health_check(service_name, health_check) {
                // The unit came up as a process but never passed its health
                // check — it is NOT healthy, and leaving it running would let
                // status report a live-but-never-healthy process as `healthy`
                // (e.g. a dev server that drifted to another port). Stop it so it
                // is not a zombie on the wrong port; the monitor's restart_policy
                // still retries the whole start, bounded by max_restarts.
                warn!(
                    "Service '{service_name}' failed its health check; stopping it (not leaving a never-healthy process)"
                );
                if let Err(stop_err) = self.stop_service_with_intent(service_name, false)
                {
                    warn!(
                        "Failed to stop '{service_name}' after health-check failure: {stop_err}"
                    );
                }
                return Err(err);
            }
        }

        Ok(state)
    }

    /// Internal implementation of wait_for_service_ready that accepts explicit handles for processes
    /// and PID file. This allows the function to be called from both instance methods and static contexts.
    fn wait_for_ready(
        service_name: &str,
        project: &str,
        processes: &Arc<Mutex<HashMap<String, ManagedChild>>>,
        pid_file: &Arc<Mutex<PidFile>>,
        state: Option<(&Arc<Mutex<ServiceStateFile>>, &Arc<Config>)>,
        command: Option<&str>,
        epoch: Option<(&AtomicU64, u64, &AtomicBool)>,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        let mut waited = Duration::ZERO;
        let mut running_since = None;

        while waited <= SERVICE_START_TIMEOUT {
            if epoch.is_some_and(|(current, expected, cancelled)| {
                cancelled.load(Ordering::SeqCst)
                    || current.load(Ordering::SeqCst) != expected
            }) {
                return Err(Self::interrupted(service_name));
            }
            match Self::probe_service_state_recording(
                service_name,
                processes,
                pid_file,
                state,
            )? {
                ServiceProbe::Running => {
                    let started = running_since.get_or_insert_with(Instant::now);
                    if started.elapsed() >= SERVICE_START_STABILITY {
                        return Ok(ServiceReadyState::Running);
                    }

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

                    let tail = crate::logs::tail_service_log(project, service_name, 8);

                    // A port collision is by far the most common immediate-exit
                    // cause, and a bare "Address already in use (os error 48)" in
                    // the tail tells the user nothing actionable. Classify it as
                    // its own code so the failure names the real problem.
                    let diag = if output_indicates_port_conflict(&tail) {
                        let port = port_from_output(&tail)
                            .or_else(|| port_from_command(command));
                        let subject = match port {
                            Some(port) => format!(
                                "service `{service_name}` could not bind port {port}: already in use"
                            ),
                            None => format!(
                                "service `{service_name}` could not bind its port: already in use"
                            ),
                        };
                        crate::diag::Diagnostic::error(
                            crate::diag::SgCode::PortInUse,
                            subject,
                        )
                        .note(
                            "another process is already listening on that port, so this \
                             service exited at start",
                        )
                        .note(
                            "stop whatever holds the port, or change the port in this \
                             service's command",
                        )
                        .evidence(format!("last output from `{service_name}`"), tail)
                        .help_cmd("see what sysg manages", "sysg status")
                        .help_cmd(
                            "view logs",
                            format!("sysg logs -s {service_name} -p {project}"),
                        )
                        .help_docs()
                    } else {
                        crate::diag::Diagnostic::error(
                            crate::diag::SgCode::UnitImmediateExit,
                            format!(
                                "service `{service_name}` exited immediately at start"
                            ),
                        )
                        .note(format!("the process {how} before it finished starting"))
                        .evidence(format!("last output from `{service_name}`"), tail)
                        .help_cmd(
                            "view logs",
                            format!("sysg logs -s {service_name} -p {project}"),
                        )
                        .help_cmd("check status", format!("sysg status -p {project}"))
                        .help_docs()
                    };

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
    pub(crate) fn wait_for_dependency_completion(
        &self,
        service_name: &str,
        dep: &str,
    ) -> Result<(), ProcessManagerError> {
        let epoch = self.boot_epoch.load(Ordering::SeqCst);
        info!("Waiting for dependency '{dep}' of '{service_name}' to complete");
        self.op_slot.detail_for(
            &self.cfg().project.id.clone(),
            format!("waiting on dependency '{dep}' of '{service_name}'"),
        );

        loop {
            if self.boot_cancelled() || !self.boot_active(epoch) {
                return Err(Self::interrupted(service_name));
            }
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
                    let config = self.cfg();
                    let status = config
                        .services
                        .contains_key(dep)
                        .then(|| {
                            let key = config.state_key(dep);
                            self.state_file.lock().ok().and_then(|state| {
                                state.get(&key).map(|entry| entry.status)
                            })
                        })
                        .flatten();

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

    /// Corrects a state entry that claims `Running` with a pid that is gone.
    ///
    /// The last line of defence for the `lost`-forever bug. Whatever raced —
    /// concurrent restarts, a reap whose caller discarded the result, a crash
    /// between clearing the pid file and writing the state — the invariant is
    /// simple: a service recorded as running whose pid is dead is NOT running,
    /// and status must not keep reporting it as `lost` with nothing able to
    /// clear it. Runs every monitor tick, so recovery is automatic.
    ///
    /// Only touches entries whose recorded pid is verifiably dead; a live
    /// process keeps its record untouched.
    fn clear_stale_running_state(ctx: &DaemonContext, name: &str) {
        if !ctx.config.services.contains_key(name) {
            return;
        }
        let key = ctx.config.state_key(name);

        let stale_pid = {
            let Ok(guard) = ctx.state_file.lock() else {
                return;
            };
            match guard.get(&key) {
                Some(entry)
                    if matches!(entry.status, ServiceLifecycleStatus::Running) =>
                {
                    match entry.pid {
                        Some(pid) if !Self::pid_is_alive(pid) => pid,
                        _ => return,
                    }
                }
                _ => return,
            }
        };

        // A live child handle means a fresh instance is starting under this
        // name; leave the record for the start path to stamp.
        if ctx
            .lock_processes()
            .map(|guard| guard.contains_key(name))
            .unwrap_or(true)
        {
            return;
        }

        let corrected = ServiceLifecycleStatus::Stopped;
        warn!(
            "Service '{name}' was recorded running with pid {stale_pid}, which is gone; correcting to {corrected:?}."
        );
        if let Ok(mut guard) = ctx.state_file.lock()
            && let Err(err) = guard.set(&key, corrected, None, None, None)
        {
            warn!("Failed to correct stale running state for '{name}': {err}");
        }
    }

    /// Records an exit the probe just witnessed, so the state file can never be
    /// left claiming `running` with a pid that is gone.
    ///
    /// Only rewrites an entry still marked `Running`: a concurrent restart may
    /// already have recorded a NEWER outcome (or launched a fresh instance), and
    /// stamping this exit over that would resurrect a stale verdict.
    fn record_observed_exit(
        service_name: &str,
        status: &ExitStatus,
        state_file: &Arc<Mutex<ServiceStateFile>>,
        config: &Arc<Config>,
    ) {
        if !config.services.contains_key(service_name) {
            return;
        }
        let key = config.state_key(service_name);
        let Ok(mut guard) = state_file.lock() else {
            return;
        };
        if !matches!(
            guard.get(&key).map(|entry| entry.status),
            Some(ServiceLifecycleStatus::Running)
        ) {
            return;
        }

        let lifecycle = if status.success() {
            ServiceLifecycleStatus::ExitedSuccessfully
        } else {
            ServiceLifecycleStatus::ExitedWithError
        };
        #[cfg(unix)]
        let signal = status.signal();
        #[cfg(not(unix))]
        let signal: Option<i32> = None;
        if let Err(err) = guard.set(&key, lifecycle, None, status.code(), signal) {
            warn!("Failed to record observed exit for '{service_name}': {err}");
        }
    }

    /// Reads a service's recorded lifecycle status from the state file.
    ///
    /// This is the state SHARED across concurrent operations, so it is the only
    /// reliable answer to "did this service already run to completion?" when
    /// another restart may have observed the exit first.
    fn recorded_status(&self, service_name: &str) -> Option<ServiceLifecycleStatus> {
        let config = self.cfg();
        if !config.services.contains_key(service_name) {
            return None;
        }
        let key = config.state_key(service_name);
        let guard = self.state_file.lock().ok()?;
        guard.get(&key).map(|entry| entry.status)
    }

    /// Reports whether a pid is still alive. `kill(pid, 0)` succeeds for a live
    /// process and for a zombie the caller has yet to reap; a zombie holds no
    /// resources and no longer runs, so it is treated as dead here.
    fn pid_is_alive(pid: u32) -> bool {
        let target = pid as libc::pid_t;
        if unsafe { libc::kill(target, 0) } != 0 {
            return false;
        }
        !Self::pid_is_zombie(target)
    }

    fn process_group_is_alive(pgid: libc::pid_t) -> bool {
        if pgid <= 0 {
            return false;
        }
        if unsafe { libc::killpg(pgid, 0) } == 0 {
            return true;
        }
        matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        )
    }

    /// Reports whether a pid is a reaped-pending zombie rather than a live
    /// process, so a stop is not held open waiting on a corpse.
    fn pid_is_zombie(pid: libc::pid_t) -> bool {
        let Ok(output) = Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output()
        else {
            return false;
        };
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .starts_with('Z')
    }

    /// Chooses the terminal state for a service the monitor saw exit while a
    /// manual-stop or restart-suppress flag was set.
    ///
    /// A restart sets those flags to tear the old instance down, so the flag
    /// alone does not mean the user stopped the service. A ONE-SHOT that ran and
    /// exited 0 has COMPLETED — recording it as `stopped` rewrote finished
    /// builds/migrations as stopped+warn after `restart -p`, and `stopped` reads
    /// as a FAILED dependency downstream. Long-running services are unaffected:
    /// a clean exit there is still a stop.
    fn stopped_or_completed(
        ctx: &DaemonContext,
        name: &str,
        exit_success: bool,
    ) -> ServiceLifecycleStatus {
        // `restart_policy: always` is the only declaration that a clean exit
        // should be treated as a stop worth restarting; everything else that
        // exits 0 ran to completion. Probe-style units (`redis-cli ping`) declare
        // no policy at all, so keying off `never` alone would miss them.
        let wants_restart = ctx
            .config
            .services
            .get(name)
            .is_some_and(|service| service.restarts_after_success());
        if exit_success && !wants_restart {
            ServiceLifecycleStatus::ExitedSuccessfully
        } else {
            ServiceLifecycleStatus::Stopped
        }
    }

    /// Attempts to determine the current state of a tracked service without blocking.
    ///
    /// Uses `try_wait` to check the underlying child process and updates the PID file if the
    /// service has exited. To avoid holding the process map lock longer than necessary, the child
    /// handle is temporarily removed and inserted back when still running.
    fn probe_service_state(
        service_name: &str,
        processes: &Arc<Mutex<HashMap<String, ManagedChild>>>,
        pid_file: &Arc<Mutex<PidFile>>,
    ) -> Result<ServiceProbe, ProcessManagerError> {
        Self::probe_service_state_recording(service_name, processes, pid_file, None)
    }

    /// Probes a service, optionally RECORDING the exit it observes.
    ///
    /// The probe is the only place that witnesses the exit — it reaps the child
    /// and clears the pid. Leaving the lifecycle write to the caller meant that
    /// when a caller discarded the result (a racing concurrent restart), the
    /// state file kept `running` with a pid that no longer existed. Status
    /// prefers the state file's pid over the pid file's, so those units reported
    /// `lost` FOREVER with nothing able to correct them: pid.xml was clean,
    /// state.xml was not. Passing the state handle makes the observation and the
    /// record atomic.
    fn probe_service_state_recording(
        service_name: &str,
        processes: &Arc<Mutex<HashMap<String, ManagedChild>>>,
        pid_file: &Arc<Mutex<PidFile>>,
        state: Option<(&Arc<Mutex<ServiceStateFile>>, &Arc<Config>)>,
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
                    drop(pid_guard);

                    if let Some((state_file, config)) = state {
                        Self::record_observed_exit(
                            service_name,
                            &status,
                            state_file,
                            config,
                        );
                    }

                    return Ok(ServiceProbe::Exited(status));
                }
                Ok(None) => {
                    processes_guard.insert(service_name.to_string(), child);
                    return Ok(ServiceProbe::Running);
                }
                Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                    let child_pid = child.id();
                    drop(processes_guard);
                    let mut pid_guard = pid_file.lock()?;
                    pid_guard.clear_pid_if_matches(service_name, child_pid)?;
                    return Ok(ServiceProbe::NotStarted);
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
        let services: HashSet<String> = self.cfg().services.keys().cloned().collect();
        self.restart_services_subset(&services)
    }

    /// Restarts selected services in dependency order while preserving monitoring.
    pub(crate) fn restart_services_subset(
        &self,
        services: &HashSet<String>,
    ) -> Result<(), ProcessManagerError> {
        info!("Restarting all services...");

        let config = self.cfg();
        let order = config.service_start_order()?;
        self.shutdown_monitor();
        let mut restarted_services = Vec::new();
        let mut healthy_services = HashSet::new();
        let mut completed_services = HashSet::new();
        let mut failed_services = HashSet::new();
        let mut skipped_services = HashSet::new();
        let mut first_error = None;

        'services: for service_name in order {
            if !services.contains(&service_name) {
                continue;
            }
            let service = match config.services.get(&service_name) {
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

            let should_skip = match &service.skip {
                Some(SkipConfig::Flag(value)) => Ok(*value),
                Some(SkipConfig::Command(command)) => {
                    self.evaluate_skip_condition(&service_name, command)
                }
                None => Ok(false),
            };
            match should_skip {
                Ok(true) => {
                    if let Err(err) = self.stop_service(&service_name) {
                        first_error.get_or_insert(err);
                        failed_services.insert(service_name.clone());
                        continue;
                    }
                    if let Err(err) = self.mark_skipped(&service_name) {
                        first_error.get_or_insert(err);
                        failed_services.insert(service_name.clone());
                        continue;
                    }
                    skipped_services.insert(service_name.clone());
                    continue;
                }
                Ok(false) => {}
                Err(err) => {
                    first_error.get_or_insert(err);
                    failed_services.insert(service_name.clone());
                    continue;
                }
            }

            if let Some(deps) = &service.depends_on {
                for dep in deps {
                    let dep_name = dep.service();
                    let dep_skipped = skipped_services.contains(dep_name)
                        || matches!(
                            self.recorded_status(dep_name),
                            Some(ServiceLifecycleStatus::Skipped)
                        );
                    if dep_skipped {
                        if let Err(err) = self.stop_service(&service_name) {
                            first_error.get_or_insert(err);
                            failed_services.insert(service_name.clone());
                        } else if let Err(err) = self.mark_skipped(&service_name) {
                            first_error.get_or_insert(err);
                            failed_services.insert(service_name.clone());
                        } else {
                            skipped_services.insert(service_name.clone());
                        }
                        continue 'services;
                    }
                    if failed_services.contains(dep_name)
                        || (services.contains(dep_name)
                            && !healthy_services.contains(dep_name))
                    {
                        let err = ProcessManagerError::DependencyFailed {
                            service: service_name.clone(),
                            dependency: dep_name.to_string(),
                        };
                        first_error.get_or_insert(err);
                        failed_services.insert(service_name.clone());
                        continue 'services;
                    }
                    if dep.condition() != DependsOnCondition::Completed
                        && !services.contains(dep_name)
                    {
                        let dep_running = self
                            .pid_file
                            .lock()
                            .ok()
                            .and_then(|guard| {
                                let pid = guard.get(dep_name)?;
                                let identity_ok =
                                    guard.start_for(dep_name).is_some_and(|started| {
                                        process_start_time(pid) == Some(started)
                                    });
                                (identity_ok && Self::pid_is_alive(pid)).then_some(())
                            })
                            .is_some();
                        if !dep_running {
                            let err = ProcessManagerError::DependencyFailed {
                                service: service_name.clone(),
                                dependency: dep_name.to_string(),
                            };
                            first_error.get_or_insert(err);
                            failed_services.insert(service_name.clone());
                            continue 'services;
                        }
                    }
                    if dep.condition() == DependsOnCondition::Completed
                        && !completed_services.contains(dep_name)
                    {
                        if let Err(err) =
                            self.wait_for_dependency_completion(&service_name, dep_name)
                        {
                            error!(
                                "Failed to restart '{service_name}' because dependency '{dep_name}' did not complete: {err}"
                            );
                            first_error.get_or_insert(err);
                            failed_services.insert(service_name.clone());
                            continue 'services;
                        }
                        completed_services.insert(dep_name.to_string());
                    }
                }
            }

            let strategy_str = service
                .deployment
                .as_ref()
                .and_then(|deployment| deployment.strategy.as_deref());

            let strategy = strategy_str
                .and_then(|s| DeploymentStrategy::from_str(s).ok())
                .unwrap_or_default();

            let mut service_to_start = service.clone();
            service_to_start.skip = None;
            let result = match strategy {
                DeploymentStrategy::Rolling => {
                    self.rolling_restart_service(&service_name, &service_to_start)
                }
                DeploymentStrategy::Immediate => {
                    self.immediate_restart_service(&service_name, &service_to_start)
                }
            };
            match result {
                Ok(ServiceReadyState::CompletedSuccess) => {
                    healthy_services.insert(service_name.clone());
                    completed_services.insert(service_name.clone());
                    restarted_services.push(service_name);
                }
                Ok(ServiceReadyState::Running) => {
                    healthy_services.insert(service_name.clone());
                    restarted_services.push(service_name);
                }
                Err(err) => {
                    error!("Failed to restart '{service_name}': {err}");
                    first_error.get_or_insert(err);
                    failed_services.insert(service_name);
                }
            }
        }

        if let Err(err) = self.spawn_monitor_thread() {
            first_error.get_or_insert(err);
        }
        if let Err(err) =
            self.verify_services_running(&restarted_services, &completed_services)
        {
            first_error.get_or_insert(err);
        }
        match first_error {
            Some(err) => Err(err),
            None => {
                info!("All services restarted successfully.");
                Ok(())
            }
        }
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
        let _replacement = self.replacement(name);

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

                if previous.is_some()
                    && Self::logs_indicate_port_conflict(&self.cfg().project.id, name)
                {
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
        let _replacement = self.replacement(name);

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

        if self.boot_cancelled() {
            return Err(Self::interrupted(service_name));
        }
        let started = Instant::now();

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&self.project_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.set_service_env(&mut cmd, service_name);
        let mut child = spawn_session(&mut cmd).map_err(|source| {
            ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source,
            }
        })?;
        let child_pid = child.id();

        let service_name_owned = service_name.to_string();
        let project_id = self.cfg().project.id.clone();
        let open_sink = |kind: &str| {
            let path = resolve_log_path(&project_id, service_name, kind);
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
                if guard.len() >= PRE_START_TAIL_LINES {
                    guard.pop_front();
                }
                guard.push_back(line.to_string());
            }
        };

        let stdout_handle = if let Some(stdout) = child.stdout.take() {
            let service_label = service_name_owned.clone();
            let stdout_sink = Arc::clone(&stdout_sink);
            let stderr_sink = Arc::clone(&stderr_sink);
            let tail = Arc::clone(&tail);
            match thread::Builder::new()
                .name(OUTPUT_STDOUT_THREAD.into())
                .spawn(move || {
                    let reader = BufReader::new(stdout);
                    for line in reader.lines().map_while(Result::ok) {
                        info!("[{service_label} pre-start] {line}");
                        push_tail(&tail, &line);
                        for sink in [&stdout_sink, &stderr_sink] {
                            if let Ok(mut guard) = sink.lock()
                                && let Some(file) = guard.as_mut()
                            {
                                let _ = writeln!(file, "[pre_start] {line}");
                            }
                        }
                    }
                }) {
                Ok(handle) => Some(handle),
                Err(source) => {
                    let _ = Self::terminate_process_tree(
                        service_name,
                        child_pid,
                        Some(child_pid as libc::pid_t),
                    );
                    let _ = child.wait();
                    return Err(ProcessManagerError::ServiceStartError {
                        service: service_name.to_string(),
                        source,
                    });
                }
            }
        } else {
            None
        };

        let stderr_handle = if let Some(stderr) = child.stderr.take() {
            let service_label = service_name_owned.clone();
            let stdout_sink = Arc::clone(&stdout_sink);
            let stderr_sink = Arc::clone(&stderr_sink);
            let tail = Arc::clone(&tail);
            match thread::Builder::new()
                .name(OUTPUT_STDERR_THREAD.into())
                .spawn(move || {
                    let reader = BufReader::new(stderr);
                    for line in reader.lines().map_while(Result::ok) {
                        warn!("[{service_label} pre-start] {line}");
                        push_tail(&tail, &line);
                        for sink in [&stdout_sink, &stderr_sink] {
                            if let Ok(mut guard) = sink.lock()
                                && let Some(file) = guard.as_mut()
                            {
                                let _ = writeln!(file, "[pre_start] {line}");
                            }
                        }
                    }
                }) {
                Ok(handle) => Some(handle),
                Err(source) => {
                    let _ = Self::terminate_process_tree(
                        service_name,
                        child_pid,
                        Some(child_pid as libc::pid_t),
                    );
                    let _ = child.wait();
                    if let Some(handle) = stdout_handle {
                        let _ = handle.join();
                    }
                    return Err(ProcessManagerError::ServiceStartError {
                        service: service_name.to_string(),
                        source,
                    });
                }
            }
        } else {
            None
        };

        let pre_start_timeout = command_timeout();
        let epoch = self.boot_epoch.load(Ordering::SeqCst);
        if self.boot_cancelled() {
            let _ = Self::terminate_process_tree(
                service_name,
                child_pid,
                Some(child_pid as libc::pid_t),
            );
            let _ = child.wait();
            if let Some(handle) = stdout_handle {
                let _ = handle.join();
            }
            if let Some(handle) = stderr_handle {
                let _ = handle.join();
            }
            return Err(Self::interrupted(service_name));
        }
        let status = match wait_with_epoch(
            &mut child,
            pre_start_timeout,
            Some((&self.boot_epoch, epoch, &self.boot_cancelled)),
        ) {
            Ok(Some(status)) => status,
            Ok(None) => {
                let _ = Self::terminate_process_tree(
                    service_name,
                    child_pid,
                    Some(child_pid as libc::pid_t),
                );
                let _ = child.wait();
                if let Some(handle) = stdout_handle {
                    let _ = handle.join();
                }
                if let Some(handle) = stderr_handle {
                    let _ = handle.join();
                }
                let active = self.boot_active(epoch);
                let secs = pre_start_timeout.as_secs();
                if !active {
                    write_marker("\u{2716} pre-start was cancelled; killed");
                    return Err(Self::interrupted(service_name));
                }

                write_marker(&format!(
                    "\u{2716} pre-start timed out after {secs}s; killed"
                ));
                self.record_start_failure(service_name, None, None);
                let captured = tail
                    .lock()
                    .map(|guard| guard.iter().cloned().collect())
                    .unwrap_or_default();
                let project = self.cfg().project.id.clone();
                let diag = crate::diag::Diagnostic::error(
                    crate::diag::SgCode::PreStartTimeout,
                    format!("pre_start for `{service_name}` timed out"),
                )
                .origin(
                    format!("services.{service_name}.deployment.pre_start"),
                    None,
                    None,
                )
                .note(format!(
                    "`{command}` did not finish within {secs}s and its process tree was terminated"
                ))
                .note("the service was not launched")
                .evidence("pre_start output", captured)
                .help_cmd(
                    "view logs",
                    format!("sysg logs -s {service_name} -p {project}"),
                )
                .help_docs();
                return Err(ProcessManagerError::Diag(Box::new(diag)));
            }
            Err(source) => {
                let _ = Self::terminate_process_tree(
                    service_name,
                    child_pid,
                    Some(child_pid as libc::pid_t),
                );
                let _ = child.wait();
                if let Some(handle) = stdout_handle {
                    let _ = handle.join();
                }
                if let Some(handle) = stderr_handle {
                    let _ = handle.join();
                }
                return Err(ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source,
                });
            }
        };

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
            let project = self.cfg().project.id.clone();
            let diag = crate::diag::Diagnostic::error(
                crate::diag::SgCode::PreStartFailed,
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
        let epoch = self.boot_epoch.load(Ordering::SeqCst);
        let attempt_timeout = if let Some(raw) = &health_check.attempt_timeout {
            Self::parse_duration(raw)?
        } else {
            Duration::from_secs(30)
        };

        let retries = health_check.retries.unwrap_or(3).max(1);
        let interval = health_check
            .interval
            .as_deref()
            .map_or(Ok(Duration::from_secs(2)), Self::parse_duration)?;
        // `attempt_timeout` caps EACH probe. There is no separate total-timeout
        // knob: the absolute max wait is derived as retries x (attempt_timeout +
        // interval), so a hopeless check (a port that stays taken) fails fast on
        // connection refusal instead of stranding a foreground start.
        let client = if health_check.url.is_some() {
            // A health check is a DIRECT probe to the service — never route it
            // through an HTTP proxy. reqwest reads HTTP_PROXY/ALL_PROXY from the
            // environment by default, which made a probe to 127.0.0.1 hang for
            // the full attempt_timeout (the proxy can't reach localhost) while
            // `curl` — which bypasses the proxy for localhost — succeeded at once.
            Some(
                Client::builder()
                    .timeout(attempt_timeout)
                    .no_proxy()
                    .build()
                    .map_err(|err| ProcessManagerError::ServiceStartError {
                        service: service_name.to_string(),
                        source: std::io::Error::other(err.to_string()),
                    })?,
            )
        } else {
            None
        };

        let mut last_outcome = HealthProbeOutcome::Unhealthy;

        for attempt in 1..=retries {
            if self.boot_cancelled() || !self.boot_active(epoch) {
                return Err(Self::interrupted(service_name));
            }
            self.op_slot.detail_for(
                &self.cfg().project.id.clone(),
                format!(
                    "health check for '{service_name}' (attempt {attempt}/{retries})"
                ),
            );
            match self.perform_configured_health_check(
                service_name,
                health_check,
                client.as_ref(),
                attempt_timeout,
            ) {
                Ok(true) => {
                    info!(
                        "Health check passed for '{service_name}' on attempt {attempt}"
                    );
                    return Ok(());
                }
                Ok(false) => {
                    last_outcome = HealthProbeOutcome::Unhealthy;
                    debug!(
                        "Health check attempt {attempt} ran but reported unhealthy for '{service_name}'",
                    );
                }
                Err(err) if err.kind() == ErrorKind::TimedOut => {
                    last_outcome = HealthProbeOutcome::Timeout;
                    debug!(
                        "Health check attempt {attempt} timed out for '{service_name}': {err}",
                    );
                }
                Err(err) => {
                    last_outcome = HealthProbeOutcome::Unreachable;
                    debug!(
                        "Health check attempt {attempt} could not reach '{service_name}': {err}",
                    );
                }
            }

            if self.boot_cancelled() || !self.boot_active(epoch) {
                return Err(Self::interrupted(service_name));
            }

            if attempt != retries && !self.wait_boot_delay(epoch, interval) {
                return Err(Self::interrupted(service_name));
            }
        }

        Err(ProcessManagerError::Diag(Box::new(
            self.health_check_failure_diag(
                service_name,
                health_check,
                attempt_timeout,
                retries,
                last_outcome,
            ),
        )))
    }

    /// Builds the diagnostic for a service that never became healthy: what was
    /// checked, whether the process is even alive, its last output, and the
    /// exact commands to dig further. The code reflects *why* the last probe
    /// failed — unhealthy, unreachable, or timed out.
    fn health_check_failure_diag(
        &self,
        service_name: &str,
        health_check: &HealthCheckConfig,
        attempt_timeout: Duration,
        retries: u32,
        outcome: HealthProbeOutcome,
    ) -> crate::diag::Diagnostic {
        use crate::diag::{Diagnostic, SgCode};

        let project = self.cfg().project.id.clone();
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

        let mut diag = match outcome {
            HealthProbeOutcome::Unhealthy => {
                let mut d = Diagnostic::error(
                    SgCode::HealthUnmet,
                    format!("service `{service_name}` failed to become healthy"),
                )
                .note(format!(
                    "the health check against {target} ran but reported the service is not healthy over {retries} attempts",
                ));
                d = if alive {
                    d.note(
                        "the process is running but never answered the health check \
                         — it may be listening on a different address or still starting",
                    )
                } else {
                    d.note(
                        "the process is not running — it exited before it could become healthy",
                    )
                };
                d
            }
            HealthProbeOutcome::Unreachable => Diagnostic::error(
                SgCode::HealthCheckUnreachable,
                format!("service `{service_name}`'s health check could not reach it"),
            )
            .note(format!(
                "every probe against {target} failed to connect (the address may be wrong or a port is not listening) over {retries} attempts",
            )),
            HealthProbeOutcome::Timeout => Diagnostic::error(
                SgCode::HealthCheckTimeout,
                format!("service `{service_name}`'s health check timed out"),
            )
            .note(format!(
                "no probe against {target} completed within the {}s per-attempt budget over {retries} attempts",
                attempt_timeout.as_secs()
            )),
        };

        diag = diag.evidence(
            format!("last output from `{service_name}`"),
            crate::logs::tail_service_log(&project, service_name, 8),
        );

        diag.help_cmd(
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
            self.perform_http_health_check(service_name, client, url)
        } else {
            Err(std::io::Error::other(
                "health check requires either a command or a url",
            ))
        }
    }

    /// Performs a single health check request and evaluates the response.
    fn perform_health_check(client: &Client, url: &str) -> Result<bool, std::io::Error> {
        let response = client.get(url).send().map_err(|err| {
            let kind = if err.is_timeout() {
                ErrorKind::TimedOut
            } else {
                ErrorKind::ConnectionRefused
            };
            std::io::Error::new(kind, err.to_string())
        })?;

        Ok(response.status().is_success())
    }

    fn perform_http_health_check(
        &self,
        service_name: &str,
        client: &Client,
        url: &str,
    ) -> Result<bool, std::io::Error> {
        use std::sync::mpsc;

        let client = client.clone();
        let url = url.to_string();
        let (tx, rx) = mpsc::sync_channel(HEALTH_RESULT_CAPACITY);
        thread::Builder::new()
            .name(format!("health-{service_name}"))
            .spawn(move || {
                let _ = tx.send(Self::perform_health_check(&client, &url));
            })
            .map_err(|err| std::io::Error::other(err.to_string()))?;

        let epoch = self.boot_epoch.load(Ordering::SeqCst);
        loop {
            if self.boot_cancelled() || !self.boot_active(epoch) {
                return Err(std::io::Error::new(
                    ErrorKind::Interrupted,
                    format!("health check for '{service_name}' was cancelled"),
                ));
            }
            match rx.recv_timeout(SERVICE_POLL_INTERVAL) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(std::io::Error::other(format!(
                        "health check worker for '{service_name}' stopped"
                    )));
                }
            }
        }
    }

    fn wait_boot_delay(&self, epoch: u64, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if self.boot_cancelled() || !self.boot_active(epoch) {
                return false;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            thread::sleep(remaining.min(SERVICE_POLL_INTERVAL));
        }
        !self.boot_cancelled() && self.boot_active(epoch)
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
        self.set_service_env(&mut child, service_name);
        child.stdout(Stdio::null());
        child.stderr(Stdio::null());
        let mut child = spawn_session(&mut child)?;
        let child_pid = child.id();
        let epoch = self.boot_epoch.load(Ordering::SeqCst);
        match wait_with_epoch(
            &mut child,
            timeout,
            Some((&self.boot_epoch, epoch, &self.boot_cancelled)),
        )? {
            Some(status) => Ok(status.success()),
            None => {
                let _ = Self::terminate_process_tree(
                    service_name,
                    child_pid,
                    Some(child_pid as libc::pid_t),
                );
                let _ = child.wait();
                Err(std::io::Error::new(
                    if self.boot_active(epoch) {
                        ErrorKind::TimedOut
                    } else {
                        ErrorKind::Interrupted
                    },
                    if self.boot_active(epoch) {
                        format!(
                            "health check command timed out after {timeout:?}: {command}"
                        )
                    } else {
                        format!("health check command was cancelled: {command}")
                    },
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
            attempt_timeout: health_check.attempt_timeout.clone(),
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
        self.set_service_env(&mut cmd, service_name);

        let epoch = self.boot_epoch.load(Ordering::SeqCst);
        let output = output_with_timeout(
            &mut cmd,
            command_timeout(),
            &format!("blue/green switch for {service_name}"),
            Some((&self.boot_epoch, epoch, &self.boot_cancelled)),
        )?;
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
    fn logs_indicate_port_conflict(project: &str, service_name: &str) -> bool {
        let path = resolve_log_path(project, service_name, "stderr");
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
        if service.restart_is_disabled() {
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

            let config = self.cfg();
            let Some(service_cfg) = config.services.get(service_name) else {
                continue;
            };

            // `completed_services` records what THIS call observed. A concurrent
            // restart can reap a one-shot's exit first, so this call never sees
            // `CompletedSuccess` and would judge a service that ran perfectly as
            // failed — three racing restarts each reported SG0302 for a build
            // that had in fact finished with "All packages built successfully!".
            // The recorded state is the shared truth; consult it before failing.
            if matches!(
                self.recorded_status(service_name),
                Some(ServiceLifecycleStatus::ExitedSuccessfully)
            ) {
                continue;
            }

            if !Self::should_verify_service(service_cfg) {
                continue;
            }

            let mut stable = true;

            for attempt in 0..POST_RESTART_VERIFY_ATTEMPTS {
                if attempt > 0 {
                    thread::sleep(POST_RESTART_VERIFY_DELAY);
                }

                match Self::probe_service_state_recording(
                    service_name,
                    &self.processes,
                    &self.pid_file,
                    Some((&self.state_file, &self.cfg())),
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

        if let Err(err) = detached.child.wait()
            && err.raw_os_error() != Some(libc::ECHILD)
        {
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
        let config = self.cfg();
        let project_id = config.project.id.clone();
        let log_settings = service.effective_logs(&config.logs);

        let handle = thread::Builder::new()
            .name(SERVICE_LAUNCH_THREAD.into())
            .spawn(move || {
                debug!("Starting service thread for '{service_name}'");

                match Daemon::launch_attached_service(
                    &project_id,
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
            })
            .map_err(|source| ProcessManagerError::ServiceStartError {
                service: name.to_string(),
                source,
            })?;

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
                        Some((&self.boot_epoch, &self.boot_cancelled)),
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
                        Some((&self.boot_epoch, &self.boot_cancelled)),
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
                        Some((&self.boot_epoch, &self.boot_cancelled)),
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
        let config = self.cfg();
        let log_settings = service.effective_logs(&config.logs);
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
                        Some((&self.boot_epoch, &self.boot_cancelled)),
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
                        Some((&self.boot_epoch, &self.boot_cancelled)),
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
                        Some((&self.boot_epoch, &self.boot_cancelled)),
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
        processes: &Arc<Mutex<HashMap<String, ManagedChild>>>,
        pid_file: &Arc<Mutex<PidFile>>,
        state_file: &Arc<Mutex<ServiceStateFile>>,
        config: &Arc<Config>,
    ) -> Result<(), ProcessManagerError> {
        let (pid, service_group_id, has_child, started) = {
            let mut processes_guard = processes.lock()?;
            let (persisted_group, persisted_start) = pid_file
                .lock()
                .map(|guard| {
                    (
                        guard.pgid_for(service_name).map(|id| id as libc::pid_t),
                        guard.start_for(service_name),
                    )
                })
                .unwrap_or((None, None));

            if let Some(child) = processes_guard.get_mut(service_name) {
                let process_id = child.id();
                let group_id =
                    Self::process_group_for_pid(process_id).or(persisted_group);
                (Some(process_id), group_id, true, persisted_start)
            } else {
                let guard = pid_file.lock()?;
                let stored_pid = guard.get(service_name);
                let mut group_id = persisted_group;

                if stored_pid.is_some() && group_id.is_none() {
                    group_id = stored_pid.and_then(Self::process_group_for_pid);
                }

                (stored_pid, group_id, false, persisted_start)
            }
        };

        if !has_child
            && let Some(process_id) = pid
            && Self::pid_is_alive(process_id)
            && (started.is_none() || process_start_time(process_id) != started)
        {
            return Err(ProcessManagerError::ServiceStopError {
                service: service_name.to_string(),
                source: std::io::Error::other(format!(
                    "refusing to stop pid {process_id}: its recorded process identity does not match"
                )),
            });
        }
        if !has_child
            && pid.is_none_or(|process_id| !Self::pid_is_alive(process_id))
            && let Some(group_id) = service_group_id
            && Self::process_group_is_alive(group_id)
        {
            let Some(expected) = started else {
                return Err(ProcessManagerError::ServiceStopError {
                    service: service_name.to_string(),
                    source: std::io::Error::other(
                        "refusing to stop a live process group whose recorded leader is gone",
                    ),
                });
            };
            if process_start_time(group_id as u32)
                .is_some_and(|actual| actual != expected)
            {
                return Err(ProcessManagerError::ServiceStopError {
                    service: service_name.to_string(),
                    source: std::io::Error::other(
                        "refusing to stop a reused process group",
                    ),
                });
            }
        }

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
            let supervisor_group = unsafe { libc::getpgid(0) };
            if group_id <= 0 || group_id == supervisor_group {
                return Err(ProcessManagerError::ServiceStopError {
                    service: service_name.to_string(),
                    source: std::io::Error::other(
                        "refusing to stop an invalid service process group",
                    ),
                });
            }
            Self::terminate_process_tree(service_name, group_id as u32, Some(group_id))?;
        }

        let child_handle = {
            let mut processes_guard = processes.lock()?;
            processes_guard.remove(service_name)
        };

        if let Some(mut child) = child_handle
            && let Err(err) = child.wait()
            && err.raw_os_error() != Some(libc::ECHILD)
        {
            warn!("Failed to wait on '{service_name}' after termination: {err}");
        }

        // VERIFY the process is actually gone before recording it as stopped.
        // The kill above can silently no-op — a stale recorded pgid signals a
        // group that no longer belongs to this service, and ESRCH is swallowed
        // as "already dead". Writing `stopped` on that basis is how `stop -p`
        // came to report success while the services were still running and
        // holding their ports. A stop that cannot confirm death must say so.
        if let Some(process_id) = pid
            && Self::pid_is_alive(process_id)
        {
            let deadline = Instant::now() + STOP_VERIFY_TIMEOUT;
            while Instant::now() < deadline && Self::pid_is_alive(process_id) {
                thread::sleep(SERVICE_POLL_INTERVAL);
            }
            if Self::pid_is_alive(process_id) {
                return Err(ProcessManagerError::ServiceStopError {
                    service: service_name.to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "process {process_id} for '{service_name}' is still alive after termination"
                        ),
                    ),
                });
            }
        }

        match pid_file.lock()?.remove(service_name) {
            Ok(_) | Err(PidFileError::ServiceNotFound) => {}
            Err(err) => return Err(err.into()),
        }

        if config.services.contains_key(service_name) {
            let key = config.state_key(service_name);
            let mut state_guard = state_file.lock()?;
            // A one-shot that already RAN TO COMPLETION is `done`, and stopping
            // a project must not rewrite that history into `stopped`. Doing so
            // reported finished builds/migrations as stopped+warn after a
            // restart, dragged the project to WARN, and — worse — made them read
            // as FAILED dependencies (a `Stopped` dep is a hard failure below),
            // so anything downstream refused to come up. There is no process
            // left to stop in this case; only the record would change.
            let already_completed = matches!(
                state_guard.get(&key).map(|entry| entry.status),
                Some(ServiceLifecycleStatus::ExitedSuccessfully)
            );
            if !already_completed {
                state_guard.set(
                    &key,
                    ServiceLifecycleStatus::Stopped,
                    None,
                    None,
                    None,
                )?;
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

        let config = self.cfg();
        let result = Self::stop_service_with_handles(
            service_name,
            &self.processes,
            &self.pid_file,
            &self.state_file,
            &config,
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
            && let Some(service) = config.services.get(service_name)
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
                None,
            );
        }

        // A targeted stop relies on the recorded pid/pgid, which — now that
        // every service leads its own session — terminates the whole service
        // tree authoritatively. The command-matching stale sweep is deliberately
        // NOT run here: two services with the same command are indistinguishable
        // by command line, so scanning would reap same-command siblings (even in
        // other projects, whose pids this daemon does not know).

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
            if let Ok(mut guard) = ctx.lock_stopped_for_dependency() {
                guard
                    .entry(service.clone())
                    .or_default()
                    .insert(root.to_string());
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
                if let Ok(mut guard) = ctx.lock_stopped_for_dependency()
                    && let Some(roots) = guard.get_mut(&service)
                {
                    roots.remove(root);
                    if roots.is_empty() {
                        guard.remove(&service);
                    }
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

    /// Revives dependents that were stopped as casualties of a crashed
    /// dependency, once every dependency that felled them is healthy again. A
    /// candidate is revived only when every declared dependency condition is
    /// satisfied and it is not skipped; reviving clears its casualty flags so
    /// the normal reconcile pass restarts it. Runs each monitor tick, so a
    /// multi-level stack heals bottom-up over successive ticks.
    fn revive_ready_dependents(ctx: &DaemonContext) {
        let casualties: Vec<String> = match ctx.lock_stopped_for_dependency() {
            Ok(guard) => guard.keys().cloned().collect(),
            Err(_) => return,
        };
        if casualties.is_empty() {
            return;
        }

        for name in casualties {
            let Some(service) = ctx.config.services.get(&name) else {
                if let Ok(mut guard) = ctx.lock_stopped_for_dependency() {
                    guard.remove(&name);
                }
                continue;
            };

            if matches!(service.skip, Some(SkipConfig::Flag(true))) {
                if let Ok(mut guard) = ctx.lock_stopped_for_dependency() {
                    guard.remove(&name);
                }
                continue;
            }

            if Self::unmet_restart_dependency(ctx, service).is_some() {
                continue;
            }

            info!(
                "Dependency of '{name}' recovered; reviving the dependent stopped as a casualty."
            );
            if let Ok(mut guard) = ctx.lock_manual_stop_flags() {
                guard.remove(&name);
            }
            if let Ok(mut guard) = ctx.lock_restart_suppressed() {
                guard.remove(&name);
            }
            if let Ok(mut guard) = ctx.lock_stopped_for_dependency() {
                guard.remove(&name);
            }
        }
    }

    /// Reads one service's persisted lifecycle state from monitor-owned storage.
    fn recorded_status_in_context(
        ctx: &DaemonContext,
        service_name: &str,
    ) -> Option<ServiceLifecycleStatus> {
        let key = ctx.config.state_key(service_name);
        ctx.lock_state_file()
            .ok()
            .and_then(|state| state.get(&key).map(|entry| entry.status))
    }

    /// Whether one dependency has reached its declared restart readiness condition.
    fn restart_dependency_ready(
        ctx: &DaemonContext,
        dependency: &crate::config::DependsOn,
    ) -> bool {
        let dependency_name = dependency.service();
        let restarting = ctx
            .lock_restart_in_flight()
            .map(|services| services.contains(dependency_name))
            .unwrap_or(true);
        if restarting {
            return false;
        }

        match dependency.condition() {
            DependsOnCondition::Started => ctx.lock_pid_file().ok().is_some_and(|pids| {
                pids.get(dependency_name).is_some_and(|pid| {
                    pids.start_for(dependency_name).is_some_and(|started| {
                        Self::pid_is_alive(pid)
                            && process_start_time(pid) == Some(started)
                    })
                })
            }),
            DependsOnCondition::Completed => matches!(
                Self::recorded_status_in_context(ctx, dependency_name),
                Some(ServiceLifecycleStatus::ExitedSuccessfully)
            ),
        }
    }

    /// Returns the first dependency that blocks an automatic restart.
    fn unmet_restart_dependency<'a>(
        ctx: &DaemonContext,
        service: &'a ServiceConfig,
    ) -> Option<&'a str> {
        service.depends_on.as_ref()?.iter().find_map(|dependency| {
            (!Self::restart_dependency_ready(ctx, dependency))
                .then_some(dependency.service())
        })
    }

    /// Stops all running services.
    ///
    /// Iterates over all active processes and terminates them.
    pub fn stop_services(&self) -> Result<(), ProcessManagerError> {
        let mut services: HashSet<String> = {
            let guard = self.pid_file.lock()?;
            guard
                .services
                .keys()
                .chain(guard.service_groups.keys())
                .cloned()
                .collect()
        };
        services.extend(self.processes.lock()?.keys().cloned());
        let mut services: Vec<String> = services.into_iter().collect();
        services.sort_unstable();
        let mut first_error = None;

        for service in services {
            if let Err(err) = self.stop_service(&service) {
                error!("Failed to stop service '{service}': {err}");
                first_error.get_or_insert(err);
            }
        }

        if let Some(err) = first_error {
            return Err(err);
        }
        Ok(())
    }

    /// Stops every process whose identity is recorded in one project store.
    pub fn stop_tracked(store: StateStore) -> Result<(), ProcessManagerError> {
        let mut pid_file = PidFile::load(store.clone())?;
        let mut state_file = ServiceStateFile::load(store)?;
        let mut services: HashSet<String> = pid_file
            .services
            .keys()
            .chain(pid_file.service_groups.keys())
            .cloned()
            .collect();
        let mut services: Vec<String> = services.drain().collect();
        services.sort_unstable();
        let mut first_error = None;

        for service in services {
            let pid = pid_file.get(&service);
            let pgid = pid_file
                .pgid_for(&service)
                .map(|value| value as libc::pid_t);
            let started = pid_file.start_for(&service);
            let pid_live = pid.is_some_and(Self::pid_is_alive);

            if pid_live
                && (started.is_none() || pid.and_then(process_start_time) != started)
            {
                first_error.get_or_insert_with(|| {
                    ProcessManagerError::ServiceStopError {
                        service: service.clone(),
                        source: std::io::Error::other(
                            "recorded process identity does not match",
                        ),
                    }
                });
                continue;
            }
            if !pid_live
                && let Some(group_id) = pgid
                && Self::process_group_is_alive(group_id)
            {
                let valid = started.is_some_and(|expected| {
                    process_start_time(group_id as u32)
                        .is_none_or(|actual| actual == expected)
                });
                let supervisor_group = unsafe { libc::getpgid(0) };
                if !valid || group_id <= 0 || group_id == supervisor_group {
                    first_error.get_or_insert_with(|| {
                        ProcessManagerError::ServiceStopError {
                            service: service.clone(),
                            source: std::io::Error::other(
                                "recorded process group identity does not match",
                            ),
                        }
                    });
                    continue;
                }
            }

            if pid_live || pgid.is_some_and(Self::process_group_is_alive) {
                let root = pid.or_else(|| {
                    pgid.filter(|value| *value > 0).map(|value| value as u32)
                });
                if let Some(root) = root
                    && let Err(err) = Self::terminate_process_tree(&service, root, pgid)
                {
                    first_error.get_or_insert(err);
                    continue;
                }
            }

            if let Err(err) = pid_file.remove(&service)
                && !matches!(err, PidFileError::ServiceNotFound)
            {
                first_error.get_or_insert(err.into());
                continue;
            }

            let keys: Vec<String> = state_file
                .services
                .iter()
                .filter(|(key, entry)| {
                    entry.status == ServiceLifecycleStatus::Running
                        && key.rsplit(':').next() == Some(service.as_str())
                })
                .map(|(key, _)| key.clone())
                .collect();
            for key in keys {
                if let Err(err) = state_file.set(
                    &key,
                    ServiceLifecycleStatus::Stopped,
                    None,
                    None,
                    None,
                ) {
                    first_error.get_or_insert(err.into());
                }
            }
        }

        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Ensures that the monitor thread is running, spawning it if necessary.
    fn spawn_monitor_thread(&self) -> Result<(), ProcessManagerError> {
        let mut handle_slot = self
            .monitor_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let should_spawn = match handle_slot.as_ref() {
            Some(handle) => handle.is_finished(),
            None => true,
        };

        if should_spawn {
            debug!("Starting service monitoring thread...");
            self.running.store(true, Ordering::SeqCst);

            let ctx = self.context();

            let handle = thread::Builder::new()
                .name("sysg-monitor".to_string())
                .spawn(move || Self::monitor_loop(ctx))
                .map_err(|source| {
                    self.running.store(false, Ordering::SeqCst);
                    ProcessManagerError::ServiceStartError {
                        service: "monitor".to_string(),
                        source,
                    }
                })?;

            *handle_slot = Some(handle);
        }

        Ok(())
    }

    /// Blocks on the monitoring thread if it is running.
    fn wait_for_monitor(&self) {
        if let Some(handle) = self
            .monitor_handle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
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
                let mut locked_processes = match ctx.lock_processes() {
                    Ok(processes) => processes,
                    Err(err) => {
                        error!("Failed to inspect managed processes: {err}");
                        thread::sleep(MONITOR_RETRY_DELAY);
                        continue;
                    }
                };
                let mut vanished: Vec<String> = Vec::new();
                for (name, child) in locked_processes.iter_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            if status.success() {
                                info!("Service '{name}' exited normally.");
                            } else {
                                warn!("Service '{name}' was terminated with {status:?}.");
                            }
                            exited_services.push((name.clone(), status, child.id()));
                        }
                        Ok(None) => {
                            trace!("Service '{name}' is still running.");
                            active_services += 1;
                        }
                        // ECHILD ("No child processes"): the child was already
                        // reaped elsewhere (e.g. a completed cron/one-shot unit).
                        // It is simply gone — drop it from the map so we stop
                        // probing it, instead of error-logging every tick forever.
                        Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                            debug!(
                                "Service '{name}' already reaped (ECHILD); dropping from monitor."
                            );
                            vanished.push(name.clone());
                        }
                        Err(e) => error!("Failed to check status of '{name}': {e}"),
                    }
                }
                for name in vanished {
                    locked_processes.remove(&name);
                }
            }

            if !exited_services.is_empty() {
                for (name, exit_status, exited_pid) in exited_services {
                    let (owns_record, recorded_pgid) = match ctx.lock_pid_file() {
                        Ok(guard) => {
                            (guard.get(&name) == Some(exited_pid), guard.pgid_for(&name))
                        }
                        Err(err) => {
                            error!("Failed to inspect PID entry for '{name}': {err}");
                            continue;
                        }
                    };
                    if !owns_record {
                        if let Ok(mut processes) = ctx.lock_processes()
                            && processes
                                .get(&name)
                                .is_some_and(|child| child.id() == exited_pid)
                        {
                            processes.remove(&name);
                        }
                        continue;
                    }

                    #[cfg(target_os = "linux")]
                    ctx.cancel_service_thread(&name);

                    Self::reap_orphaned_group_before_restart(&name, recorded_pgid);

                    let manually_stopped = ctx
                        .lock_manual_stop_flags()
                        .map(|mut guard| guard.remove(&name))
                        .unwrap_or(false);
                    let restart_suppressed_for_service = ctx
                        .lock_restart_suppressed()
                        .map(|guard| guard.contains(&name))
                        .unwrap_or(true);
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
                                None,
                            );
                        }
                    }

                    if manually_stopped {
                        info!("Service '{name}' was manually stopped. Skipping restart.");
                        if let Ok(mut guard) = ctx.lock_pid_file()
                            && let Err(err) = guard.remove(&name)
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
                            Self::stopped_or_completed(&ctx, &name, exit_success),
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
                            Self::stopped_or_completed(&ctx, &name, exit_success),
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
                            .is_some_and(|service| service.restarts_after_failure());

                        if should_restart {
                            let already = ctx
                                .lock_restart_in_flight()
                                .map(|g| g.contains(&name))
                                .unwrap_or(true);
                            if !already {
                                warn!("Service '{name}' crashed. Restarting...");
                                if let Ok(mut guard) = ctx.lock_restart_in_flight() {
                                    guard.insert(name.clone());
                                }
                                restarted_services.push((name.clone(), recorded_pgid));
                            }
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

                    if let Ok(mut guard) = ctx.lock_pid_file()
                        && let Err(err) = guard.clear_pid(&name)
                        && !matches!(err, PidFileError::ServiceNotFound)
                    {
                        warn!("Failed to clear PID entry for '{name}': {err}");
                    }

                    if let Ok(mut processes) = ctx.lock_processes()
                        && processes
                            .get(&name)
                            .is_some_and(|child| child.id() == exited_pid)
                    {
                        processes.remove(&name);
                    }
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

            Self::revive_ready_dependents(&ctx);

            let mut reconciled = Self::reconcile_lost_services(&ctx);
            restarted_services.append(&mut reconciled);

            for (name, recorded_pgid) in restarted_services {
                let live_pgid = ctx.lock_pid_file().ok().and_then(|g| g.pgid_for(&name));
                let is_current_live = recorded_pgid.is_some()
                    && recorded_pgid == live_pgid
                    && ctx
                        .lock_processes()
                        .map(|p| p.contains_key(&name))
                        .unwrap_or(false);
                if !is_current_live {
                    Self::reap_orphaned_group_before_restart(&name, recorded_pgid);
                }
                if let Some(service) = ctx.config.services.get(&name) {
                    Self::handle_restart(&name, service, ctx.clone());
                } else if let Ok(mut guard) = ctx.lock_restart_in_flight() {
                    guard.remove(&name);
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

        // Clear stale pid entries FIRST, independently of the processes map. A
        // recorded pid whose process is gone is stale no matter what the map
        // says — and under concurrent restarts the map can still hold a `Child`
        // handle for a service that already exited, which made the `tracked`
        // skip below hide the staleness. Status then reported `lost`/`warn`
        // indefinitely for a one-shot that had completed successfully, dragging
        // the whole project to WARN with nothing that could ever clear it.
        for name in ctx.config.services.keys() {
            Self::clear_stale_pid_entry(ctx, name);
            Self::clear_stale_running_state(ctx, name);
        }

        for (name, service) in &ctx.config.services {
            if tracked.contains(name) {
                continue;
            }
            let recovered = ctx.lock_pid_file().ok().is_some_and(|pids| {
                pids.get(name).is_some_and(|pid| {
                    pids.start_for(name).is_some_and(|started| {
                        Self::pid_is_alive(pid)
                            && process_start_time(pid) == Some(started)
                    })
                })
            });
            if recovered {
                continue;
            }
            if !Self::should_verify_service(service) {
                continue;
            }

            if matches!(
                Self::recorded_status_in_context(ctx, name),
                Some(ServiceLifecycleStatus::Skipped)
            ) {
                continue;
            }

            if !service.restarts_after_failure() {
                continue;
            }

            if let Some(dependency) = Self::unmet_restart_dependency(ctx, service) {
                debug!(
                    "Deferring automatic restart of '{name}' until dependency '{dependency}' reaches its required state."
                );
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
            let recorded_pgid = ctx
                .lock_pid_file()
                .ok()
                .and_then(|guard| guard.pgid_for(name));
            to_restart.push((name.clone(), recorded_pgid));
        }

        to_restart
    }

    /// Reconciles stale process ownership without signaling a reused PID.
    fn clear_stale_pid_entry(ctx: &DaemonContext, name: &str) {
        let (pid, pgid, started) = match ctx.lock_pid_file() {
            Ok(guard) => (guard.get(name), guard.pgid_for(name), guard.start_for(name)),
            Err(_) => return,
        };
        if pid.is_none() && pgid.is_none() {
            return;
        }

        let reused;
        if let Some(pid) = pid
            && Self::pid_is_alive(pid)
        {
            match started {
                Some(expected) if process_start_time(pid) == Some(expected) => return,
                None => {
                    warn!(
                        "Refusing to reconcile live PID {pid} for '{name}' without a process identity"
                    );
                    return;
                }
                Some(_) => reused = true,
            }
        } else {
            reused = false;
        }

        if !reused
            && let Some(pgid) = pgid.map(|value| value as libc::pid_t)
            && Self::process_group_is_alive(pgid)
        {
            let Some(expected) = started else {
                warn!(
                    "Refusing to reconcile live process group {pgid} for '{name}' without a process identity"
                );
                return;
            };
            if process_start_time(pgid as u32).is_none_or(|actual| actual == expected) {
                Self::reap_orphaned_group_before_restart(name, Some(pgid));
                if Self::process_group_is_alive(pgid) {
                    warn!("Process group {pgid} for '{name}' survived reconciliation");
                    return;
                }
            }
        }

        if let Ok(mut guard) = ctx.lock_pid_file() {
            match guard.remove(name) {
                Ok(_) => debug!("Cleared stale process ownership for '{name}'"),
                Err(PidFileError::ServiceNotFound) => {}
                Err(err) => warn!("Failed to clear stale PID entry for '{name}': {err}"),
            }
        }
    }

    /// Handles restarting a service if its restart policy allows.
    fn handle_restart(name: &str, service: &ServiceConfig, ctx: DaemonContext) {
        if let Some(dependency) = Self::unmet_restart_dependency(&ctx, service) {
            debug!(
                "Deferring automatic restart of '{name}' until dependency '{dependency}' reaches its required state."
            );
            if let Ok(mut guard) = ctx.lock_restart_in_flight() {
                guard.remove(name);
            }
            return;
        }

        let name = name.to_string();
        let service_clone = service.clone();
        let hooks = service.hooks.clone();
        let max_restarts = service.max_restarts;
        {
            let mut counts = ctx
                .restart_counts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let count = counts.entry(name.clone()).or_insert(0);
            *count += 1;

            if let Some(max) = max_restarts
                && *count > max
            {
                error!(
                    "Service '{name}' has reached maximum restart attempts ({max}). Giving up."
                );
                ctx.restart_in_flight
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&name);
                return;
            }
        }

        let backoff = match service.backoff.as_deref() {
            Some(raw) => match Self::parse_duration(raw) {
                Ok(duration) => duration,
                Err(err) => {
                    warn!(
                        "Invalid restart backoff '{raw}' for '{name}': {err}; using {DEFAULT_RESTART_BACKOFF:?}."
                    );
                    DEFAULT_RESTART_BACKOFF
                }
            },
            None => DEFAULT_RESTART_BACKOFF,
        };

        let in_flight = Arc::clone(&ctx.restart_in_flight);
        let in_flight_name = name.clone();
        if let Err(err) = thread::Builder::new()
            .name(format!("sysg-restart-{name}"))
            .spawn(move || {
                let _in_flight =
                    InFlightGuard::new(&ctx.restart_in_flight, name.clone());
                warn!("Restarting '{name}' after {backoff:?}...");
                thread::sleep(backoff);

                if !ctx.running.load(Ordering::SeqCst) {
                    return;
                }

                if ctx
                    .lock_restart_suppressed()
                    .map(|guard| guard.contains(&name))
                    .unwrap_or(true)
                {
                    info!(
                        "Skipping automatic restart of '{name}' because it is currently suppressed."
                    );
                    if let Ok(mut counts) = ctx.lock_restart_counts() {
                        counts.remove(&name);
                    }
                    return;
                }

                if ctx
                    .lock_manual_stop_flags()
                    .map(|mut guard| guard.remove(&name))
                    .unwrap_or(true)
                {
                    info!(
                        "Skipping automatic restart of '{name}' due to concurrent manual stop."
                    );
                    if let Ok(mut counts) = ctx.lock_restart_counts() {
                        counts.remove(&name);
                    }
                    return;
                }

                if let Some(dependency) =
                    Self::unmet_restart_dependency(&ctx, &service_clone)
                {
                    debug!(
                        "Deferring automatic restart of '{name}' because dependency '{dependency}' became unavailable during backoff."
                    );
                    return;
                }

                let Some(daemon) = Self::from_context(&ctx) else {
                    debug!(
                        "Skipping automatic restart of '{name}' because its daemon ended."
                    );
                    return;
                };
                let restart_result = daemon.start_service(&name, &service_clone);

                if !ctx.running.load(Ordering::SeqCst) {
                    if matches!(&restart_result, Ok(ServiceReadyState::Running)) {
                        let _ = daemon.stop_service_with_intent(&name, false);
                    }
                    return;
                }

                let hook_outcome = match &restart_result {
                    Ok(ServiceReadyState::Running) => {
                        info!(
                            "Service '{name}' restarted and passed its readiness gates."
                        );
                        HookOutcome::Success
                    }
                    Ok(ServiceReadyState::CompletedSuccess) => {
                        if matches!(
                            daemon.recorded_status(&name),
                            Some(ServiceLifecycleStatus::Skipped)
                        ) {
                            info!("Service '{name}' is skipped after restart evaluation.");
                        } else {
                            info!(
                                "Service '{name}' completed successfully during restart."
                            );
                        }
                        HookOutcome::Success
                    }
                    Err(err) => {
                        error!("Failed to restart '{name}': {err}");
                        HookOutcome::Error
                    }
                };

                if matches!(hook_outcome, HookOutcome::Success)
                    && let Ok(mut counts) = ctx.lock_restart_counts()
                {
                    counts.insert(name.clone(), 0);
                }

                if let Some(action) = hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnRestart, hook_outcome))
                {
                    run_hook(
                        action,
                        &service_clone.env,
                        HookStage::OnRestart,
                        hook_outcome,
                        &name,
                        &ctx.project_root,
                        Some((&ctx.boot_epoch, &ctx.boot_cancelled)),
                    );
                }
            })
        {
            in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&in_flight_name);
            error!("Failed to spawn restart worker for '{in_flight_name}': {err}");
        }
    }
}

/// Clears a service's `restart_in_flight` entry when the restart thread ends,
/// on every exit path including early returns.
struct InFlightGuard {
    /// Shared set whose entry belongs to this worker.
    set: Arc<Mutex<HashSet<String>>>,
    /// Service entry removed when the worker finishes.
    name: String,
}

impl InFlightGuard {
    /// Claims cleanup responsibility for one in-flight restart entry.
    fn new(set: &Arc<Mutex<HashSet<String>>>, name: String) -> Self {
        Self {
            set: Arc::clone(set),
            name,
        }
    }
}

impl Drop for InFlightGuard {
    /// Releases the restart entry on every worker exit path.
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.name);
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
mod port_in_use_tests {
    use super::{output_indicates_port_conflict, port_from_output};

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn detects_the_macos_and_linux_spellings() {
        assert!(output_indicates_port_conflict(&lines(&[
            "Error: Address already in use (os error 48)"
        ])));
        assert!(output_indicates_port_conflict(&lines(&[
            "thread panicked: Address already in use (os error 98)"
        ])));
        assert!(!output_indicates_port_conflict(&lines(&[
            "Error: permission denied"
        ])));
    }

    #[test]
    fn pulls_the_port_when_the_output_names_one() {
        assert_eq!(
            port_from_output(&lines(&["failed to bind 127.0.0.1:8080: in use"])),
            Some(8080)
        );
        assert_eq!(
            port_from_output(&lines(&["could not listen on port 9101"])),
            Some(9101)
        );
        assert_eq!(
            port_from_output(&lines(&[
                "2026-07-18T17:35:04.790530Z INFO fetching provider=espn",
                "Error: Address already in use (os error 48)",
            ])),
            None
        );
        assert_eq!(port_from_output(&lines(&["Address already in use"])), None);
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
            version: crate::config::Version::V2,
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
            let log_path = resolve_log_path("demo", "web", "stderr");
            if let Some(dir) = log_path.parent() {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(
                &log_path,
                "Error: Server(\"Address already in use (os error 98)\")\n",
            )
            .unwrap();

            assert!(Daemon::logs_indicate_port_conflict("demo", "web"));
        });
    }

    #[test]
    fn logs_indicate_port_conflict_returns_false_when_not_present() {
        with_temp_home(|_| {
            let log_path = resolve_log_path("demo", "api", "stderr");
            if let Some(dir) = log_path.parent() {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(&log_path, "Some other failure\n").unwrap();

            assert!(!Daemon::logs_indicate_port_conflict("demo", "api"));
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

            let config = daemon.config();
            let svc = config.services.get("app").unwrap();
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
            let config = daemon.config();
            let svc = config.services.get("slow").unwrap();

            assert!(matches!(
                daemon.start_service("slow", svc).unwrap(),
                ServiceReadyState::Running
            ));

            daemon.ensure_monitoring().unwrap();
            thread::sleep(Duration::from_millis(2500));

            assert!(daemon.pid_file.lock().unwrap().get("slow").is_none());

            let state_guard = daemon.state_file.lock().unwrap();
            let service_hash = daemon.config().state_key("slow");
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
