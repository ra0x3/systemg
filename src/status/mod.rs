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
    config::{Config, ProjectConfig, ServiceConfig, StatusSnapshotMode},
    cron::{
        CronExecutionRecord, CronExecutionStatus, CronStateFile, PersistedCronJobState,
    },
    daemon::{PidFile, ServiceLifecycleStatus, ServiceStateFile},
    error::{PidFileError, ProcessManagerError, ServiceStateError},
    metrics::{MetricSample, MetricsHandle, MetricsStore, MetricsSummary},
    spawn::{DynamicSpawnManager, SpawnedChild, SpawnedChildKind},
    state_store::StateStore,
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
    /// No unit currently requires operator attention.
    Healthy,
    /// At least one unit is impaired or suspicious, but no unit is known to have failed.
    Warn,
    /// At least one unit is in a known failed condition requiring action.
    Failing,
}

/// Describes what type of unit is represented by a status entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    /// Regular service managed by the supervisor.
    Service,
    /// Cron-scheduled job that runs at specified intervals.
    Cron,
    /// Orphaned process from a removed or renamed service.
    Orphaned,
}

/// Admin-facing factual state for a unit.
///
/// `UnitState` answers "what is this unit doing, or what happened most
/// recently?" It is intentionally separate from [`UnitIntent`] and
/// [`UnitHealth`]: a `Stopped` unit may be acceptable when its intent is
/// `Manual`, while a `Lost` unit is suspicious because the supervisor expected
/// to find a tracked process and could not.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UnitState {
    /// A live process is currently observed.
    Running,
    /// The unit completed successfully and is not expected to stay alive.
    Done,
    /// The unit exited unsuccessfully or reported a failed cron run.
    Failed,
    /// The unit was intentionally stopped.
    Stopped,
    /// The unit was intentionally skipped by configuration.
    Skipped,
    /// A previously tracked or expected process is no longer present.
    Lost,
    /// The process is defunct and waiting to be reaped.
    Zombie,
    /// A cron unit is scheduled but has not completed a run yet.
    Queued,
    /// A cron execution was blocked by an already-running prior execution.
    Overlap,
    /// The supervisor does not have enough evidence to classify the unit.
    #[default]
    Unknown,
}

/// Admin-facing intent for a unit.
///
/// `UnitIntent` answers "what should this unit normally do?" This gives state
/// labels their operational meaning: `Stopped` + `Serve` is a warning, while
/// `Stopped` + `Manual` is expected.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UnitIntent {
    /// Long-running service expected to remain available.
    Serve,
    /// One-shot service expected to complete and exit.
    Once,
    /// Cron-scheduled unit expected to run on its configured schedule.
    Cron,
    /// Manually controlled unit that is allowed to be stopped.
    #[default]
    Manual,
    /// Unit skipped by configuration.
    Skip,
    /// Persisted runtime state that no longer has a matching configuration.
    Orphan,
}

/// Admin-facing health classification for a specific unit.
///
/// `UnitHealth` answers "does this unit need operator attention?" It is not a
/// runtime state and does not say whether a process is currently running.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnitHealth {
    /// Unit is doing exactly what its intent requires.
    Healthy,
    /// Unit is acceptable but not active, such as a completed one-shot or queued cron.
    Idle,
    /// Unit is suspicious or not in the desired shape, but has not hard-failed.
    Warn,
    /// Unit is in a known failed condition requiring action.
    Failing,
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
    /// Handles new.
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
    /// Handles new.
    pub fn new(child: SpawnedChild, children: Vec<SpawnedProcessNode>) -> Self {
        Self { child, children }
    }
}

/// Collects tracked pids.
fn collect_tracked_pids(nodes: &[SpawnedProcessNode], seen: &mut HashSet<u32>) {
    for node in nodes {
        if seen.insert(node.child.pid) {
            collect_tracked_pids(&node.children, seen);
        }
    }
}

/// Retains unique.
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

/// Appends unique nodes.
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
/// Reads proc task children.
fn read_proc_task_children(pid: u32) -> Option<Vec<u32>> {
    let path = format!("/proc/{pid}/task/{pid}/children");
    let contents = fs::read_to_string(path).ok()?;
    let child_pids = contents
        .split_whitespace()
        .filter_map(|token| token.parse::<u32>().ok())
        .collect::<Vec<_>>();

    Some(child_pids)
}

/// Builds spawn tree.
fn build_spawn_tree(
    manager: &DynamicSpawnManager,
    parent_pid: u32,
    system: Option<&System>,
) -> Vec<SpawnedProcessNode> {
    manager
        .get_children(parent_pid)
        .into_iter()
        .map(|mut child| {
            child.user = Some(StatusManager::get_process_user(child.pid));
            let cmdline = StatusManager::get_process_cmdline(child.pid);
            if !cmdline.is_empty() {
                child.command = cmdline;
            }
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

/// Parent-child lookup built from one refreshed process table.
struct ProcessIndex<'a> {
    system: &'a System,
    children_by_parent: HashMap<u32, Vec<u32>>,
}

impl<'a> ProcessIndex<'a> {
    /// Builds an index from the already-refreshed process table.
    fn new(system: &'a System) -> Self {
        let mut children_by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, process) in system.processes() {
            if let Some(parent) = process.parent() {
                children_by_parent
                    .entry(parent.as_u32())
                    .or_default()
                    .push(pid.as_u32());
            }
        }

        Self {
            system,
            children_by_parent,
        }
    }

    /// Returns known child pids for a parent.
    fn child_pids(&self, parent_pid: u32) -> impl Iterator<Item = u32> + '_ {
        self.children_by_parent
            .get(&parent_pid)
            .into_iter()
            .flatten()
            .copied()
    }

    /// Returns process metadata from the indexed table.
    fn process(&self, pid: u32) -> Option<&'a sysinfo::Process> {
        self.system.process(SysPid::from_u32(pid))
    }
}

/// Augments spawn tree with system descendants.
fn augment_spawn_tree_with_system_descendants(
    node: &mut SpawnedProcessNode,
    process_index: Option<&ProcessIndex<'_>>,
    seen: &mut HashSet<u32>,
) {
    seen.insert(node.child.pid);
    for child in &mut node.children {
        augment_spawn_tree_with_system_descendants(child, process_index, seen);
    }

    let system_nodes = build_spawn_tree_from_system(
        process_index,
        node.child.pid,
        node.child.depth + 1,
        seen,
    );
    append_unique_nodes(&mut node.children, system_nodes, seen);
}

/// Builds spawn tree from pidfile.
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
        if system.is_some()
            && !matches!(
                StatusManager::process_state(child_pid),
                ProcessState::Running
            )
        {
            continue;
        }

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
            user: Some(StatusManager::get_process_user(child_pid)),
            kind: SpawnedChildKind::Spawned,
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
            if system.is_some()
                && !matches!(
                    StatusManager::process_state(metadata.pid),
                    ProcessState::Running
                )
            {
                continue;
            }

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
                user: Some(StatusManager::get_process_user(metadata.pid)),
                kind: SpawnedChildKind::Spawned,
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

/// Builds spawn tree from system.
fn build_spawn_tree_from_system(
    process_index: Option<&ProcessIndex<'_>>,
    parent_pid: u32,
    depth: usize,
    seen: &mut HashSet<u32>,
) -> Vec<SpawnedProcessNode> {
    let mut nodes = Vec::new();
    if let Some(index) = process_index {
        for child_pid in index.child_pids(parent_pid) {
            if seen.contains(&child_pid) {
                continue;
            }

            let (cpu_percent, rss_bytes) =
                sample_process_metrics(Some(index.system), child_pid);
            let command = StatusManager::get_process_cmdline(child_pid);
            let mut display_name = command
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            if display_name.is_empty() {
                display_name = format!("pid-{child_pid}");
            }
            let started_at = if let Some(process) = index.process(child_pid) {
                process_started_at(index.system, process)
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
                user: Some(StatusManager::get_process_user(child_pid)),
                kind: SpawnedChildKind::Peripheral,
            };

            let descendants =
                build_spawn_tree_from_system(process_index, child_pid, depth + 1, seen);

            nodes.push(SpawnedProcessNode::new(child, descendants));
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(child_pids) = read_proc_task_children(parent_pid) {
            for child_pid in child_pids {
                if seen.contains(&child_pid) {
                    continue;
                }

                let system = process_index.map(|index| index.system);
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

                let started_at = if let Some(index) = process_index
                    && let Some(process) = index.process(child_pid)
                {
                    process_started_at(index.system, process)
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
                    user: Some(StatusManager::get_process_user(child_pid)),
                    kind: SpawnedChildKind::Peripheral,
                };

                let descendants = build_spawn_tree_from_system(
                    process_index,
                    child_pid,
                    depth + 1,
                    seen,
                );

                nodes.push(SpawnedProcessNode::new(child, descendants));
            }
        }
    }

    nodes
}

/// Handles process started at.
fn process_started_at(_system: &System, _process: &sysinfo::Process) -> SystemTime {
    SystemTime::now()
}

/// Samples process metrics.
fn sample_process_metrics(
    system: Option<&System>,
    pid: u32,
) -> (Option<f32>, Option<u64>) {
    if let Some(system) = system
        && let Some(process) = system.process(SysPid::from_u32(pid))
    {
        let cpu = Some(process.cpu_usage());
        let rss = Some(process.memory());
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectStatus>,
    pub kind: UnitKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<ServiceLifecycleStatus>,
    #[serde(default)]
    pub state: UnitState,
    #[serde(default)]
    pub intent: UnitIntent,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spawned_children: Vec<SpawnedProcessNode>,
}

/// Project metadata attached to a status entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectStatus {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub mode: ProjectRunMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
}

impl From<&ProjectConfig> for ProjectStatus {
    fn from(project: &ProjectConfig) -> Self {
        Self {
            id: project.id.clone(),
            name: project.name.clone(),
            mode: ProjectRunMode::Daemon,
            config_path: None,
        }
    }
}

/// How a project is tied to the supervisor lifecycle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRunMode {
    /// Project should continue running independently of any foreground client.
    #[default]
    Daemon,
    /// Project lifetime is owned by a foreground client/session.
    Foreground,
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
    /// Handles from.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<MetricSample>,
}

