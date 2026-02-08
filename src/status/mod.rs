#![allow(missing_docs)]
//! Status management for services in the daemon.
#[cfg(target_os = "linux")]
use std::time::UNIX_EPOCH;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    process::Command,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime},
};
#[cfg(target_os = "linux")]
use std::{fs, path::Path};

use chrono::{DateTime, Local, Utc};
use chrono_tz::Tz;
#[cfg(not(target_os = "linux"))]
use nix::sys::signal;
use nix::unistd::{Pid, getpgid};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid as SysPid, ProcessesToUpdate, System};
use thiserror::Error;
use tracing::{debug, error};

use crate::{
    config::Config,
    cron::{
        CronExecutionRecord, CronExecutionStatus, CronStateFile, PersistedCronJobState,
    },
    daemon::{PidFile, ServiceLifecycleStatus, ServiceStateFile},
    error::{PidFileError, ProcessManagerError, ServiceStateError},
    metrics::{MetricSample, MetricsHandle, MetricsStore, MetricsSummary},
    spawn::{DynamicSpawnManager, SpawnedChild},
};

const GREEN_BOLD: &str = "\x1b[1;32m";
const RED_BOLD: &str = "\x1b[1;31m";
const MAGENTA_BOLD: &str = "\x1b[1;35m";
const YELLOW_BOLD: &str = "\x1b[1;33m";
const RESET: &str = "\x1b[0m";

/// Version identifier for the machine-readable status snapshot payload.
pub const STATUS_SCHEMA_VERSION: &str = "status.v1";

/// Errors emitted when building or refreshing status snapshots.
#[derive(Debug, Error)]
pub enum StatusError {
    /// Failed to acquire the PID file information from disk.
    #[error("failed to load pid file: {0}")]
    PidFile(#[from] PidFileError),
    /// Failed to read the persisted service state file from disk.
    #[error("failed to load service state file: {0}")]
    ServiceState(#[from] ServiceStateError),
    /// Failed to refresh cron state information.
    #[error("failed to load cron state file: {0}")]
    CronState(#[from] std::io::Error),
    /// Mutex guarding the PID file was poisoned.
    #[error("pid file mutex poisoned")]
    PidFilePoisoned,
    /// Mutex guarding the service state file was poisoned.
    #[error("service state mutex poisoned")]
    ServiceStatePoisoned,
    /// Metrics store mutex was poisoned.
    #[error("metrics store mutex poisoned")]
    MetricsPoisoned,
    /// Failed to load configuration metadata required for display purposes.
    #[error("failed to load configuration: {0}")]
    Config(#[from] ProcessManagerError),
}

/// Represents the overall health of the supervisor runtime.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverallHealth {
    Healthy,
    Degraded,
    Failing,
}

/// Describes what type of unit is represented by a status entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Service,
    Cron,
    Orphaned,
}

/// Health classification for a specific unit.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnitHealth {
    Healthy,
    Degraded,
    Failing,
    Inactive, // Service is stopped/not running - health is not applicable
}

/// Machine-readable snapshot of supervisor state, cached by the resident daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusSnapshot {
    /// Version identifier for the snapshot schema format.
    pub schema_version: String,
    /// Timestamp when this snapshot was captured.
    pub captured_at: DateTime<Utc>,
    /// Aggregate health status across all units.
    pub overall_health: OverallHealth,
    /// List of all managed units and their current status.
    pub units: Vec<UnitStatus>,
}

impl StatusSnapshot {
    fn new(units: Vec<UnitStatus>) -> Self {
        let overall_health = compute_overall_health(&units);
        Self {
            schema_version: STATUS_SCHEMA_VERSION.to_string(),
            captured_at: Utc::now(),
            overall_health,
            units,
        }
    }

    /// Returns an empty snapshot used during bootstrap before any data is available.
    pub fn empty() -> Self {
        Self {
            schema_version: STATUS_SCHEMA_VERSION.to_string(),
            captured_at: Utc::now(),
            overall_health: OverallHealth::Healthy,
            units: Vec::new(),
        }
    }
}

/// Hierarchical status for a dynamically spawned child process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnedProcessNode {
    pub child: SpawnedChild,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<SpawnedProcessNode>,
}

impl SpawnedProcessNode {
    pub fn new(child: SpawnedChild, children: Vec<SpawnedProcessNode>) -> Self {
        Self { child, children }
    }
}

fn collect_tracked_pids(nodes: &[SpawnedProcessNode], seen: &mut HashSet<u32>) {
    for node in nodes {
        if seen.insert(node.child.pid) {
            collect_tracked_pids(&node.children, seen);
        }
    }
}

fn retain_unique(node: &mut SpawnedProcessNode, seen: &mut HashSet<u32>) -> bool {
    if !seen.insert(node.child.pid) {
        return false;
    }

    let mut filtered = Vec::new();
    for mut child in node.children.drain(..) {
        if retain_unique(&mut child, seen) {
            filtered.push(child);
        }
    }
    node.children = filtered;
    true
}

fn append_unique_nodes(
    target: &mut Vec<SpawnedProcessNode>,
    mut nodes: Vec<SpawnedProcessNode>,
    seen: &mut HashSet<u32>,
) {
    for mut node in nodes.drain(..) {
        if retain_unique(&mut node, seen) {
            target.push(node);
        }
    }
}

#[cfg(target_os = "linux")]
fn read_proc_task_children(pid: u32) -> Option<Vec<u32>> {
    let path = format!("/proc/{pid}/task/{pid}/children");
    let contents = fs::read_to_string(path).ok()?;
    let child_pids = contents
        .split_whitespace()
        .filter_map(|token| token.parse::<u32>().ok())
        .collect::<Vec<_>>();

    Some(child_pids)
}

fn build_spawn_tree(
    manager: &DynamicSpawnManager,
    parent_pid: u32,
    system: Option<&System>,
) -> Vec<SpawnedProcessNode> {
    manager
        .get_children(parent_pid)
        .into_iter()
        .map(|mut child| {
            let (cpu_percent, rss_bytes) = sample_process_metrics(system, child.pid);
            if cpu_percent.is_some() || rss_bytes.is_some() {
                manager.update_child_metrics(child.pid, cpu_percent, rss_bytes);
            }
            child.cpu_percent = cpu_percent;
            child.rss_bytes = rss_bytes;

            let descendants = build_spawn_tree(manager, child.pid, system);
            SpawnedProcessNode::new(child, descendants)
        })
        .collect()
}