/// Thread-safe cache of the most recent status snapshot.
#[derive(Clone)]
pub struct StatusCache {
    inner: Arc<RwLock<StatusSnapshot>>,
}

impl StatusCache {
    /// Handles new.
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
    /// Handles spawn.
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

    /// Stops this item.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for StatusRefresher {
    /// Handles drop.
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
    mode: StatusSnapshotMode,
) -> Result<StatusSnapshot, StatusError> {
    collect_runtime_snapshot_with_cron_hashes(
        config,
        pid_file,
        service_state,
        metrics,
        spawn_manager,
        mode,
        None,
    )
}

/// Builds a fresh snapshot using a caller-provided set of valid cron hashes.
pub(crate) fn collect_runtime_snapshot_with_cron_hashes(
    config: Arc<Config>,
    pid_file: &Arc<Mutex<PidFile>>,
    service_state: &Arc<Mutex<ServiceStateFile>>,
    metrics: Option<&MetricsHandle>,
    spawn_manager: Option<&DynamicSpawnManager>,
    mode: StatusSnapshotMode,
    _valid_cron_hashes: Option<&HashSet<String>>,
) -> Result<StatusSnapshot, StatusError> {
    let store = StateStore::for_project(&config.project.id);
    let mut cron_state = CronStateFile::load(store)?;
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
        mode,
    ))
}

/// Returns service hashes for cron-managed services in a config.
pub(crate) fn cron_hashes_for_config(config: &Config) -> HashSet<String> {
    config
        .services
        .iter()
        .filter(|(_, svc)| svc.cron.is_some())
        .map(|(_, svc)| svc.compute_hash())
        .collect()
}

/// Builds a snapshot purely from persisted state on disk.
pub fn collect_disk_snapshot(
    config: Option<Config>,
) -> Result<StatusSnapshot, StatusError> {
    let store = match config.as_ref() {
        Some(c) => StateStore::for_project(&c.project.id),
        None => StateStore::loose(),
    };
    let pid_file = PidFile::load(store.clone())?;
    let mut service_state = ServiceStateFile::load(store.clone())?;
    let mut cron_state = CronStateFile::load(store)?;
    let config_ref = config.as_ref();

    Ok(build_snapshot(
        config_ref,
        &pid_file,
        &mut service_state,
        &mut cron_state,
        None,
        None,
        StatusSnapshotMode::Detailed,
    ))
}

/// Maps each cron service name to the persisted hash whose state is most recent.
///
/// A service's config hash changes whenever its config is edited, so history and
/// metrics persisted under an older hash would otherwise be unreachable. This lets
/// callers recover that state by service name.
fn reconcile_cron_hashes_by_name(cron_state: &CronStateFile) -> HashMap<String, String> {
    let mut best: HashMap<String, (String, Option<SystemTime>)> = HashMap::new();
    for (hash, job) in cron_state.jobs() {
        let Some(name) = job.service_name.as_deref() else {
            continue;
        };
        let candidate = job
            .last_execution
            .or_else(|| job.execution_history.back().map(|record| record.started_at));
        match best.get(name) {
            Some((_, existing)) if *existing >= candidate => {}
            _ => {
                best.insert(name.to_string(), (hash.clone(), candidate));
            }
        }
    }
    best.into_iter()
        .map(|(name, (hash, _))| (name, hash))
        .collect()
}

/// Builds snapshot.
fn build_snapshot(
    config: Option<&Config>,
    pid_file: &PidFile,
    service_state: &mut ServiceStateFile,
    cron_state: &mut CronStateFile,
    metrics_store: Option<&MetricsStore>,
    spawn_manager: Option<&DynamicSpawnManager>,
    mode: StatusSnapshotMode,
) -> StatusSnapshot {
    let mut hash_to_name: HashMap<String, String> = HashMap::new();
    let mut hash_kind: HashMap<String, UnitKind> = HashMap::new();
    let mut unit_hashes: BTreeSet<String> = BTreeSet::new();
    let project_status = config.map(|cfg| ProjectStatus::from(&cfg.project));

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

    let cron_hash_by_name = reconcile_cron_hashes_by_name(cron_state);

    let mut units = Vec::new();

    let process_system = if matches!(mode, StatusSnapshotMode::Detailed) {
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        Some(sys)
    } else {
        None
    };
    let process_index = process_system.as_ref().map(ProcessIndex::new);

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

        let mut process_runtime = if matches!(mode, StatusSnapshotMode::Off) {
            None
        } else {
            pid.map(|pid| ProcessRuntime {
                pid,
                state: StatusManager::process_state(pid),
                user: if matches!(mode, StatusSnapshotMode::Detailed) {
                    Some(StatusManager::get_process_user(pid))
                } else {
                    None
                },
            })
        };

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

        if lifecycle.is_none() && actual_name.is_some() && kind != UnitKind::Orphaned {
            lifecycle = Some(ServiceLifecycleStatus::Stopped);
        }

        let uptime = if matches!(mode, StatusSnapshotMode::Detailed) {
            match process_runtime.as_ref() {
                Some(runtime) if matches!(runtime.state, ProcessState::Running) => {
                    compute_uptime(runtime.pid)
                }
                _ => None,
            }
        } else {
            None
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

        let cron_hash = if cron_state.jobs().contains_key(&hash) {
            Some(hash.clone())
        } else if kind == UnitKind::Cron {
            actual_name
                .as_deref()
                .and_then(|name| cron_hash_by_name.get(name).cloned())
        } else {
            None
        };

        let cron = cron_hash
            .as_ref()
            .and_then(|cron_hash| cron_state.jobs().get(cron_hash))
            .map(|job| {
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

        let service_config =
            config.and_then(|cfg| cfg.services.get(actual_name.as_deref().unwrap_or("")));
        let intent = derive_unit_intent(kind, service_config);

        if let Some(runtime) = process_runtime.as_ref()
            && matches!(runtime.state, ProcessState::Missing)
            && missing_pid_is_expected(kind, intent, lifecycle, cron.as_ref())
        {
            process_runtime = None;
        }

        let state =
            derive_unit_state(kind, lifecycle, process_runtime.as_ref(), cron.as_ref());
        let health = derive_unit_health(
            kind,
            state,
            intent,
            lifecycle,
            process_runtime.as_ref(),
            cron.as_ref(),
        );
        let metrics_summary = metrics_store
            .and_then(|store| {
                store.summarize_unit(&hash).or_else(|| {
                    cron_hash
                        .as_deref()
                        .filter(|cron_hash| *cron_hash != hash)
                        .and_then(|cron_hash| store.summarize_unit(cron_hash))
                })
            })
            .map(UnitMetricsSummary::from);

        let command = service_config.map(|service_config| service_config.command.clone());
        let runtime_command = if matches!(mode, StatusSnapshotMode::Detailed) {
            process_runtime
                .as_ref()
                .map(|runtime| StatusManager::get_process_cmdline(runtime.pid))
                .filter(|cmd| !cmd.is_empty())
        } else {
            None
        };
        let service_hash_for_spawn = if matches!(kind, UnitKind::Service | UnitKind::Cron)
        {
            Some(hash.as_str())
        } else {
            None
        };

        let spawned_children = if matches!(mode, StatusSnapshotMode::Detailed)
            && let Some(runtime) = &process_runtime
        {
            let mut seen = HashSet::new();
            seen.insert(runtime.pid);

            let mut nodes = Vec::new();

            if let Some(manager) = spawn_manager {
                let managed =
                    build_spawn_tree(manager, runtime.pid, process_system.as_ref());
                collect_tracked_pids(&managed, &mut seen);
                nodes.extend(managed);
            }

            let pidfile_system = if spawn_manager.is_some() {
                process_system.as_ref()
            } else {
                None
            };
            let pidfile_nodes = build_spawn_tree_from_pidfile(
                pid_file,
                runtime.pid,
                service_hash_for_spawn,
                true,
                pidfile_system,
            );
            append_unique_nodes(&mut nodes, pidfile_nodes, &mut seen);

            let system_nodes = build_spawn_tree_from_system(
                process_index.as_ref(),
                runtime.pid,
                1,
                &mut seen,
            );
            append_unique_nodes(&mut nodes, system_nodes, &mut seen);

            if let Some(index) = process_index.as_ref() {
                for node in &mut nodes {
                    augment_spawn_tree_with_system_descendants(
                        node,
                        Some(index),
                        &mut seen,
                    );
                }
            }

            nodes
        } else {
            Vec::new()
        };

        units.push(UnitStatus {
            name: display_name,
            hash,
            project: project_status.clone(),
            kind,
            lifecycle,
            state,
            intent,
            health,
            process: process_runtime,
            uptime,
            last_exit,
            cron,
            metrics: metrics_summary,
            command,
            runtime_command,
            spawned_children,
        });
    }

    for (service_name, &pid_value) in pid_file.services() {
        if hash_to_name.values().any(|name| name == service_name) {
            continue;
        }

        let runtime = if matches!(mode, StatusSnapshotMode::Off) {
            None
        } else {
            Some(ProcessRuntime {
                pid: pid_value,
                state: StatusManager::process_state(pid_value),
                user: if matches!(mode, StatusSnapshotMode::Detailed) {
                    Some(StatusManager::get_process_user(pid_value))
                } else {
                    None
                },
            })
        };
        let uptime = if matches!(mode, StatusSnapshotMode::Detailed)
            && let Some(runtime) = runtime.as_ref()
            && matches!(runtime.state, ProcessState::Running)
        {
            compute_uptime(runtime.pid)
        } else {
            None
        };

        let health = match runtime.as_ref().map(|runtime| runtime.state) {
            Some(ProcessState::Running) => UnitHealth::Healthy,
            Some(ProcessState::Zombie | ProcessState::Missing) => UnitHealth::Failing,
            None => UnitHealth::Idle,
        };
        let state = derive_unit_state(UnitKind::Orphaned, None, runtime.as_ref(), None);
        let intent = UnitIntent::Orphan;

        let metrics_summary = metrics_store
            .and_then(|store| store.summarize_unit(service_name))
            .map(UnitMetricsSummary::from);

        let mut spawned_children = if matches!(mode, StatusSnapshotMode::Detailed)
            && let Some(manager) = spawn_manager
        {
            build_spawn_tree(manager, pid_value, process_system.as_ref())
        } else {
            Vec::new()
        };

        if matches!(mode, StatusSnapshotMode::Detailed) && spawned_children.is_empty() {
            let pidfile_system = if spawn_manager.is_some() {
                process_system.as_ref()
            } else {
                None
            };
            spawned_children = build_spawn_tree_from_pidfile(
                pid_file,
                pid_value,
                None,
                true,
                pidfile_system,
            );
        }

        units.push(UnitStatus {
            name: service_name.clone(),
            hash: service_name.clone(),
            project: None,
            kind: UnitKind::Orphaned,
            lifecycle: None,
            state,
            intent,
            health,
            process: runtime,
            uptime,
            last_exit: None,
            cron: None,
            metrics: metrics_summary,
            command: None,
            runtime_command: if matches!(mode, StatusSnapshotMode::Detailed) {
                Some(StatusManager::get_process_cmdline(pid_value))
                    .filter(|cmd| !cmd.is_empty())
            } else {
                None
            },
            spawned_children,
        });
    }

    StatusSnapshot::new(units)
}

/// Handles cron record to summary.
fn cron_record_to_summary(record: &CronExecutionRecord) -> CronExecutionSummary {
    CronExecutionSummary {
        started_at: DateTime::<Utc>::from(record.started_at),
        completed_at: record.completed_at.map(DateTime::<Utc>::from),
        status: record.status.clone(),
        exit_code: record.exit_code,
        pid: record.pid,
        user: record.user.clone(),
        command: record.command.clone(),
        metrics: record.metrics.clone(),
    }
}

/// Returns whether a missing PID is expected for a unit that has already
/// finished its work.
///
/// Cron jobs and one-shot services are not meant to keep a process alive, so a
/// PID that has since exited is stale runtime evidence rather than a fault. Once
/// such a unit has terminal lifecycle state or recorded cron history, its state
/// and health should be derived from those facts instead of the dead PID.
fn missing_pid_is_expected(
    kind: UnitKind,
    intent: UnitIntent,
    lifecycle: Option<ServiceLifecycleStatus>,
    cron: Option<&CronUnitStatus>,
) -> bool {
    if matches!(kind, UnitKind::Cron) {
        return cron.is_some_and(|cron| cron.last_run.is_some());
    }

    if matches!(intent, UnitIntent::Once) {
        return matches!(
            lifecycle,
            Some(
                ServiceLifecycleStatus::ExitedSuccessfully
                    | ServiceLifecycleStatus::ExitedWithError
                    | ServiceLifecycleStatus::Stopped
                    | ServiceLifecycleStatus::Skipped
            )
        );
    }

    false
}

/// Derives the factual state shown to operators.
fn derive_unit_state(
    kind: UnitKind,
    lifecycle: Option<ServiceLifecycleStatus>,
    runtime: Option<&ProcessRuntime>,
    cron: Option<&CronUnitStatus>,
) -> UnitState {
    if let Some(runtime) = runtime {
        return match runtime.state {
            ProcessState::Running => UnitState::Running,
            ProcessState::Zombie => UnitState::Zombie,
            ProcessState::Missing => UnitState::Lost,
        };
    }

    if let Some(cron_status) = cron {
        if let Some(last) = cron_status.last_run.as_ref()
            && let Some(status) = &last.status
        {
            return match status {
                CronExecutionStatus::Success => UnitState::Done,
                CronExecutionStatus::Failed(reason) => {
                    if reason.contains("Failed to get PID") {
                        UnitState::Queued
                    } else {
                        UnitState::Failed
                    }
                }
                CronExecutionStatus::OverlapError => UnitState::Overlap,
            };
        }

        return UnitState::Queued;
    }

    match lifecycle {
        Some(ServiceLifecycleStatus::Running) => UnitState::Running,
        Some(ServiceLifecycleStatus::ExitedSuccessfully) => UnitState::Done,
        Some(ServiceLifecycleStatus::ExitedWithError) => UnitState::Failed,
        Some(ServiceLifecycleStatus::Stopped) => UnitState::Stopped,
        Some(ServiceLifecycleStatus::Skipped) => UnitState::Skipped,
        None if matches!(kind, UnitKind::Cron) => UnitState::Queued,
        None => UnitState::Unknown,
    }
}

/// Derives what the supervisor expects from a unit.
fn derive_unit_intent(
    kind: UnitKind,
    service_config: Option<&ServiceConfig>,
) -> UnitIntent {
    match kind {
        UnitKind::Cron => UnitIntent::Cron,
        UnitKind::Orphaned => UnitIntent::Orphan,
        UnitKind::Service => {
            let Some(service_config) = service_config else {
                return UnitIntent::Manual;
            };

            if service_config.skip.is_some() {
                return UnitIntent::Skip;
            }

            if matches!(service_config.restart_policy.as_deref(), Some("never")) {
                return UnitIntent::Once;
            }

            UnitIntent::Serve
        }
    }
}

/// Derives the operator-action health classification for a unit.
fn derive_unit_health(
    kind: UnitKind,
    state: UnitState,
    intent: UnitIntent,
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
                if matches!(intent, UnitIntent::Serve) {
                    return UnitHealth::Warn;
                }
            }
        }
    }

    if let Some(cron_status) = cron {
        if let Some(last) = cron_status.last_run.as_ref()
            && let Some(status) = &last.status
        {
            return match status {
                CronExecutionStatus::Success => UnitHealth::Healthy,
                CronExecutionStatus::Failed(reason) => {
                    if reason.contains("Failed to get PID") {
                        UnitHealth::Idle
                    } else {
                        UnitHealth::Failing
                    }
                }
                CronExecutionStatus::OverlapError => UnitHealth::Warn,
            };
        }

        return UnitHealth::Idle;
    }

    match lifecycle {
        Some(ServiceLifecycleStatus::ExitedWithError) => return UnitHealth::Failing,
        Some(ServiceLifecycleStatus::Running) => return UnitHealth::Healthy,
        Some(ServiceLifecycleStatus::Skipped) => {
            return UnitHealth::Idle;
        }
        Some(ServiceLifecycleStatus::Stopped) => {
            if matches!(intent, UnitIntent::Serve) {
                return UnitHealth::Warn;
            }
            return UnitHealth::Idle;
        }
        Some(ServiceLifecycleStatus::ExitedSuccessfully) => {
            return UnitHealth::Healthy;
        }
        None => {}
    }

    match (state, kind) {
        (UnitState::Lost, _) => UnitHealth::Warn,
        (UnitState::Zombie | UnitState::Failed, _) => UnitHealth::Failing,
        (UnitState::Queued | UnitState::Stopped | UnitState::Skipped, _) => {
            UnitHealth::Idle
        }
        (_, UnitKind::Cron | UnitKind::Service | UnitKind::Orphaned) => UnitHealth::Warn,
    }
}