fn build_spawn_tree_from_pidfile(
    pid_file: &PidFile,
    parent_pid: u32,
    service_hash: Option<&str>,
    is_root: bool,
    system: Option<&System>,
) -> Vec<SpawnedProcessNode> {
    let mut nodes = Vec::new();

    let mut child_metadata = Vec::new();

    let child_pids = pid_file.get_children(parent_pid);
    if !child_pids.is_empty() {
        for child_pid in child_pids {
            if let Some(metadata) = pid_file.get_spawn_metadata(child_pid) {
                child_metadata.push(metadata.clone());
            }
        }
    } else {
        child_metadata.extend(
            pid_file
                .spawn_children_for_parent(parent_pid)
                .into_iter()
                .cloned(),
        );
    }

    for metadata in child_metadata {
        if let Some(hash) = service_hash
            && metadata.service_hash.as_deref() != Some(hash)
        {
            continue;
        }

        let child_pid = metadata.pid;
        let mut child = SpawnedChild {
            name: metadata.name.clone(),
            pid: child_pid,
            parent_pid: metadata.parent_pid,
            command: metadata.command.clone(),
            started_at: metadata.started_at,
            ttl: metadata.ttl_secs.map(Duration::from_secs),
            depth: metadata.depth,
            cpu_percent: metadata.cpu_percent,
            rss_bytes: metadata.rss_bytes,
            last_exit: metadata.last_exit.clone(),
        };

        let (cpu, rss) = sample_process_metrics(system, child_pid);
        if cpu.is_some() || rss.is_some() {
            child.cpu_percent = cpu;
            child.rss_bytes = rss;
        }

        let descendants = build_spawn_tree_from_pidfile(
            pid_file,
            child_pid,
            service_hash,
            false,
            system,
        );
        nodes.push(SpawnedProcessNode::new(child, descendants));
    }

    if !nodes.is_empty() {
        return nodes;
    }

    if is_root && let Some(hash) = service_hash {
        for metadata in pid_file.spawn_roots_for_service(hash) {
            let mut child = SpawnedChild {
                name: metadata.name.clone(),
                pid: metadata.pid,
                parent_pid: metadata.parent_pid,
                command: metadata.command.clone(),
                started_at: metadata.started_at,
                ttl: metadata.ttl_secs.map(Duration::from_secs),
                depth: metadata.depth,
                cpu_percent: metadata.cpu_percent,
                rss_bytes: metadata.rss_bytes,
                last_exit: metadata.last_exit.clone(),
            };

            let (cpu, rss) = sample_process_metrics(system, metadata.pid);
            if cpu.is_some() || rss.is_some() {
                child.cpu_percent = cpu;
                child.rss_bytes = rss;
            }

            let descendants = build_spawn_tree_from_pidfile(
                pid_file,
                metadata.pid,
                Some(hash),
                false,
                system,
            );
            nodes.push(SpawnedProcessNode::new(child, descendants));
        }
    }

    nodes
}

fn build_spawn_tree_from_system(
    system: Option<&System>,
    parent_pid: u32,
    depth: usize,
    seen: &mut HashSet<u32>,
) -> Vec<SpawnedProcessNode> {
    let mut nodes = Vec::new();
    if let Some(system_ref) = system {
        for (proc_pid, process) in system_ref.processes() {
            if let Some(parent) = process.parent()
                && parent.as_u32() == parent_pid
            {
                let child_pid = proc_pid.as_u32();
                if seen.contains(&child_pid) {
                    continue;
                }

                let (cpu_percent, rss_bytes) =
                    sample_process_metrics(Some(system_ref), child_pid);
                let command = StatusManager::get_process_cmdline(child_pid);
                let mut display_name = command
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                if display_name.is_empty() {
                    display_name = format!("pid-{child_pid}");
                }
                let started_at = process_started_at(system_ref, process);

                let child = SpawnedChild {
                    name: display_name,
                    pid: child_pid,
                    parent_pid,
                    command,
                    started_at,
                    ttl: None,
                    depth,
                    cpu_percent,
                    rss_bytes,
                    last_exit: None,
                };

                let descendants =
                    build_spawn_tree_from_system(system, child_pid, depth + 1, seen);

                nodes.push(SpawnedProcessNode::new(child, descendants));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(child_pids) = read_proc_task_children(parent_pid) {
            for child_pid in child_pids {
                if seen.contains(&child_pid) {
                    continue;
                }

                let (cpu_percent, rss_bytes) = sample_process_metrics(system, child_pid);
                let command = StatusManager::get_process_cmdline(child_pid);
                let mut display_name = command
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                if display_name.is_empty() {
                    display_name = format!("pid-{child_pid}");
                }

                let started_at = if let Some(system_ref) = system
                    && let Some(process) = system_ref.process(SysPid::from_u32(child_pid))
                {
                    process_started_at(system_ref, process)
                } else {
                    SystemTime::now()
                };

                let child = SpawnedChild {
                    name: display_name,
                    pid: child_pid,
                    parent_pid,
                    command,
                    started_at,
                    ttl: None,
                    depth,
                    cpu_percent,
                    rss_bytes,
                    last_exit: None,
                };

                let descendants =
                    build_spawn_tree_from_system(system, child_pid, depth + 1, seen);

                nodes.push(SpawnedProcessNode::new(child, descendants));
            }
        }
    }

    nodes
}

fn process_started_at(_system: &System, _process: &sysinfo::Process) -> SystemTime {
    SystemTime::now()
}

fn sample_process_metrics(
    system: Option<&System>,
    pid: u32,
) -> (Option<f32>, Option<u64>) {
    if let Some(system) = system
        && let Some(process) = system.process(SysPid::from_u32(pid))
    {
        let cpu = Some(process.cpu_usage());
        let rss = Some(process.memory() * 1024);
        (cpu, rss)
    } else {
        (None, None)
    }
}

/// Status entry for a managed service or cron unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitStatus {
    pub name: String,
    pub hash: String,
    pub kind: UnitKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<ServiceLifecycleStatus>,
    pub health: UnitHealth,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessRuntime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<UptimeInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit: Option<ExitMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<CronUnitStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<UnitMetricsSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spawned_children: Vec<SpawnedProcessNode>,
}

/// Summarized metrics attached to a unit entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitMetricsSummary {
    pub latest_cpu_percent: f32,
    pub average_cpu_percent: f32,
    pub max_cpu_percent: f32,
    pub latest_rss_bytes: u64,
    pub samples: usize,
}

impl From<MetricsSummary> for UnitMetricsSummary {
    fn from(summary: MetricsSummary) -> Self {
        Self {
            latest_cpu_percent: summary.latest_cpu_percent,
            average_cpu_percent: summary.average_cpu_percent,
            max_cpu_percent: summary.max_cpu_percent,
            latest_rss_bytes: summary.latest_rss_bytes,
            samples: summary.samples,
        }
    }
}

/// Runtime process metadata for a unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessRuntime {
    pub pid: u32,
    pub state: ProcessState,
}

/// Captures how long a process has been active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UptimeInfo {
    pub seconds: u64,
    pub human: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
}

/// Exit metadata tracked for the last lifecycle transition of a unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

/// Cron-specific status attributes that augment a unit entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronUnitStatus {
    pub timezone_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<CronExecutionSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_runs: Vec<CronExecutionSummary>,
}

/// Shallow projection of cron execution history used for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronExecutionSummary {
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<CronExecutionStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<MetricSample>,
}

/// Thread-safe cache of the most recent status snapshot.
#[derive(Clone)]
pub struct StatusCache {
    inner: Arc<RwLock<StatusSnapshot>>,
}

impl StatusCache {
    pub fn new(initial: StatusSnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(initial)),
        }
    }

    /// Returns a cloned copy of the cached snapshot for read-only consumers.
    pub fn snapshot(&self) -> StatusSnapshot {
        self.inner
            .read()
            .expect("status snapshot lock poisoned")
            .clone()
    }

    /// Replaces the cached snapshot. Intended to be called by the refresher thread.
    pub fn replace(&self, snapshot: StatusSnapshot) {
        if let Ok(mut guard) = self.inner.write() {
            *guard = snapshot;
        }
    }
}

/// Background worker that periodically refreshes the cached status snapshot.
pub struct StatusRefresher {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl StatusRefresher {
    pub fn spawn<F>(cache: StatusCache, interval: Duration, mut builder: F) -> Self
    where
        F: FnMut() -> Result<StatusSnapshot, StatusError> + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !stop_clone.load(Ordering::SeqCst) {
                match builder() {
                    Ok(snapshot) => cache.replace(snapshot),
                    Err(err) => error!("failed to refresh status snapshot: {err}"),
                }

                let mut slept = Duration::ZERO;
                while slept < interval {
                    if stop_clone.load(Ordering::SeqCst) {
                        return;
                    }

                    let remaining = interval.saturating_sub(slept);
                    let step = if remaining > Duration::from_millis(100) {
                        Duration::from_millis(100)
                    } else {
                        remaining
                    };
                    thread::sleep(step);
                    slept += step;
                }
            }
        });

        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for StatusRefresher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Builds a fresh snapshot from the supervisor runtime, locking shared state while collecting.