/// Human-readable explanation of why a unit holds its current [`UnitHealth`].
///
/// Each field maps onto a section of the README-style health report rendered by
/// the `sysg status` interactive view: a title, a 0-10 severity, a one-line
/// summary, a longer description of the cause, and a concrete recommended fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// Health classification the report explains.
    pub health: UnitHealth,
    /// Coarse 0-10 urgency, increasing from healthy to hard failure.
    pub severity: u8,
    /// Short title summarizing the condition.
    pub title: String,
    /// One-line summary suitable for a `TLDR:` line.
    pub tldr: String,
    /// Longer prose describing what happened and why it carries this health.
    pub description: String,
    /// Concrete next step, including the command to run and why it helps.
    pub recommended_fix: String,
}

/// Formats the recorded exit of a unit as a short human phrase.
fn describe_exit(exit: Option<&ExitMetadata>) -> Option<String> {
    let exit = exit?;
    if let Some(signal) = exit.signal {
        Some(format!("terminated by signal {signal}"))
    } else {
        exit.exit_code
            .map(|code| format!("exited with code {code}"))
    }
}

/// Builds a human-readable explanation of a unit's current health.
///
/// The branch order mirrors `derive_unit_health` so the explanation always
/// matches the verdict shown in the status table.
pub fn explain_unit_health(unit: &UnitStatus) -> HealthReport {
    let name = unit.name.as_str();
    let restart = format!("sysg restart -s {name} --log-level debug");
    let logs = format!("sysg logs -s {name} -l 200");

    if let Some(runtime) = unit.process.as_ref() {
        match runtime.state {
            ProcessState::Running => {
                return HealthReport {
                    health: UnitHealth::Healthy,
                    severity: 0,
                    title: format!("'{name}' is healthy"),
                    tldr: "The tracked process is alive and doing its job.".to_string(),
                    description: format!(
                        "systemg observed a live process (PID {}) for '{name}', \
which matches its intent. Nothing requires attention.",
                        runtime.pid
                    ),
                    recommended_fix: "No action needed.".to_string(),
                };
            }
            ProcessState::Zombie => {
                return HealthReport {
                    health: UnitHealth::Failing,
                    severity: 8,
                    title: format!("'{name}' is a zombie process"),
                    tldr: "The process died but was never reaped by its parent."
                        .to_string(),
                    description: format!(
                        "The tracked process (PID {}) for '{name}' is defunct: it \
has exited but its parent has not collected the exit status, so it lingers in \
the process table. A zombie cannot do work and usually signals that the \
supervising parent is stuck or mis-handling child reaping.",
                        runtime.pid
                    ),
                    recommended_fix: format!(
                        "Restart the unit so systemg respawns a clean process:\n\n    \
{restart}\n\nIf zombies keep reappearing, inspect the parent process; a \
parent that never calls wait() will keep leaking them."
                    ),
                };
            }
            ProcessState::Missing => {
                if matches!(unit.intent, UnitIntent::Serve) {
                    return HealthReport {
                        health: UnitHealth::Warn,
                        severity: 5,
                        title: format!("'{name}' has a tracked PID but no process"),
                        tldr: "systemg expected a running process and could not find it."
                            .to_string(),
                        description: format!(
                            "systemg still holds PID {} for '{name}', but that PID is no \
longer present in the process table. The process likely crashed or was killed \
out from under the supervisor without a clean lifecycle transition.",
                            runtime.pid
                        ),
                        recommended_fix: format!(
                            "Check why the process vanished, then restart it:\n\n    \
{logs}\n    {restart}"
                        ),
                    };
                }
            }
        }
    }

    if let Some(cron_status) = unit.cron.as_ref() {
        if let Some(last) = cron_status.last_run.as_ref()
            && let Some(status) = &last.status
        {
            return match status {
                CronExecutionStatus::Success => HealthReport {
                    health: UnitHealth::Healthy,
                    severity: 0,
                    title: format!("'{name}' last cron run succeeded"),
                    tldr: "The most recent scheduled run completed cleanly.".to_string(),
                    description: format!(
                        "The last run of '{name}' finished successfully and the unit \
is waiting for its next scheduled trigger."
                    ),
                    recommended_fix: "No action needed.".to_string(),
                },
                CronExecutionStatus::Failed(reason)
                    if reason.contains("Failed to get PID") =>
                {
                    HealthReport {
                        health: UnitHealth::Idle,
                        severity: 2,
                        title: format!("'{name}' cron run finished too fast to track"),
                        tldr: "The job exited before systemg could capture its PID."
                            .to_string(),
                        description: format!(
                            "systemg could not attach a PID to the last run of '{name}' \
because it completed almost immediately. This is usually harmless for very \
short jobs, but it means runtime metrics were not collected for that run."
                        ),
                        recommended_fix: format!(
                            "If the job is meant to be short-lived, no action is needed. \
To confirm it did its work, check the output:\n\n    {logs}"
                        ),
                    }
                }
                CronExecutionStatus::Failed(reason) => {
                    let detail = if reason.trim().is_empty() {
                        "no failure reason was recorded".to_string()
                    } else {
                        format!("the recorded reason was: {reason}")
                    };
                    HealthReport {
                        health: UnitHealth::Failing,
                        severity: 7,
                        title: format!("'{name}' last cron run failed"),
                        tldr: "The most recent scheduled run ended in failure."
                            .to_string(),
                        description: format!(
                            "The last run of '{name}' failed and {detail}. The unit will \
still fire on its next schedule, but the failed run did not complete its work."
                        ),
                        recommended_fix: format!(
                            "Read the run output to find the root cause, then trigger a \
manual run once fixed:\n\n    {logs}\n    {restart}"
                        ),
                    }
                }
                CronExecutionStatus::OverlapError => HealthReport {
                    health: UnitHealth::Warn,
                    severity: 4,
                    title: format!("'{name}' cron run overlapped a prior run"),
                    tldr:
                        "A new run was skipped because the previous one was still going."
                            .to_string(),
                    description: format!(
                        "A scheduled run of '{name}' was blocked because the previous \
run had not finished. The job is taking longer than its interval, so runs are \
piling up against each other."
                    ),
                    recommended_fix: format!(
                        "Either widen the schedule interval or make the job faster. \
Inspect how long runs take:\n\n    {logs}"
                    ),
                },
            };
        }

        return HealthReport {
            health: UnitHealth::Idle,
            severity: 1,
            title: format!("'{name}' has no completed cron runs yet"),
            tldr: "The cron unit is scheduled but has not run.".to_string(),
            description: format!(
                "'{name}' is registered as a cron unit but has not completed a run \
yet, so there is nothing to evaluate. It will run at its next scheduled time."
            ),
            recommended_fix: "No action needed; wait for the next scheduled run."
                .to_string(),
        };
    }

    match unit.lifecycle {
        Some(ServiceLifecycleStatus::ExitedWithError) => {
            let exit = describe_exit(unit.last_exit.as_ref())
                .map(|phrase| format!(" It {phrase}."))
                .unwrap_or_default();
            return HealthReport {
                health: UnitHealth::Failing,
                severity: 8,
                title: format!("'{name}' exited with an error"),
                tldr: "The service stopped because it failed.".to_string(),
                description: format!(
                    "'{name}' is no longer running because it exited unsuccessfully.{exit} \
A service that should stay available has crashed or returned a non-zero status."
                ),
                recommended_fix: format!(
                    "Read the logs to find why it failed, then restart it:\n\n    \
{logs}\n    {restart}"
                ),
            };
        }
        Some(ServiceLifecycleStatus::Running) => {
            return HealthReport {
                health: UnitHealth::Healthy,
                severity: 0,
                title: format!("'{name}' is running"),
                tldr: "The service is up and matches its intent.".to_string(),
                description: format!("'{name}' is running normally."),
                recommended_fix: "No action needed.".to_string(),
            };
        }
        Some(ServiceLifecycleStatus::Skipped) => {
            return HealthReport {
                health: UnitHealth::Idle,
                severity: 1,
                title: format!("'{name}' was skipped"),
                tldr: "A skip rule kept this unit from starting.".to_string(),
                description: format!(
                    "'{name}' did not start because a configured skip rule matched. \
This is intentional and does not indicate a problem."
                ),
                recommended_fix: "No action needed unless you expected it to run; \
in that case review the unit's skip condition in your config."
                    .to_string(),
            };
        }
        Some(ServiceLifecycleStatus::Stopped) => {
            if matches!(unit.intent, UnitIntent::Serve) {
                return HealthReport {
                    health: UnitHealth::Warn,
                    severity: 5,
                    title: format!("'{name}' is stopped but should be serving"),
                    tldr: "A long-running service is intentionally stopped.".to_string(),
                    description: format!(
                        "'{name}' has intent 'Serve', meaning it is expected to stay \
available, but it is currently stopped. Nothing is serving requests for this \
unit right now."
                    ),
                    recommended_fix: format!(
                        "Start it again if it should be up:\n\n    {restart}\n\nIf it is \
meant to stay down, this is expected and can be ignored."
                    ),
                };
            }
            return HealthReport {
                health: UnitHealth::Idle,
                severity: 1,
                title: format!("'{name}' is stopped"),
                tldr: "The unit is stopped, which is allowed for its intent.".to_string(),
                description: format!(
                    "'{name}' is stopped and its intent does not require it to stay \
running, so this is an acceptable resting state."
                ),
                recommended_fix: "No action needed.".to_string(),
            };
        }
        Some(ServiceLifecycleStatus::ExitedSuccessfully) => {
            return HealthReport {
                health: UnitHealth::Healthy,
                severity: 0,
                title: format!("'{name}' completed successfully"),
                tldr: "The unit exited cleanly with status 0.".to_string(),
                description: format!(
                    "'{name}' ran to completion and exited successfully. A clean exit \
does not require operator action."
                ),
                recommended_fix: "No action needed.".to_string(),
            };
        }
        None => {}
    }

    match (unit.state, unit.kind) {
        (UnitState::Lost, _) => HealthReport {
            health: UnitHealth::Warn,
            severity: 5,
            title: format!("'{name}' was lost"),
            tldr: "systemg expected a tracked process and can no longer find it."
                .to_string(),
            description: format!(
                "systemg held state for '{name}' but the process it expected is gone, \
with no clean lifecycle transition recorded. It may have been killed externally."
            ),
            recommended_fix: format!(
                "Check the logs and restart to get back to a known state:\n\n    \
{logs}\n    {restart}"
            ),
        },
        (UnitState::Zombie | UnitState::Failed, _) => HealthReport {
            health: UnitHealth::Failing,
            severity: 8,
            title: format!("'{name}' is in a failed state"),
            tldr: "The unit failed or is defunct.".to_string(),
            description: format!(
                "'{name}' is in a failed or defunct state and is not doing its job. \
This requires attention to recover."
            ),
            recommended_fix: format!(
                "Inspect the logs and restart:\n\n    {logs}\n    {restart}"
            ),
        },
        (UnitState::Queued | UnitState::Stopped | UnitState::Skipped, _) => {
            HealthReport {
                health: UnitHealth::Idle,
                severity: 1,
                title: format!("'{name}' is idle"),
                tldr: "The unit is inactive by design.".to_string(),
                description: format!(
                    "'{name}' is queued, stopped, or skipped and does not currently \
require any action."
                ),
                recommended_fix: "No action needed.".to_string(),
            }
        }
        (_, UnitKind::Cron | UnitKind::Service | UnitKind::Orphaned) => HealthReport {
            health: UnitHealth::Warn,
            severity: 4,
            title: format!("'{name}' is not in its expected state"),
            tldr: "systemg has no clear evidence the unit is doing its job.".to_string(),
            description: format!(
                "systemg could not match '{name}' to a healthy condition. There is no \
reliable runtime or lifecycle fact confirming it is doing what its intent \
requires, so it is flagged for a look."
            ),
            recommended_fix: format!(
                "Inspect the unit and its logs to confirm its state:\n\n    \
sysg inspect -s {name}\n    {logs}"
            ),
        },
    }
}