pub fn collect_runtime_snapshot(
    config: Arc<Config>,
    pid_file: &Arc<Mutex<PidFile>>,
    service_state: &Arc<Mutex<ServiceStateFile>>,
    metrics: Option<&MetricsHandle>,
    spawn_manager: Option<&DynamicSpawnManager>,
) -> Result<StatusSnapshot, StatusError> {
    let mut cron_state = CronStateFile::load()?;
    let valid_cron_hashes: HashSet<String> = config
        .services
        .iter()
        .filter(|(_, svc)| svc.cron.is_some())
        .map(|(_, svc)| svc.compute_hash())
        .collect();
    prune_orphaned_cron_jobs(&mut cron_state, &valid_cron_hashes)?;

    let pid_guard = pid_file.lock().map_err(|_| StatusError::PidFilePoisoned)?;
    let mut state_guard = service_state
        .lock()
        .map_err(|_| StatusError::ServiceStatePoisoned)?;
    let metrics_guard = match metrics {
        Some(handle) => Some(handle.read().map_err(|_| StatusError::MetricsPoisoned)?),
        None => None,
    };

    Ok(build_snapshot(
        Some(config.as_ref()),
        &pid_guard,
        &mut state_guard,
        &mut cron_state,
        metrics_guard.as_deref(),
        spawn_manager,
    ))
}

/// Builds a snapshot purely from persisted state on disk.
pub fn collect_disk_snapshot(
    config: Option<Config>,
) -> Result<StatusSnapshot, StatusError> {
    let pid_file = PidFile::load()?;
    let mut service_state = ServiceStateFile::load()?;
    let mut cron_state = CronStateFile::load()?;
    let config_ref = config.as_ref();

    if let Some(cfg) = config_ref {
        let valid_cron_hashes: HashSet<String> = cfg
            .services
            .iter()
            .filter(|(_, svc)| svc.cron.is_some())
            .map(|(_, svc)| svc.compute_hash())
            .collect();
        prune_orphaned_cron_jobs(&mut cron_state, &valid_cron_hashes)?;
    }

    Ok(build_snapshot(
        config_ref,
        &pid_file,
        &mut service_state,
        &mut cron_state,
        None,
        None,
    ))
}

fn prune_orphaned_cron_jobs(
    cron_state: &mut CronStateFile,
    valid_hashes: &HashSet<String>,
) -> Result<(), StatusError> {
    if cron_state.jobs().is_empty() {
        return Ok(());
    }

    if cron_state.prune_jobs_not_in(valid_hashes) {
        cron_state.save().map_err(StatusError::CronState)?;
    }

    Ok(())
}

fn build_snapshot(
    config: Option<&Config>,
    pid_file: &PidFile,
    service_state: &mut ServiceStateFile,
    cron_state: &mut CronStateFile,
    metrics_store: Option<&MetricsStore>,
    spawn_manager: Option<&DynamicSpawnManager>,
) -> StatusSnapshot {
    let mut hash_to_name: HashMap<String, String> = HashMap::new();
    let mut hash_kind: HashMap<String, UnitKind> = HashMap::new();
    let mut unit_hashes: BTreeSet<String> = BTreeSet::new();

    if let Some(cfg) = config {
        for (service_name, service_config) in &cfg.services {
            let hash = service_config.compute_hash();
            hash_to_name.insert(hash.clone(), service_name.clone());
            if service_config.cron.is_some() {
                hash_kind.insert(hash.clone(), UnitKind::Cron);
            } else {
                hash_kind.insert(hash.clone(), UnitKind::Service);
            }
            unit_hashes.insert(hash);
        }
    }

    unit_hashes.extend(service_state.services().keys().cloned());
    unit_hashes.extend(cron_state.jobs().keys().cloned());

    let mut units = Vec::new();

    let process_system = {
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        Some(sys)
    };

    for hash in unit_hashes {
        let actual_name = hash_to_name.get(&hash).cloned();
        let display_name = actual_name
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| format!("[orphaned] {}", truncate_hash(&hash)));

        let kind = hash_kind.get(&hash).copied().unwrap_or_else(|| {
            if cron_state.jobs().contains_key(&hash) {
                UnitKind::Cron
            } else if actual_name.is_some() {
                UnitKind::Service
            } else {
                UnitKind::Orphaned
            }
        });

        if kind == UnitKind::Cron && actual_name.is_none() {
            continue;
        }

        let mut state_entry = service_state.get(&hash).cloned();
        let mut lifecycle = state_entry.as_ref().map(|entry| entry.status);
        let mut pid = state_entry.as_ref().and_then(|entry| entry.pid);

        if pid.is_none()
            && let Some(name) = actual_name.as_deref()
        {
            pid = pid_file.pid_for(name);
        }

        let process_runtime = pid.map(|pid| ProcessRuntime {
            pid,
            state: StatusManager::process_state(pid),
        });

        if let Some(runtime) = process_runtime.as_ref()
            && matches!(runtime.state, ProcessState::Running)
            && !matches!(lifecycle, Some(ServiceLifecycleStatus::Running))
        {
            if let Err(err) = service_state.set(
                &hash,
                ServiceLifecycleStatus::Running,
                Some(runtime.pid),
                None,
                None,
            ) {
                error!("Failed to refresh running state for '{display_name}': {err}");
            } else {
                state_entry = service_state.get(&hash).cloned();
                lifecycle = state_entry.as_ref().map(|entry| entry.status);
            }
        }

        let uptime = match process_runtime.as_ref() {
            Some(runtime) if matches!(runtime.state, ProcessState::Running) => {
                compute_uptime(runtime.pid)
            }
            _ => None,
        };

        let last_exit = state_entry.as_ref().and_then(|entry| {
            if entry.exit_code.is_some() || entry.signal.is_some() {
                Some(ExitMetadata {
                    exit_code: entry.exit_code,
                    signal: entry.signal,
                })
            } else {
                None
            }
        });

        let cron = cron_state.jobs().get(&hash).map(|job| {
            let recent_runs: Vec<CronExecutionSummary> = job
                .execution_history
                .iter()
                .rev()
                .map(cron_record_to_summary)
                .collect();

            let last_run = recent_runs.first().cloned();

            CronUnitStatus {
                timezone_label: job.timezone_label.clone(),
                timezone: job.timezone.clone(),
                last_run,
                recent_runs,
            }
        });

        let health =
            derive_unit_health(kind, lifecycle, process_runtime.as_ref(), cron.as_ref());
        let metrics_summary = metrics_store
            .and_then(|store| store.summarize_unit(&hash))
            .map(UnitMetricsSummary::from);

        // Get the command from the service config if available
        let command = config
            .and_then(|cfg| cfg.services.get(actual_name.as_deref().unwrap_or("")))
            .map(|service_config| service_config.command.clone());

        // Get spawned children for this process if we have a PID
        let service_hash_for_spawn = if matches!(kind, UnitKind::Service | UnitKind::Cron)
        {
            Some(hash.as_str())
        } else {
            None
        };

        let spawned_children = if let Some(runtime) = &process_runtime {
            let mut seen = HashSet::new();
            seen.insert(runtime.pid);

            let mut nodes = Vec::new();

            if let Some(manager) = spawn_manager {
                let managed =
                    build_spawn_tree(manager, runtime.pid, process_system.as_ref());
                collect_tracked_pids(&managed, &mut seen);
                nodes.extend(managed);
            }

            let pidfile_nodes = build_spawn_tree_from_pidfile(
                pid_file,
                runtime.pid,
                service_hash_for_spawn,
                true,
                process_system.as_ref(),
            );
            append_unique_nodes(&mut nodes, pidfile_nodes, &mut seen);

            let fresh_system = if process_system.is_some() {
                let mut sys = System::new();
                sys.refresh_processes(ProcessesToUpdate::All, true);
                Some(sys)
            } else {
                None
            };

            let system_nodes = build_spawn_tree_from_system(
                fresh_system.as_ref(),
                runtime.pid,
                1,
                &mut seen,
            );
            append_unique_nodes(&mut nodes, system_nodes, &mut seen);

            nodes
        } else {
            Vec::new()
        };

        units.push(UnitStatus {
            name: display_name,
            hash,
            kind,
            lifecycle,
            health,
            process: process_runtime,
            uptime,
            last_exit,
            cron,
            metrics: metrics_summary,
            command,
            spawned_children,
        });
    }

    // Include any PID entries that have no associated hash/config data.
    for (service_name, &pid_value) in pid_file.services() {
        if hash_to_name.values().any(|name| name == service_name) {
            continue;
        }

        let runtime = ProcessRuntime {
            pid: pid_value,
            state: StatusManager::process_state(pid_value),
        };
        let uptime = if matches!(runtime.state, ProcessState::Running) {
            compute_uptime(runtime.pid)
        } else {
            None
        };

        let health = match runtime.state {
            ProcessState::Running => UnitHealth::Healthy,
            ProcessState::Zombie | ProcessState::Missing => UnitHealth::Failing,
        };

        let metrics_summary = metrics_store
            .and_then(|store| store.summarize_unit(service_name))
            .map(UnitMetricsSummary::from);

        let mut spawned_children = if let Some(manager) = spawn_manager {
            build_spawn_tree(manager, pid_value, process_system.as_ref())
        } else {
            Vec::new()
        };

        if spawned_children.is_empty() {
            spawned_children = build_spawn_tree_from_pidfile(
                pid_file,
                pid_value,
                None,
                true,
                process_system.as_ref(),
            );
        }

        units.push(UnitStatus {
            name: service_name.clone(),
            hash: service_name.clone(),
            kind: UnitKind::Orphaned,
            lifecycle: None,
            health,
            process: Some(runtime),
            uptime,
            last_exit: None,
            cron: None,
            metrics: metrics_summary,
            command: None,
            spawned_children,
        });
    }

    StatusSnapshot::new(units)
}

fn cron_record_to_summary(record: &CronExecutionRecord) -> CronExecutionSummary {
    CronExecutionSummary {
        started_at: DateTime::<Utc>::from(record.started_at),
        completed_at: record.completed_at.map(DateTime::<Utc>::from),
        status: record.status.clone(),
        exit_code: record.exit_code,
        metrics: record.metrics.clone(),
    }
}

fn derive_unit_health(
    kind: UnitKind,
    lifecycle: Option<ServiceLifecycleStatus>,
    runtime: Option<&ProcessRuntime>,
    cron: Option<&CronUnitStatus>,
) -> UnitHealth {
    if let Some(runtime) = runtime {
        match runtime.state {
            ProcessState::Running => return UnitHealth::Healthy,
            ProcessState::Zombie => {
                return UnitHealth::Failing;
            }
            ProcessState::Missing => {
                return UnitHealth::Degraded;
            }
        }
    }

    if let Some(cron_status) = cron {
        if let Some(last_run) = cron_status.last_run.as_ref() {
            if let Some(status) = &last_run.status {
                return match status {
                    CronExecutionStatus::Success => UnitHealth::Healthy,
                    CronExecutionStatus::OverlapError => UnitHealth::Failing,
                    CronExecutionStatus::Failed(reason) => {
                        // Special case: "Failed to get PID" means systemg couldn't track the process
                        // This is not a health issue with the cron job itself
                        if reason.starts_with("Failed to get PID") {
                            UnitHealth::Healthy
                        } else if last_run.exit_code.is_some()
                            || last_run.completed_at.is_some()
                        {
                            // If the cron job has completed (has exit code or completion time),
                            // it's not failing - it just had a non-zero exit in its last run
                            // Exit code 0 is healthy, non-zero is degraded
                            match last_run.exit_code {
                                Some(0) => UnitHealth::Healthy,
                                Some(_) => UnitHealth::Degraded,
                                None => UnitHealth::Healthy, // Completed without exit code info
                            }
                        } else {
                            // Still running and failing
                            UnitHealth::Failing
                        }
                    }
                };
            }

            return UnitHealth::Healthy;
        }

        return UnitHealth::Degraded;
    }

    match lifecycle {
        Some(ServiceLifecycleStatus::ExitedWithError) => return UnitHealth::Failing,
        Some(ServiceLifecycleStatus::Running) => return UnitHealth::Healthy,
        Some(ServiceLifecycleStatus::Skipped) | Some(ServiceLifecycleStatus::Stopped) => {
            return UnitHealth::Inactive;
        }
        Some(ServiceLifecycleStatus::ExitedSuccessfully) => return UnitHealth::Healthy,
        None => {}
    }

    match kind {
        UnitKind::Cron => UnitHealth::Degraded,
        UnitKind::Service | UnitKind::Orphaned => UnitHealth::Degraded,
    }
}

pub fn compute_overall_health(units: &[UnitStatus]) -> OverallHealth {
    if units
        .iter()
        .any(|unit| matches!(unit.health, UnitHealth::Failing))
    {
        return OverallHealth::Failing;
    }

    if units
        .iter()
        .any(|unit| matches!(unit.health, UnitHealth::Degraded))
    {
        return OverallHealth::Degraded;
    }

    // Note: Inactive units don't affect overall health since they're intentionally stopped
    OverallHealth::Healthy
}

fn truncate_hash(hash: &str) -> String {
    let prefix_length = hash.len().min(12);
    hash[..prefix_length].to_string()
}