/// Computes overall health.
pub fn compute_overall_health(units: &[UnitStatus]) -> OverallHealth {
    if units
        .iter()
        .any(|unit| matches!(unit.health, UnitHealth::Failing))
    {
        return OverallHealth::Failing;
    }

    if units
        .iter()
        .any(|unit| matches!(unit.health, UnitHealth::Warn))
    {
        return OverallHealth::Warn;
    }

    OverallHealth::Healthy
}

/// Truncates hash.
fn truncate_hash(hash: &str) -> String {
    let prefix_length = hash.len().min(12);
    hash[..prefix_length].to_string()
}

/// Computes uptime.
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

/// Parses elapsed seconds.
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

/// Formats elapsed.
pub fn format_elapsed(total_seconds: u64) -> String {
    match total_seconds {
        0..=59 => format!("{} secs ago", total_seconds),
        60..=3_599 => format!("{} mins ago", total_seconds / 60),
        3_600..=86_399 => format!("{} hours ago", total_seconds / 3_600),
        86_400..=604_799 => format!("{} days ago", total_seconds / 86_400),
        _ => format!("{} weeks ago", total_seconds / 604_800),
    }
}

/// Represents the state of a process in the system.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    /// Process is currently running.
    Running,
    /// Process has terminated but not been reaped by its parent.
    Zombie,
    /// Process is not found in the process table.
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

    /// Clears service pid.
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

    /// Marks service running.
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

    /// Handles process state.
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
                if Self::process_group_has_live_member(pid) {
                    return ProcessState::Running;
                }
                return ProcessState::Zombie;
            }

            ProcessState::Running
        }

        #[cfg(not(target_os = "linux"))]
        {
            let target = Pid::from_raw(pid as i32);
            match signal::kill(target, None) {
                Ok(_) => {
                    if matches!(Self::read_proc_state(pid), Some('Z') | Some('X')) {
                        ProcessState::Zombie
                    } else {
                        ProcessState::Running
                    }
                }
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

    /// Returns true when the recorded PID's process group still has a live, non-zombie member
    /// other than the recorded PID itself. A wrapper shell can exit while the real worker keeps
    /// running (reparented to PID 1 but retaining the original group); in that case the recorded
    /// PID reads as a zombie even though the service is genuinely healthy.
    ///
    /// This only applies when the recorded PID is its own process-group leader (`pid == pgid`),
    /// which systemg guarantees for service wrappers via `setpgid(0, 0)`. A bare process that
    /// merely inherited an unrelated group (e.g. the supervisor's own) is a genuine zombie.
    #[cfg(target_os = "linux")]
    fn process_group_has_live_member(pid: u32) -> bool {
        let Ok(pgid) = getpgid(Some(Pid::from_raw(pid as i32))) else {
            return false;
        };
        let pgid = pgid.as_raw();

        if pgid != pid as i32 {
            return false;
        }

        let Ok(entries) = fs::read_dir("/proc") else {
            return false;
        };
        for entry in entries.filter_map(Result::ok) {
            let Some(other) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if other == pid {
                continue;
            }
            let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some(close_paren) = stat.rfind(')') else {
                continue;
            };
            let mut fields = stat[close_paren + 1..].split_whitespace();
            let state = fields.next().and_then(|raw| raw.chars().next());
            if matches!(state, Some('Z') | Some('X')) {
                continue;
            }
            let _ppid = fields.next();
            let Some(group) = fields.next().and_then(|raw| raw.parse::<i32>().ok())
            else {
                continue;
            };
            if group == pgid {
                return true;
            }
        }
        false
    }

    #[cfg(target_os = "linux")]
    /// Reads proc state.
    fn read_proc_state(pid: u32) -> Option<char> {
        let stat_path_str = format!("/proc/{pid}/stat");
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

    #[cfg(not(target_os = "linux"))]
    /// Reads the process state character via `ps` on non-Linux Unix platforms.
    fn read_proc_state(pid: u32) -> Option<char> {
        let output = std::process::Command::new("ps")
            .args(["-o", "state=", "-p"])
            .arg(pid.to_string())
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .chars()
            .next()
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
                let proc_name = Self::get_process_cmdline(proc_pid.as_u32());
                let formatted = format!(
                    "{} ├─{} {}",
                    " ".repeat(indent),
                    proc_pid.as_u32(),
                    proc_name
                );
                children.push(formatted);

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
                    return "";
                }
            }
        }

        if is_cron {
            let store = self
                .state_file
                .lock()
                .map(|g| g.store())
                .unwrap_or_else(|_| StateStore::loose());
            let cron_state = CronStateFile::load(store).unwrap_or_default();
            if let Some(job_state) = cron_state.jobs().get(service_hash)
                && let Some(last_execution) = job_state.execution_history.back()
            {
                match last_execution.status.as_ref() {
                    Some(CronExecutionStatus::Success) => return GREEN_BOLD,
                    Some(CronExecutionStatus::Failed(_))
                    | Some(CronExecutionStatus::OverlapError) => {
                        return RED_BOLD;
                    }
                    None => {}
                }
            }
        }

        ""
    }

    /// Shows status with cron info.
    fn show_status_with_cron_info(
        &self,
        service_name: &str,
        is_cron: bool,
        config_path: Option<&str>,
    ) {
        if let Ok(config) = crate::config::load_config(config_path)
            && let Some(service_config) = config.services.get(service_name)
        {
            let service_hash = service_config.compute_hash();
            self.show_status_with_cron_info_by_hash(service_name, &service_hash, is_cron);
            return;
        }
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

        let store = self
            .state_file
            .lock()
            .map(|g| g.store())
            .unwrap_or_else(|_| StateStore::loose());
        let cron_state = CronStateFile::load(store).unwrap_or_default();
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
        let store = StateStore::for_project(&config.project.id);
        let cron_state = CronStateFile::load(store).unwrap_or_default();

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

    /// Handles print cron history.
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

        println!();
    }

    /// Formats cron status.
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

    /// Formats cron timestamp.
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

    /// Gets the **process owner username**.
    fn get_process_user(pid: u32) -> String {
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("user=")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Unknown".to_string())
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
            .map(|s| {
                s.trim()
                    .chars()
                    .map(|ch| if ch.is_control() { ' ' } else { ch })
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_else(|| "Unknown".to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        process::Command as StdCommand,
        sync::{Arc, Mutex},
        time::SystemTime,
    };

    use serde_json::json;
    use tempfile::tempdir_in;

    use super::*;
    use crate::{daemon::PersistedSpawnChild, spawn::SpawnedExit};

    #[test]
    fn process_index_maps_children_from_single_refresh() {
        let mut child = StdCommand::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn child process");
        let parent_pid = std::process::id();

        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);
        let index = ProcessIndex::new(&system);
        let found = index.child_pids(parent_pid).any(|pid| pid == child.id());

        let _ = child.kill();
        let _ = child.wait();

        assert!(found, "process index should map parent pid to child pid");
    }

    #[test]
    fn format_cron_status_success_includes_green_exit_code() {
        let record = CronExecutionRecord {
            started_at: SystemTime::now(),
            completed_at: Some(SystemTime::now()),
            status: Some(CronExecutionStatus::Success),
            exit_code: Some(0),
            pid: None,
            user: None,
            command: None,
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
            pid: None,
            user: None,
            command: None,
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
            "services": [],
            "service_groups": [],
            "parent_map": [{ "child": child_pid, "parent": parent_pid }],
            "children_map": [{ "parent": parent_pid, "children": [child_pid] }],
            "spawn_depth": [{ "pid": child_pid, "depth": 1 }],
            "spawn_metadata": [{ "pid": child_pid, "metadata": metadata }],
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

        let pid_file: PidFile = serde_json::from_value(json!({
            "services": [],
            "service_groups": [],
            "parent_map": [],
            "children_map": [],
            "spawn_depth": [
                { "pid": team_lead_pid, "depth": 1 },
                { "pid": core_pid, "depth": 2 },
                { "pid": ui_pid, "depth": 2 }
            ],
            "spawn_metadata": [
                { "pid": team_lead_pid, "metadata": team_lead },
                { "pid": core_pid, "metadata": core_infra },
                { "pid": ui_pid, "metadata": ui_dev }
            ],
        }))
        .expect("deserialize pid file");

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

        let store = StateStore::for_project("test");
        let pid_file = Arc::new(Mutex::new(
            PidFile::load(store.clone()).expect("load pid file"),
        ));
        {
            let mut guard = pid_file.lock().expect("lock pid file");
            guard.insert("demo_service", 42).expect("insert pid entry");
        }

        let service_hash = "deadbeefdeadbeef";
        let state_file = Arc::new(Mutex::new(
            ServiceStateFile::load(store).expect("load state file"),
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

        let store = StateStore::for_project(&config.project.id);
        let mut pid_file = PidFile::load(store.clone()).expect("load pid file");
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

        let mut state = ServiceStateFile::load(store).expect("load state");
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
            version: crate::config::Version::V1,
            project: crate::config::ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: crate::config::MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        let pid_file = PidFile::default();
        let mut service_state = ServiceStateFile::default();

        let cron_state_json = json!({
            "jobs": [{
                "hash": "deadbeefdeadbeef",
                "state": {
                    "last_execution": 0,  // 0 represents None for XML compatibility
                    "execution_history": [],
                    "timezone_label": "UTC",
                    "timezone": "UTC"
                }
            }]
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
            StatusSnapshotMode::Off,
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
    fn build_snapshot_completed_cron_with_stale_pid_renders_done() {
        let mut services = std::collections::HashMap::new();
        let service = crate::config::ServiceConfig {
            command: "/bin/echo hi".into(),
            cron: Some(crate::config::CronConfig {
                expression: "* * * * *".into(),
                timezone: None,
            }),
            ..crate::config::ServiceConfig::default()
        };
        let hash = service.compute_hash();
        services.insert("nightly".into(), service);
        let config = Config {
            version: crate::config::Version::V1,
            project: crate::config::ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: crate::config::MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        let mut pid_file = PidFile::default();
        pid_file.insert_in_memory("nightly", 2_000_000_000);

        let mut service_state = ServiceStateFile::default();
        let cron_state_json = json!({
            "jobs": [{
                "hash": hash,
                "state": {
                    "last_execution": 1,
                    "execution_history": [{
                        "started_at": 1,
                        "completed_at": 2,
                        "status": "Success",
                        "exit_code": 0,
                        "pid": 2_000_000_000u32,
                    }],
                    "timezone_label": "UTC",
                    "timezone": "UTC"
                }
            }]
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
            StatusSnapshotMode::Summary,
        );

        let unit = snapshot
            .units
            .iter()
            .find(|unit| unit.name == "nightly")
            .expect("nightly unit");

        assert!(
            unit.process.is_none(),
            "stale missing PID should not attach to a completed cron unit"
        );
        assert_eq!(unit.state, UnitState::Done);
        assert_eq!(unit.health, UnitHealth::Healthy);
    }

    #[test]
    fn build_snapshot_recovers_cron_history_under_stale_hash() {
        let mut services = std::collections::HashMap::new();
        let service = crate::config::ServiceConfig {
            command: "/bin/echo hi".into(),
            cron: Some(crate::config::CronConfig {
                expression: "* * * * *".into(),
                timezone: Some("UTC".into()),
            }),
            ..crate::config::ServiceConfig::default()
        };
        let live_hash = service.compute_hash();
        services.insert("nightly".into(), service);
        let config = Config {
            version: crate::config::Version::V1,
            project: crate::config::ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: crate::config::MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        let pid_file = PidFile::default();
        let mut service_state = ServiceStateFile::default();
        let cron_state_json = json!({
            "jobs": [{
                "hash": "deadbeefdeadbeef",
                "state": {
                    "service_name": "nightly",
                    "last_execution": 1,
                    "execution_history": [{
                        "started_at": 1,
                        "completed_at": 2,
                        "status": "Success",
                        "exit_code": 0,
                    }],
                    "timezone_label": "UTC",
                    "timezone": "UTC"
                }
            }]
        });
        let mut cron_state: CronStateFile =
            serde_json::from_value(cron_state_json).expect("deserialize cron state");

        assert_ne!(live_hash, "deadbeefdeadbeef");

        let snapshot = build_snapshot(
            Some(&config),
            &pid_file,
            &mut service_state,
            &mut cron_state,
            None,
            None,
            StatusSnapshotMode::Summary,
        );

        let unit = snapshot
            .units
            .iter()
            .find(|unit| unit.name == "nightly")
            .expect("nightly unit");
        let cron = unit
            .cron
            .as_ref()
            .expect("cron history recovered by service name");
        assert_eq!(cron.recent_runs.len(), 1);
        assert_eq!(cron.timezone_label, "UTC");
    }

    #[test]
    fn build_snapshot_completed_oneshot_with_stale_pid_renders_done() {
        let mut services = std::collections::HashMap::new();
        let service = crate::config::ServiceConfig {
            command: "/bin/echo hi".into(),
            restart_policy: Some("never".into()),
            ..crate::config::ServiceConfig::default()
        };
        let hash = service.compute_hash();
        services.insert("migrate".into(), service);
        let config = Config {
            version: crate::config::Version::V1,
            project: crate::config::ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: crate::config::MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        let mut pid_file = PidFile::default();
        pid_file.insert_in_memory("migrate", 2_000_000_000);

        let mut service_state = ServiceStateFile::default();
        service_state.set_in_memory(
            &hash,
            ServiceLifecycleStatus::ExitedSuccessfully,
            Some(2_000_000_000),
            Some(0),
            None,
        );
        let mut cron_state = CronStateFile::default();

        let snapshot = build_snapshot(
            Some(&config),
            &pid_file,
            &mut service_state,
            &mut cron_state,
            None,
            None,
            StatusSnapshotMode::Summary,
        );

        let unit = snapshot
            .units
            .iter()
            .find(|unit| unit.name == "migrate")
            .expect("migrate unit");

        assert!(
            unit.process.is_none(),
            "stale missing PID should not attach to a completed one-shot unit"
        );
        assert_eq!(unit.state, UnitState::Done);
        assert_eq!(unit.health, UnitHealth::Healthy);
    }

    #[test]
    fn build_snapshot_serve_service_with_stale_pid_renders_lost() {
        let mut services = std::collections::HashMap::new();
        let service = crate::config::ServiceConfig {
            command: "/bin/sleep 30".into(),
            restart_policy: Some("always".into()),
            ..crate::config::ServiceConfig::default()
        };
        let hash = service.compute_hash();
        services.insert("api".into(), service);
        let config = Config {
            version: crate::config::Version::V1,
            project: crate::config::ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: crate::config::MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        let mut pid_file = PidFile::default();
        pid_file.insert_in_memory("api", 2_000_000_000);

        let mut service_state = ServiceStateFile::default();
        service_state.set_in_memory(
            &hash,
            ServiceLifecycleStatus::Running,
            Some(2_000_000_000),
            None,
            None,
        );
        let mut cron_state = CronStateFile::default();

        let snapshot = build_snapshot(
            Some(&config),
            &pid_file,
            &mut service_state,
            &mut cron_state,
            None,
            None,
            StatusSnapshotMode::Summary,
        );

        let unit = snapshot
            .units
            .iter()
            .find(|unit| unit.name == "api")
            .expect("api unit");

        assert!(
            unit.process.is_some(),
            "a serving service with a missing PID should keep its runtime evidence"
        );
        assert_eq!(unit.state, UnitState::Lost);
        assert_eq!(unit.health, UnitHealth::Warn);
    }

    #[test]
    fn missing_pid_is_expected_only_for_finished_cron_and_oneshot() {
        let success = CronExecutionSummary {
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            status: Some(CronExecutionStatus::Success),
            exit_code: Some(0),
            pid: Some(123),
            user: None,
            command: None,
            metrics: vec![],
        };
        let completed_cron = CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: Some(success.clone()),
            recent_runs: vec![success],
        };
        let queued_cron = CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: None,
            recent_runs: vec![],
        };

        assert!(missing_pid_is_expected(
            UnitKind::Cron,
            UnitIntent::Cron,
            None,
            Some(&completed_cron),
        ));
        assert!(!missing_pid_is_expected(
            UnitKind::Cron,
            UnitIntent::Cron,
            None,
            Some(&queued_cron),
        ));
        assert!(missing_pid_is_expected(
            UnitKind::Service,
            UnitIntent::Once,
            Some(ServiceLifecycleStatus::ExitedSuccessfully),
            None,
        ));
        assert!(!missing_pid_is_expected(
            UnitKind::Service,
            UnitIntent::Once,
            Some(ServiceLifecycleStatus::Running),
            None,
        ));
        assert!(!missing_pid_is_expected(
            UnitKind::Service,
            UnitIntent::Serve,
            Some(ServiceLifecycleStatus::ExitedSuccessfully),
            None,
        ));
    }

    #[test]
    fn build_snapshot_summary_omits_expensive_process_details() {
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

        let mut services = std::collections::HashMap::new();
        let service = crate::config::ServiceConfig {
            command: "/bin/sleep 30".into(),
            ..crate::config::ServiceConfig::default()
        };
        let hash = service.compute_hash();
        services.insert("demo".into(), service);
        let config = Config {
            version: crate::config::Version::V1,
            project: crate::config::ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: crate::config::MetricsConfig::default(),
            logs: crate::config::LogsConfig::default(),
            status: crate::config::StatusConfig::default(),
        };

        let mut pid_file = PidFile::default();
        pid_file.insert_in_memory("demo", 42);
        pid_file.record_spawn_in_memory(PersistedSpawnChild {
            pid: 43,
            name: "child".into(),
            command: "sleep 30".into(),
            started_at: SystemTime::now(),
            ttl_secs: None,
            depth: 1,
            parent_pid: 42,
            service_hash: Some(hash.clone()),
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
        });

        let mut service_state = ServiceStateFile::default();
        service_state.set_in_memory(
            &hash,
            ServiceLifecycleStatus::Running,
            Some(42),
            None,
            None,
        );
        let mut cron_state = CronStateFile::default();

        let snapshot = build_snapshot(
            Some(&config),
            &pid_file,
            &mut service_state,
            &mut cron_state,
            None,
            None,
            StatusSnapshotMode::Summary,
        );
        let unit = snapshot
            .units
            .iter()
            .find(|unit| unit.name == "demo")
            .expect("demo unit");

        assert!(unit.process.is_some());
        assert!(unit.process.as_ref().unwrap().user.is_none());
        assert!(unit.uptime.is_none());
        assert!(unit.runtime_command.is_none());
        assert!(unit.spawned_children.is_empty());

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
    fn compute_overall_health_reflects_worst_unit() {
        let units = vec![
            UnitStatus {
                name: "svc-a".into(),
                hash: "hash-a".into(),
                project: None,
                kind: UnitKind::Service,
                lifecycle: None,
                state: UnitState::Unknown,
                intent: UnitIntent::Manual,
                health: UnitHealth::Healthy,
                process: None,
                uptime: None,
                last_exit: None,
                cron: None,
                metrics: None,
                command: None,
                runtime_command: None,
                spawned_children: Vec::new(),
            },
            UnitStatus {
                name: "svc-b".into(),
                hash: "hash-b".into(),
                project: None,
                kind: UnitKind::Service,
                lifecycle: None,
                state: UnitState::Unknown,
                intent: UnitIntent::Manual,
                health: UnitHealth::Failing,
                process: None,
                uptime: None,
                last_exit: None,
                cron: None,
                metrics: None,
                command: None,
                runtime_command: None,
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
            pid: None,
            user: None,
            command: None,
            metrics: vec![],
        };

        let cron_status = CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: Some(summary.clone()),
            recent_runs: vec![summary],
        };

        let health = derive_unit_health(
            UnitKind::Cron,
            UnitState::Done,
            UnitIntent::Cron,
            None,
            None,
            Some(&cron_status),
        );
        assert_eq!(health, UnitHealth::Healthy);
    }

    #[test]
    fn derive_unit_health_for_latest_failed_cron_is_failing() {
        let now = Utc::now();
        let failed = CronExecutionSummary {
            started_at: now,
            completed_at: Some(now),
            status: Some(CronExecutionStatus::Failed("exit status 1".into())),
            exit_code: Some(1),
            pid: None,
            user: None,
            command: None,
            metrics: vec![],
        };

        let cron_status = CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: Some(failed.clone()),
            recent_runs: vec![failed],
        };

        let health = derive_unit_health(
            UnitKind::Cron,
            UnitState::Failed,
            UnitIntent::Cron,
            None,
            None,
            Some(&cron_status),
        );
        assert_eq!(health, UnitHealth::Failing);
    }

    #[test]
    fn derive_unit_health_for_queued_cron_is_idle() {
        let cron_status = CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: None,
            recent_runs: vec![],
        };

        let health = derive_unit_health(
            UnitKind::Cron,
            UnitState::Queued,
            UnitIntent::Cron,
            None,
            None,
            Some(&cron_status),
        );
        assert_eq!(health, UnitHealth::Idle);
    }

    #[test]
    fn derive_unit_health_for_failed_service_is_failing() {
        let health = derive_unit_health(
            UnitKind::Service,
            UnitState::Failed,
            UnitIntent::Serve,
            Some(ServiceLifecycleStatus::ExitedWithError),
            None,
            None,
        );
        assert_eq!(health, UnitHealth::Failing);
    }

    #[test]
    fn derive_unit_health_for_stopped_serve_service_is_warn() {
        let health = derive_unit_health(
            UnitKind::Service,
            UnitState::Stopped,
            UnitIntent::Serve,
            Some(ServiceLifecycleStatus::Stopped),
            None,
            None,
        );

        assert_eq!(health, UnitHealth::Warn);
    }

    #[test]
    fn derive_unit_health_for_completed_oneshot_is_healthy() {
        let health = derive_unit_health(
            UnitKind::Service,
            UnitState::Done,
            UnitIntent::Once,
            Some(ServiceLifecycleStatus::ExitedSuccessfully),
            None,
            None,
        );

        assert_eq!(health, UnitHealth::Healthy);
    }

    #[test]
    fn derive_unit_health_for_completed_serve_service_is_healthy() {
        let health = derive_unit_health(
            UnitKind::Service,
            UnitState::Done,
            UnitIntent::Serve,
            Some(ServiceLifecycleStatus::ExitedSuccessfully),
            None,
            None,
        );

        assert_eq!(health, UnitHealth::Healthy);
    }

    #[test]
    fn derive_unit_health_for_missing_cron_pid_defers_to_success() {
        let summary = CronExecutionSummary {
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            status: Some(CronExecutionStatus::Success),
            exit_code: Some(0),
            pid: Some(17165),
            user: None,
            command: None,
            metrics: vec![],
        };
        let cron_status = CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: Some(summary.clone()),
            recent_runs: vec![summary],
        };
        let runtime = ProcessRuntime {
            pid: 17165,
            state: ProcessState::Missing,
            user: None,
        };

        let health = derive_unit_health(
            UnitKind::Cron,
            UnitState::Done,
            UnitIntent::Cron,
            None,
            Some(&runtime),
            Some(&cron_status),
        );
        assert_eq!(health, UnitHealth::Healthy);
    }

    #[test]
    fn derive_unit_health_for_missing_oneshot_pid_defers_to_exit() {
        let runtime = ProcessRuntime {
            pid: 17165,
            state: ProcessState::Missing,
            user: None,
        };

        let health = derive_unit_health(
            UnitKind::Service,
            UnitState::Done,
            UnitIntent::Once,
            Some(ServiceLifecycleStatus::ExitedSuccessfully),
            Some(&runtime),
            None,
        );
        assert_eq!(health, UnitHealth::Healthy);
    }

    #[test]
    fn derive_unit_health_for_missing_serve_pid_is_warn() {
        let runtime = ProcessRuntime {
            pid: 17165,
            state: ProcessState::Missing,
            user: None,
        };

        let health = derive_unit_health(
            UnitKind::Service,
            UnitState::Lost,
            UnitIntent::Serve,
            Some(ServiceLifecycleStatus::Running),
            Some(&runtime),
            None,
        );
        assert_eq!(health, UnitHealth::Warn);
    }

    fn unit_for_health(name: &str) -> UnitStatus {
        UnitStatus {
            name: name.into(),
            hash: "hash".into(),
            project: None,
            kind: UnitKind::Service,
            lifecycle: None,
            state: UnitState::Unknown,
            intent: UnitIntent::Manual,
            health: UnitHealth::Healthy,
            process: None,
            uptime: None,
            last_exit: None,
            cron: None,
            metrics: None,
            command: None,
            runtime_command: None,
            spawned_children: Vec::new(),
        }
    }

    #[test]
    fn explain_unit_health_matches_running_verdict() {
        let mut unit = unit_for_health("api");
        unit.process = Some(ProcessRuntime {
            pid: 1234,
            state: ProcessState::Running,
            user: None,
        });

        let report = explain_unit_health(&unit);
        assert_eq!(report.health, UnitHealth::Healthy);
        assert_eq!(report.severity, 0);
        assert!(report.title.contains("api"));
    }

    #[test]
    fn explain_unit_health_for_stopped_serve_explains_warn() {
        let mut unit = unit_for_health("api");
        unit.intent = UnitIntent::Serve;
        unit.lifecycle = Some(ServiceLifecycleStatus::Stopped);
        unit.state = UnitState::Stopped;

        let report = explain_unit_health(&unit);
        assert_eq!(report.health, UnitHealth::Warn);
        assert!(report.severity >= 4 && report.severity <= 6);
        assert!(report.recommended_fix.contains("sysg restart -s api"));
    }

    #[test]
    fn explain_unit_health_for_error_exit_includes_exit_detail() {
        let mut unit = unit_for_health("worker");
        unit.intent = UnitIntent::Serve;
        unit.lifecycle = Some(ServiceLifecycleStatus::ExitedWithError);
        unit.last_exit = Some(ExitMetadata {
            exit_code: Some(2),
            signal: None,
        });

        let report = explain_unit_health(&unit);
        assert_eq!(report.health, UnitHealth::Failing);
        assert!(report.severity >= 7);
        assert!(report.description.contains("exited with code 2"));
    }

    #[test]
    fn explain_unit_health_for_successful_serve_exit_is_healthy() {
        let mut unit = unit_for_health("worker");
        unit.intent = UnitIntent::Serve;
        unit.lifecycle = Some(ServiceLifecycleStatus::ExitedSuccessfully);
        unit.state = UnitState::Done;
        unit.last_exit = Some(ExitMetadata {
            exit_code: Some(0),
            signal: None,
        });

        let report = explain_unit_health(&unit);
        assert_eq!(report.health, UnitHealth::Healthy);
        assert_eq!(report.severity, 0);
        assert!(report.tldr.contains("status 0"));
    }

    #[test]
    fn explain_unit_health_for_failed_cron_surfaces_reason() {
        let failed = CronExecutionSummary {
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            status: Some(CronExecutionStatus::Failed("boom".into())),
            exit_code: Some(1),
            pid: None,
            user: None,
            command: None,
            metrics: vec![],
        };
        let mut unit = unit_for_health("nightly");
        unit.kind = UnitKind::Cron;
        unit.cron = Some(CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: Some(failed.clone()),
            recent_runs: vec![failed],
        });

        let report = explain_unit_health(&unit);
        assert_eq!(report.health, UnitHealth::Failing);
        assert!(report.description.contains("boom"));
    }

    #[test]
    fn explain_unit_health_for_missing_cron_pid_reports_success() {
        let summary = CronExecutionSummary {
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            status: Some(CronExecutionStatus::Success),
            exit_code: Some(0),
            pid: Some(17165),
            user: None,
            command: None,
            metrics: vec![],
        };
        let mut unit = unit_for_health("curate_tiktok");
        unit.kind = UnitKind::Cron;
        unit.intent = UnitIntent::Cron;
        unit.process = Some(ProcessRuntime {
            pid: 17165,
            state: ProcessState::Missing,
            user: None,
        });
        unit.cron = Some(CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: Some(summary.clone()),
            recent_runs: vec![summary],
        });

        let report = explain_unit_health(&unit);
        assert_eq!(report.health, UnitHealth::Healthy);
        assert_eq!(report.severity, 0);
    }

    #[test]
    fn explain_unit_health_for_missing_serve_pid_explains_warn() {
        let mut unit = unit_for_health("api");
        unit.intent = UnitIntent::Serve;
        unit.process = Some(ProcessRuntime {
            pid: 17165,
            state: ProcessState::Missing,
            user: None,
        });

        let report = explain_unit_health(&unit);
        assert_eq!(report.health, UnitHealth::Warn);
        assert_eq!(report.severity, 5);
        assert!(report.title.contains("tracked PID but no process"));
    }

    #[test]
    fn explain_unit_health_agrees_with_derive_unit_health() {
        let mut unit = unit_for_health("svc");
        unit.process = Some(ProcessRuntime {
            pid: 9,
            state: ProcessState::Zombie,
            user: None,
        });

        let derived = derive_unit_health(
            unit.kind,
            unit.state,
            unit.intent,
            unit.lifecycle,
            unit.process.as_ref(),
            unit.cron.as_ref(),
        );
        assert_eq!(explain_unit_health(&unit).health, derived);
    }

    #[test]
    fn explain_unit_health_agrees_for_missing_cron_pid() {
        let summary = CronExecutionSummary {
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            status: Some(CronExecutionStatus::Success),
            exit_code: Some(0),
            pid: Some(17165),
            user: None,
            command: None,
            metrics: vec![],
        };
        let mut unit = unit_for_health("curate_tiktok");
        unit.kind = UnitKind::Cron;
        unit.intent = UnitIntent::Cron;
        unit.process = Some(ProcessRuntime {
            pid: 17165,
            state: ProcessState::Missing,
            user: None,
        });
        unit.cron = Some(CronUnitStatus {
            timezone_label: "UTC".into(),
            timezone: Some("UTC".into()),
            last_run: Some(summary.clone()),
            recent_runs: vec![summary],
        });

        let derived = derive_unit_health(
            unit.kind,
            unit.state,
            unit.intent,
            unit.lifecycle,
            unit.process.as_ref(),
            unit.cron.as_ref(),
        );
        assert_eq!(explain_unit_health(&unit).health, derived);
    }
}