fn compute_uptime(pid: u32) -> Option<UptimeInfo> {
    #[cfg(target_os = "linux")]
    {
        let metadata = fs::metadata(format!("/proc/{pid}")).ok()?;
        let started_at = metadata.modified().ok()?;
        let started_at_utc: DateTime<Utc> = started_at.into();
        let seconds = Utc::now()
            .signed_duration_since(started_at_utc)
            .to_std()
            .ok()?
            .as_secs();
        Some(UptimeInfo {
            seconds,
            human: format_elapsed(seconds),
            started_at: Some(started_at_utc),
        })
    }

    #[cfg(target_os = "macos")]
    {
        let output = Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("etime=")
            .output()
            .ok()?;
        let raw = String::from_utf8(output.stdout).ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let seconds = parse_elapsed_seconds(trimmed)?;
        Some(UptimeInfo {
            seconds,
            human: format_elapsed(seconds),
            started_at: None,
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

fn parse_elapsed_seconds(uptime_str: &str) -> Option<u64> {
    let trimmed = uptime_str.trim();
    if trimmed.is_empty() {
        return None;
    }

    let (day_component, time_part) = match trimmed.split_once('-') {
        Some((days, rest)) => (days.trim().parse::<u64>().ok()?, rest),
        None => (0, trimmed),
    };

    let segments: Vec<&str> = time_part.split(':').collect();
    if segments.is_empty() || segments.len() > 3 {
        return None;
    }

    let mut values = [0u64; 3];
    for (idx, segment) in segments.iter().rev().enumerate() {
        values[2 - idx] = segment.trim().parse::<u64>().ok()?;
    }

    let hours = values[0];
    let minutes = values[1];
    let seconds = values[2];

    let total_seconds = seconds
        + minutes.saturating_mul(60)
        + hours.saturating_mul(3_600)
        + day_component.saturating_mul(86_400);

    Some(total_seconds)
}

pub fn format_elapsed(total_seconds: u64) -> String {
    match total_seconds {
        0..=59 => format!("{} secs ago", total_seconds),
        60..=3_599 => format!("{} mins ago", total_seconds / 60),
        3_600..=86_399 => format!("{} hours ago", total_seconds / 3_600),
        86_400..=604_799 => format!("{} days ago", total_seconds / 86_400),
        _ => format!("{} weeks ago", total_seconds / 604_800),
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Running,
    Zombie,
    Missing,
}

/// Manages service status information.
pub struct StatusManager {
    /// The PID file containing service names and their respective PIDs.
    pid_file: Arc<Mutex<PidFile>>,
    /// Persistent record of last-seen service states.
    state_file: Arc<Mutex<ServiceStateFile>>,
}

impl StatusManager {
    /// Creates a new `StatusManager` instance.
    pub fn new(
        pid_file: Arc<Mutex<PidFile>>,
        state_file: Arc<Mutex<ServiceStateFile>>,
    ) -> Self {
        Self {
            pid_file,
            state_file,
        }
    }

    fn clear_service_pid(&self, service_name: &str, service_hash: &str) {
        if let Ok(mut guard) = self.pid_file.lock() {
            let _ = guard.remove(service_name);
        }

        if let Ok(mut state_guard) = self.state_file.lock() {
            let should_update = state_guard
                .get(service_hash)
                .map(|entry| matches!(entry.status, ServiceLifecycleStatus::Running))
                .unwrap_or(false);

            if should_update {
                if let Err(err) = state_guard.set(
                    service_hash,
                    ServiceLifecycleStatus::ExitedWithError,
                    None,
                    None,
                    None,
                ) {
                    debug!(
                        "Failed to reflect cleared PID state for '{service_name}' in state file: {err}"
                    );
                }
            } else if state_guard.services().contains_key(service_name)
                && let Err(err) = state_guard.remove(service_name)
            {
                debug!(
                    "Failed to remove legacy state entry for '{service_name}' in state file: {err}"
                );
            }
        }
    }

    fn mark_service_running(&self, service_name: &str, service_hash: &str, pid: u32) {
        if let Ok(mut state_guard) = self.state_file.lock() {
            if let Err(err) = state_guard.set(
                service_hash,
                ServiceLifecycleStatus::Running,
                Some(pid),
                None,
                None,
            ) {
                debug!(
                    "Failed to record running state for '{service_name}' in state file: {err}"
                );
            } else if service_hash != service_name
                && let Err(err) = state_guard.remove(service_name)
                && !matches!(err, ServiceStateError::ServiceNotFound)
            {
                debug!(
                    "Failed to remove legacy state entry for '{service_name}' in state file: {err}"
                );
            }
        }
    }

    fn process_state(pid: u32) -> ProcessState {
        #[cfg(target_os = "linux")]
        {
            let proc_path = format!("/proc/{pid}");
            if !Path::new(&proc_path).exists() {
                return ProcessState::Missing;
            }

            if let Some(state) = Self::read_proc_state(pid)
                && matches!(state, 'Z' | 'X')
            {
                return ProcessState::Zombie;
            }

            ProcessState::Running
        }

        #[cfg(not(target_os = "linux"))]
        {
            let target = Pid::from_raw(pid as i32);
            match signal::kill(target, None) {
                Ok(_) => ProcessState::Running,
                Err(err) => {
                    if err == nix::Error::from(nix::errno::Errno::ESRCH) {
                        ProcessState::Missing
                    } else {
                        ProcessState::Running
                    }
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn read_proc_state(pid: u32) -> Option<char> {
        let stat_path_str = format!("/proc/{pid}/stat");
        let stat_path = Path::new(&stat_path_str);
        let contents = fs::read_to_string(stat_path).ok()?;
        let mut parts = contents.split_whitespace();
        parts.next()?; // pid
        let mut name_part = parts.next()?; // (comm)
        // The state follows the command, but command may contain spaces. The stat format ensures
        // the executable name is wrapped in parentheses, so consume until the closing ')'.
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

    /// Parses an uptime string in "HH:MM" format and returns a human-readable string.
    pub fn format_uptime(uptime_str: &str) -> String {
        if let Some(total_seconds) = parse_elapsed_seconds(uptime_str) {
            return format_elapsed(total_seconds);
        }

        #[cfg(target_os = "linux")]
        {
            if let Ok(parsed) =
                chrono::DateTime::parse_from_str(uptime_str, "%a %Y-%m-%d %H:%M:%S %Z")
                && let Ok(duration) = chrono::Utc::now()
                    .signed_duration_since(parsed.with_timezone(&chrono::Utc))
                    .to_std()
            {
                return format_elapsed(duration.as_secs());
            }
        }

        "Unknown".to_string()
    }

    /// Retrieves all child processes of a given PID and nests them properly.
    fn get_child_processes(pid: u32, indent: usize) -> Vec<String> {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);
        let mut children = Vec::new();

        for (proc_pid, process) in system.processes() {
            if let Some(parent) = process.parent()
                && parent.as_u32() == pid
            {
                //let proc_name = process.name().to_string_lossy().to_string();
                let proc_name = Self::get_process_cmdline(proc_pid.as_u32());
                let formatted = format!(
                    "{} ├─{} {}",
                    " ".repeat(indent),
                    proc_pid.as_u32(),
                    proc_name
                );
                children.push(formatted);

                // Recursively add grandchildren, increasing indentation
                let grand_children =
                    Self::get_child_processes(proc_pid.as_u32(), indent + 4);
                children.extend(grand_children);
            }
        }

        children
    }

    /// Shows the status of a **single service** with optional cron designation.
    fn show_status_with_cron_info_by_hash(
        &self,
        service_name: &str,
        service_hash: &str,
        is_cron: bool,
    ) {
        // Determine the health color based on service state
        let health_color = self.get_service_health_color(service_hash, is_cron);

        let display_name = if is_cron {
            format!(
                "{}[cron]{} {}{}{}",
                YELLOW_BOLD, RESET, health_color, service_name, RESET
            )
        } else {
            format!("{}{}{}", health_color, service_name, RESET)
        };

        self.show_status_impl(&display_name, service_name, service_hash);
    }

    /// Determines the health color for a service name based on its current state.
    fn get_service_health_color(
        &self,
        service_hash: &str,
        is_cron: bool,
    ) -> &'static str {
        // Check the service state
        let state_entry = {
            let guard = self
                .state_file
                .lock()
                .expect("Failed to lock service state file");
            guard.get(service_hash).cloned()
        };

        if let Some(entry) = state_entry {
            match entry.status {
                ServiceLifecycleStatus::Running
                | ServiceLifecycleStatus::ExitedSuccessfully => {
                    return GREEN_BOLD;
                }
                ServiceLifecycleStatus::ExitedWithError => {
                    return RED_BOLD;
                }
                ServiceLifecycleStatus::Stopped | ServiceLifecycleStatus::Skipped => {
                    // No color for stopped/skipped services
                    return "";
                }
            }
        }

        // For cron jobs, also check the last execution status
        if is_cron {
            let cron_state =
                CronStateFile::load().unwrap_or_else(|_| CronStateFile::default());
            if let Some(job_state) = cron_state.jobs().get(service_hash)
                && let Some(last_execution) = job_state.execution_history.back()
            {
                match last_execution.status.as_ref() {
                    Some(CronExecutionStatus::Success) => return GREEN_BOLD,
                    Some(CronExecutionStatus::Failed(_))
                    | Some(CronExecutionStatus::OverlapError) => {
                        return RED_BOLD;
                    }
                    None => {} // Still running, no color
                }
            }
        }

        // Default: no color if we can't determine the state
        ""
    }

    fn show_status_with_cron_info(
        &self,
        service_name: &str,
        is_cron: bool,
        config_path: Option<&str>,
    ) {
        // Try to load config to get the service hash
        if let Ok(config) = crate::config::load_config(config_path)
            && let Some(service_config) = config.services.get(service_name)
        {
            let service_hash = service_config.compute_hash();
            self.show_status_with_cron_info_by_hash(service_name, &service_hash, is_cron);
            return;
        }
        // Fallback: service not in config
        println!("● {} - Not found in configuration", service_name);
    }

    /// Shows the status of a **single service**.
    pub fn show_status(&self, service_name: &str, config_path: Option<&str>) {
        self.show_status_with_cron_info(service_name, false, config_path);
    }

    /// Internal implementation for showing service status.
    fn show_status_impl(
        &self,
        display_name: &str,
        service_name: &str,
        service_hash: &str,
    ) {
        debug!("Checking status for service: {service_name}");
        let state_entry = {
            let guard = self
                .state_file
                .lock()
                .expect("Failed to lock service state file");
            guard.get(service_hash).cloned()
        };

        let mut pid = state_entry.as_ref().and_then(|entry| entry.pid);
        if pid.is_none()
            && let Ok(pid_guard) = self.pid_file.lock()
        {
            pid = pid_guard.get(service_name);
        }

        if let Some(pid) = pid {
            debug!("Checking status for PID: {pid}");
            match Self::process_state(pid) {
                ProcessState::Running => {
                    if state_entry.as_ref().map(|entry| (entry.status, entry.pid))
                        != Some((ServiceLifecycleStatus::Running, Some(pid)))
                    {
                        self.mark_service_running(service_name, service_hash, pid);
                    }

                    let uptime = Self::get_process_uptime(pid);
                    let tasks = Self::get_task_count(pid);
                    let memory = Self::get_memory_usage(pid);
                    let cpu_time = Self::get_cpu_time(pid);
                    let process_group = Self::get_process_group(pid);
                    let command = Self::get_process_cmdline(pid);
                    let child_processes = Self::get_child_processes(pid, 6);
                    let uptime_label = Self::format_uptime(&uptime);

                    println!("{}● {} Running{}", GREEN_BOLD, display_name, RESET);
                    println!(
                        "   Active: {}active (running){} since {}; {}",
                        GREEN_BOLD, RESET, uptime, uptime_label
                    );
                    println!(" Main PID: {}", pid);
                    println!(
                        "    {}Tasks: {} (limit: N/A){}",
                        MAGENTA_BOLD, tasks, RESET
                    );
                    println!("   {}Memory: {:.1}M{}", MAGENTA_BOLD, memory, RESET);
                    println!("      {}CPU: {:.3}s{}", MAGENTA_BOLD, cpu_time, RESET);
                    println!(" Process Group: {}", process_group);

                    println!("     |-{} {}", pid, command.trim());
                    for child in child_processes {
                        println!("{}", child);
                    }
                    return;
                }
                ProcessState::Zombie => {
                    println!(
                        "● {} - Process {} is zombie (defunct); service is no longer running",
                        display_name, pid
                    );
                    self.clear_service_pid(service_name, service_hash);
                    return;
                }
                ProcessState::Missing => {
                    println!("● {} - Process {} not found", display_name, pid);
                    self.clear_service_pid(service_name, service_hash);
                    return;
                }
            }
        }

        if let Some(entry) = state_entry {
            match entry.status {
                ServiceLifecycleStatus::Skipped => {
                    println!("● {} - Skipped via configuration", display_name);
                    return;
                }
                ServiceLifecycleStatus::ExitedSuccessfully => {
                    let note = entry
                        .exit_code
                        .map(|code| format!(" (exit code {code})"))
                        .unwrap_or_default();
                    println!(
                        "● {} - {}Exited successfully{}{}",
                        display_name, GREEN_BOLD, note, RESET
                    );
                    return;
                }
                ServiceLifecycleStatus::ExitedWithError => {
                    let detail = match (entry.exit_code, entry.signal) {
                        (Some(code), _) => format!("exit code {code}"),
                        (None, Some(sig)) => format!("signal {sig}"),
                        _ => "unknown reason".to_string(),
                    };
                    println!(
                        "● {} - {}Exited with error ({}){}",
                        display_name, RED_BOLD, detail, RESET
                    );
                    return;
                }
                ServiceLifecycleStatus::Stopped => {
                    println!("● {} - Stopped", display_name);
                    return;
                }
                ServiceLifecycleStatus::Running => {}
            }
        }

        println!("● {} - Not running", display_name);
    }

    /// Shows the status of **all services** (including orphaned state).
    pub fn show_statuses_all(&self) {
        // Try to load config to map hashes to names
        let config = crate::config::load_config(None).ok();
        let hash_to_name: std::collections::HashMap<String, String> = config
            .as_ref()
            .map(|cfg| {
                cfg.services
                    .iter()
                    .map(|(name, svc_config)| (svc_config.compute_hash(), name.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut service_hashes: BTreeSet<String> = BTreeSet::new();

        {
            let state_guard = self
                .state_file
                .lock()
                .expect("Failed to lock service state file");
            service_hashes.extend(state_guard.services().keys().cloned());
        }

        let cron_state =
            CronStateFile::load().unwrap_or_else(|_| CronStateFile::default());
        service_hashes.extend(cron_state.jobs().keys().cloned());

        if service_hashes.is_empty() {
            println!("No managed services.");
            return;
        }

        println!("Service statuses:");
        for hash in service_hashes {
            if let Some(service_name) = hash_to_name.get(&hash) {
                let is_cron = cron_state.jobs().contains_key(&hash);
                self.show_status_with_cron_info_by_hash(service_name, &hash, is_cron);
                if let Some(cron_job) = cron_state.jobs().get(&hash) {
                    Self::print_cron_history(service_name, cron_job);
                }
            } else {
                println!(
                    "● [orphaned] {} - Service not in current config",
                    &hash[..16]
                );
            }
        }
    }

    /// Shows the status of services **only in the current config** (filtered).
    pub fn show_statuses_filtered(&self, config: &crate::config::Config) {
        let cron_state =
            CronStateFile::load().unwrap_or_else(|_| CronStateFile::default());

        if config.services.is_empty() {
            println!("No managed services.");
            return;
        }

        println!("Service statuses:");
        for (service_name, service_config) in &config.services {
            let service_hash = service_config.compute_hash();
            let is_cron = cron_state.jobs().contains_key(&service_hash);
            self.show_status_with_cron_info_by_hash(service_name, &service_hash, is_cron);
            if let Some(cron_job) = cron_state.jobs().get(&service_hash) {
                Self::print_cron_history(service_name, cron_job);
            }
        }
    }

    /// Shows the status of **all services** (legacy method, calls show_statuses_all).
    #[deprecated(note = "Use show_statuses_filtered or show_statuses_all instead")]
    pub fn show_statuses(&self) {
        self.show_statuses_all()
    }

    fn print_cron_history(service_name: &str, job_state: &PersistedCronJobState) {
        let label = if job_state.timezone_label.trim().is_empty() {
            "UTC".to_string()
        } else {
            job_state.timezone_label.trim().to_string()
        };

        println!("  Cron history ({label}) for {service_name}:");

        if job_state.execution_history.is_empty() {
            println!("    - No runs recorded yet.");
            return;
        }

        for record in job_state.execution_history.iter().rev() {
            let timestamp = record.completed_at.unwrap_or(record.started_at);
            let ts =
                Self::format_cron_timestamp(timestamp, job_state.timezone.as_deref());
            let status_str = Self::format_cron_status(record);
            println!("    - {ts} | {status_str}");
        }

        // Visually separate cron details from subsequent services.
        println!();
    }

    fn format_cron_status(record: &CronExecutionRecord) -> String {
        match record.status.as_ref() {
            Some(CronExecutionStatus::Success) => {
                let code = record.exit_code.unwrap_or(0);
                format!("{GREEN_BOLD}exit {code}{RESET}")
            }
            Some(CronExecutionStatus::Failed(reason)) => {
                let base = if let Some(code) = record.exit_code {
                    format!("{RED_BOLD}exit {code}{RESET}")
                } else {
                    format!("{RED_BOLD}failed{RESET}")
                };

                if reason.trim().is_empty() {
                    base
                } else {
                    format!("{base} - {reason}")
                }
            }
            Some(CronExecutionStatus::OverlapError) => {
                format!("{RED_BOLD}overlap detected{RESET}")
            }
            None => format!("{MAGENTA_BOLD}in progress{RESET}"),
        }
    }

    fn format_cron_timestamp(
        time: std::time::SystemTime,
        tz_hint: Option<&str>,
    ) -> String {
        let datetime_utc: DateTime<Utc> = time.into();

        if let Some(hint) = tz_hint {
            if hint.eq_ignore_ascii_case("utc") {
                return datetime_utc.format("%Y-%m-%d %H:%M:%S %Z").to_string();
            }

            if let Ok(tz) = hint.parse::<Tz>() {
                return datetime_utc
                    .with_timezone(&tz)
                    .format("%Y-%m-%d %H:%M:%S %Z")
                    .to_string();
            }
        } else {
            let datetime_local: DateTime<Local> = DateTime::from(time);
            return datetime_local.format("%Y-%m-%d %H:%M:%S %Z").to_string();
        }

        datetime_utc.format("%Y-%m-%d %H:%M:%S %Z").to_string()
    }

    /// Gets the **uptime** of a process.
    fn get_process_uptime(pid: u32) -> String {
        #[cfg(target_os = "linux")]
        {
            let start_time = std::fs::metadata(format!("/proc/{}", pid))
                .and_then(|meta| meta.modified())
                .unwrap_or(UNIX_EPOCH);
            let start_time: DateTime<Utc> = start_time.into();
            start_time.format("%a %Y-%m-%d %H:%M:%S UTC").to_string()
        }

        #[cfg(target_os = "macos")]
        {
            Command::new("ps")
                .arg("-p")
                .arg(pid.to_string())
                .arg("-o")
                .arg("etime=")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string()) // Strip newlines and any extra spaces
                .unwrap_or_else(|| "Unknown".to_string())
        }
    }

    /// Gets the **task count** (threads).
    fn get_task_count(pid: u32) -> u32 {
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("thcount=")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
    }

    /// Gets the **memory usage** in MB.
    fn get_memory_usage(pid: u32) -> f64 {
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("rss=")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|kb| kb / 1024.0)
            .unwrap_or(0.0)
    }

    /// Gets the **CPU time** used by the process.
    fn get_cpu_time(pid: u32) -> f64 {
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("time=")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|s| {
                let time_parts: Vec<&str> = s.trim().split(':').collect();
                match time_parts.as_slice() {
                    [mins, secs] => Some(
                        (mins.parse::<f64>().unwrap_or(0.0) * 60.0)
                            + secs.parse::<f64>().unwrap_or(0.0),
                    ),
                    _ => None,
                }
            })
            .unwrap_or(0.0)
    }

    /// Gets the **process group ID**.
    fn get_process_group(pid: u32) -> String {
        getpgid(Some(Pid::from_raw(pid as i32)))
            .map(|pgid| pgid.to_string())
            .unwrap_or_else(|_| "Unknown".to_string())
    }

    /// Gets the **command line** of a process.
    fn get_process_cmdline(pid: u32) -> String {
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("command=")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .unwrap_or_else(|| "Unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        sync::{Arc, Mutex},
        time::SystemTime,
    };

    use serde_json::{Map, Value, json};
    use tempfile::tempdir_in;

    use super::*;
    use crate::{daemon::PersistedSpawnChild, spawn::SpawnedExit};

    #[test]
    fn format_cron_status_success_includes_green_exit_code() {
        let record = CronExecutionRecord {
            started_at: SystemTime::now(),
            completed_at: Some(SystemTime::now()),
            status: Some(CronExecutionStatus::Success),
            exit_code: Some(0),
            metrics: vec![],
        };

        let formatted = StatusManager::format_cron_status(&record);
        assert!(formatted.contains("exit 0"));
        assert!(formatted.contains(GREEN_BOLD));
    }

    #[test]
    fn format_cron_status_failure_shows_reason() {
        let record = CronExecutionRecord {
            started_at: SystemTime::now(),
            completed_at: Some(SystemTime::now()),
            status: Some(CronExecutionStatus::Failed("boom".into())),
            exit_code: Some(2),
            metrics: vec![],
        };

        let formatted = StatusManager::format_cron_status(&record);
        assert!(formatted.contains("exit 2"));
        assert!(formatted.contains("boom"));
        assert!(formatted.contains(RED_BOLD));
    }

    #[test]
    fn build_spawn_tree_from_pidfile_carries_metrics_and_exit() {
        let parent_pid = 100;
        let child_pid = 200;

        let metadata = PersistedSpawnChild {
            pid: child_pid,
            name: "child".into(),
            command: "echo hi".into(),
            started_at: SystemTime::now(),
            ttl_secs: None,
            depth: 1,
            parent_pid,
            service_hash: None,
            cpu_percent: Some(12.5),
            rss_bytes: Some(2048),
            last_exit: Some(SpawnedExit {
                exit_code: Some(0),
                signal: None,
                finished_at: Some(SystemTime::now()),
            }),
        };

        let pid_file: PidFile = serde_json::from_value(json!({
            "services": {},
            "parent_map": { child_pid.to_string(): parent_pid },
            "children_map": { parent_pid.to_string(): [child_pid] },
            "spawn_depth": { child_pid.to_string(): 1 },
            "spawn_metadata": { child_pid.to_string(): metadata },
        }))
        .expect("deserialize pid file");

        let nodes =
            build_spawn_tree_from_pidfile(&pid_file, parent_pid, None, true, None);
        assert_eq!(nodes.len(), 1);
        let child = &nodes[0].child;
        assert_eq!(child.cpu_percent, Some(12.5));
        assert_eq!(child.rss_bytes, Some(2048));
        assert!(child.last_exit.is_some());
    }

    #[test]
    fn build_spawn_tree_from_pidfile_recovers_nested_metadata() {
        let owner_pid = 5000;
        let team_lead_pid = 5001;
        let core_pid = 5002;
        let ui_pid = 5003;
        let service_hash = "demo-hash";

        let team_lead = PersistedSpawnChild {
            pid: team_lead_pid,
            name: "team_lead".into(),
            command: "team lead".into(),
            started_at: SystemTime::now(),
            ttl_secs: None,
            depth: 1,
            parent_pid: owner_pid,
            service_hash: Some(service_hash.to_string()),
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
        };

        let core_infra = PersistedSpawnChild {
            pid: core_pid,
            name: "core_infra_dev".into(),
            command: "core".into(),
            started_at: SystemTime::now(),
            ttl_secs: None,
            depth: 2,
            parent_pid: team_lead_pid,
            service_hash: Some(service_hash.to_string()),
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
        };

        let ui_dev = PersistedSpawnChild {
            pid: ui_pid,
            name: "ui_dev".into(),
            command: "ui".into(),
            started_at: SystemTime::now(),
            ttl_secs: None,
            depth: 2,
            parent_pid: team_lead_pid,
            service_hash: Some(service_hash.to_string()),
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
        };

        let mut spawn_metadata = Map::new();
        spawn_metadata.insert(
            team_lead_pid.to_string(),
            serde_json::to_value(&team_lead).expect("serialize team lead"),
        );
        spawn_metadata.insert(
            core_pid.to_string(),
            serde_json::to_value(&core_infra).expect("serialize core"),
        );
        spawn_metadata.insert(
            ui_pid.to_string(),
            serde_json::to_value(&ui_dev).expect("serialize ui"),
        );

        let mut spawn_depth = Map::new();
        spawn_depth.insert(team_lead_pid.to_string(), json!(1));
        spawn_depth.insert(core_pid.to_string(), json!(2));
        spawn_depth.insert(ui_pid.to_string(), json!(2));

        let mut root = Map::new();
        root.insert("services".to_string(), json!({}));
        root.insert("parent_map".to_string(), json!({}));
        root.insert("children_map".to_string(), json!({}));
        root.insert("spawn_depth".to_string(), Value::Object(spawn_depth));
        root.insert("spawn_metadata".to_string(), Value::Object(spawn_metadata));

        let pid_file: PidFile =
            serde_json::from_value(Value::Object(root)).expect("deserialize pid file");

        let nodes = build_spawn_tree_from_pidfile(
            &pid_file,
            owner_pid,
            Some(service_hash),
            true,
            None,
        );

        assert_eq!(nodes.len(), 1);
        let team = &nodes[0];
        assert_eq!(team.child.name, "team_lead");
        assert_eq!(team.child.parent_pid, owner_pid);
        let mut child_names: Vec<_> = team
            .children
            .iter()
            .map(|node| node.child.name.as_str())
            .collect();
        child_names.sort();
        assert_eq!(child_names, vec!["core_infra_dev", "ui_dev"]);
    }

    #[test]
    fn clear_service_pid_marks_hash_entry_as_exited() {
        let _guard = crate::test_utils::env_lock();

        let base = env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base directory");
        let temp = tempdir_in(&base).expect("create temp home");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home directory");

        let original_home = env::var("HOME").ok();
        unsafe {
            env::set_var("HOME", &home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let pid_file = Arc::new(Mutex::new(PidFile::load().expect("load pid file")));
        {
            let mut guard = pid_file.lock().expect("lock pid file");
            guard.insert("demo_service", 42).expect("insert pid entry");
        }

        let service_hash = "deadbeefdeadbeef";
        let state_file = Arc::new(Mutex::new(
            ServiceStateFile::load().expect("load state file"),
        ));
        {
            let mut guard = state_file.lock().expect("lock state file");
            guard
                .set(
                    service_hash,
                    ServiceLifecycleStatus::Running,
                    Some(42),
                    None,
                    None,
                )
                .expect("record running state");
        }

        let manager = StatusManager::new(Arc::clone(&pid_file), Arc::clone(&state_file));
        manager.clear_service_pid("demo_service", service_hash);

        {
            let guard = state_file.lock().expect("lock state file");
            let entry = guard.get(service_hash).expect("state entry present");
            assert_eq!(entry.status, ServiceLifecycleStatus::ExitedWithError);
            assert!(entry.pid.is_none());
        }

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

    #[test]
    fn disk_snapshot_includes_spawn_children_from_pidfile() {
        let _guard = crate::test_utils::env_lock();

        let base = env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base directory");
        let temp = tempdir_in(&base).expect("create temp home");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home directory");

        let original_home = env::var("HOME").ok();
        unsafe {
            env::set_var("HOME", &home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let config_path = home.join("systemg.yaml");
        fs::write(
            &config_path,
            r#"version: "1"
services:
  demo:
    command: "/bin/true"
    spawn:
      mode: "dynamic"
      limits:
        children: 5
        depth: 3
"#,
        )
        .expect("write config");

        let config =
            crate::config::load_config(Some(config_path.to_string_lossy().as_ref()))
                .expect("load config");
        let service = config.services.get("demo").expect("demo service");
        let hash = service.compute_hash();

        let mut pid_file = PidFile::load().expect("load pid file");
        pid_file.insert("demo", 42).expect("insert service pid");
        let persisted = PersistedSpawnChild {
            pid: 4242,
            name: "agent_child".into(),
            command: "echo hi".into(),
            started_at: SystemTime::now(),
            ttl_secs: Some(5),
            depth: 1,
            parent_pid: 42,
            service_hash: Some(hash.to_string()),
            cpu_percent: Some(0.0),
            rss_bytes: Some(0),
            last_exit: None,
        };
        pid_file
            .record_spawn(persisted)
            .expect("record spawn metadata");

        let mut state = ServiceStateFile::load().expect("load state");
        state
            .set(&hash, ServiceLifecycleStatus::Running, Some(42), None, None)
            .expect("persist running state");

        let snapshot = collect_disk_snapshot(Some(config)).expect("collect snapshot");
        let unit = snapshot
            .units
            .iter()
            .find(|unit| unit.name == "demo")
            .expect("demo unit present");

        assert_eq!(unit.spawned_children.len(), 1);
        let child = &unit.spawned_children[0].child;
        assert_eq!(child.name, "agent_child");
        assert_eq!(child.pid, 4242);
        assert_eq!(child.parent_pid, 42);

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

    #[test]
    fn build_snapshot_omits_orphaned_cron_units() {
        let services = std::collections::HashMap::new();
        let config = Config {
            version: "1".into(),
            services,
            project_dir: None,
            env: None,
            metrics: crate::config::MetricsConfig::default(),
        };

        let pid_file = PidFile::default();
        let mut service_state = ServiceStateFile::default();

        let cron_state_json = json!({
            "jobs": {
                "deadbeefdeadbeef": {
                    "last_execution": null,
                    "execution_history": [],
                    "timezone_label": "UTC",
                    "timezone": "UTC"
                }
            }
        });
        let mut cron_state: CronStateFile =
            serde_json::from_value(cron_state_json).expect("deserialize cron state");

        let snapshot = build_snapshot(
            Some(&config),
            &pid_file,
            &mut service_state,
            &mut cron_state,
            None,
            None,
        );

        let orphan_cron_units: Vec<_> = snapshot
            .units
            .iter()
            .filter(|unit| {
                unit.kind == UnitKind::Cron && unit.name.starts_with("[orphaned]")
            })
            .collect();

        assert!(
            orphan_cron_units.is_empty(),
            "orphaned cron entries should be pruned from status"
        );
    }

    #[test]
    fn compute_overall_health_reflects_worst_unit() {
        let units = vec![
            UnitStatus {
                name: "svc-a".into(),
                hash: "hash-a".into(),
                kind: UnitKind::Service,
                lifecycle: None,
                health: UnitHealth::Healthy,
                process: None,
                uptime: None,
                last_exit: None,
                cron: None,
                metrics: None,
                command: None,
                spawned_children: Vec::new(),
            },
            UnitStatus {
                name: "svc-b".into(),
                hash: "hash-b".into(),
                kind: UnitKind::Service,
                lifecycle: None,
                health: UnitHealth::Failing,
                process: None,
                uptime: None,
                last_exit: None,
                cron: None,
                metrics: None,
                command: None,
                spawned_children: Vec::new(),
            },
        ];

        assert_eq!(compute_overall_health(&units), OverallHealth::Failing);
    }

    #[test]
    fn derive_unit_health_for_successful_cron_is_healthy() {
        let summary = CronExecutionSummary {
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            status: Some(CronExecutionStatus::Success),
            exit_code: Some(0),
            metrics: vec![],
        };

        let cron_status = CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: Some(summary),
            recent_runs: Vec::new(),
        };

        let health = derive_unit_health(UnitKind::Cron, None, None, Some(&cron_status));
        assert_eq!(health, UnitHealth::Healthy);
    }

    #[test]
    fn derive_unit_health_for_failed_service_is_failing() {
        let health = derive_unit_health(
            UnitKind::Service,
            Some(ServiceLifecycleStatus::ExitedWithError),
            None,
            None,
        );
        assert_eq!(health, UnitHealth::Failing);
    }
}
