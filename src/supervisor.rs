#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    ffi::CString,
    fs::File,
    io,
    io::Write,
    os::fd::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use nix::unistd::{Uid, User};
use sysinfo::{ProcessesToUpdate, System};
use thiserror::Error;
use tracing::{debug, error, info, warn};

use crate::{
    config::{
        Config, LogSink, SkipConfig, SpawnMode, StatusSnapshotMode, TerminationPolicy,
        load_projects_from_file, supervisor::SupervisorTimeouts,
    },
    cron::{CronExecutionStatus, CronManager},
    daemon::{
        Daemon, PersistedSpawnChild, ServiceLifecycleStatus, ServiceReadyState,
        ServiceStateFile,
    },
    error::{LogsManagerError, ProcessManagerError},
    ipc::{self, ControlCommand, ControlResponse, InspectPayload},
    logs::{
        LogManager, LogSection, get_service_log_path, resolve_log_path,
        spawn_dynamic_child_log_writer, write_log_section_header,
    },
    metrics::{self, MetricSample, MetricsCollector, MetricsHandle},
    opslot::{OpParts, OpSlot},
    runtime,
    spawn::{DynamicSpawnManager, SpawnedChild, SpawnedChildKind, SpawnedExit},
    start::{self, BootFrame, BootJournal},
    status::{
        BootStatus, ProjectRunMode, StatusCache, StatusError, StatusRefresher,
        StatusSnapshot, collect_runtime_snapshot,
        collect_runtime_snapshot_with_cron_hashes, compute_overall_health,
        cron_hashes_for_config,
    },
    upgrade::{
        HANDOFF_SCHEMA_VERSION, HandoffProject, LIVE_REEXEC_PROTOCOL, LiveUpgradeInfo,
        SupervisorHandoff, UpgradeTarget,
    },
};

/// Interval between cron scheduler scans.
const CRON_TICK_INTERVAL: Duration = Duration::from_secs(1);
/// Reason recorded when a cron run's exit status was consumed elsewhere and
/// never routed back to the run that owned it. The outcome is unknown, and an
/// unknown outcome is never a success.
const CRON_STATUS_LOST_REASON: &str =
    "the run's exit status was consumed by another reaper before it could be read";
/// Delay before retrying a failed control-socket accept.
const CONTROL_ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
/// Maximum time allowed for a live-upgrade acceptance response to reach its client.
const UPGRADE_ACCEPT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a progress subscriber waits for its operation to register.
///
/// Clients subscribe before sending the mutation so they cannot miss its
/// opening frames, which means the journal legitimately does not exist for the
/// moment it takes the request to reach the owner thread.
const OP_STREAM_REGISTER_TIMEOUT: Duration = Duration::from_secs(3);
/// Gap between checks while waiting for an operation to register.
const OP_STREAM_REGISTER_POLL: Duration = Duration::from_millis(10);
/// Attempts to publish the post-boot snapshot before announcing the boot done.
const BOOT_SNAPSHOT_ATTEMPTS: usize = 3;
/// Delay between post-boot snapshot publication attempts.
const BOOT_SNAPSHOT_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Supervisor errors.
#[derive(Debug, Error)]
pub enum SupervisorError {
    /// Process management error.
    #[error(transparent)]
    Process(#[from] ProcessManagerError),
    /// IPC control channel error.
    #[error(transparent)]
    Control(#[from] ipc::ControlError),
    /// I/O error.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Metrics subsystem error.
    #[error(transparent)]
    Metrics(#[from] metrics::MetricsError),
    /// Status snapshot error.
    #[error(transparent)]
    Status(#[from] StatusError),
    /// Log streaming error.
    #[error(transparent)]
    Logs(#[from] LogsManagerError),
}

/// Converts a supervisor error into the response the client renders: structured
/// diagnostics pass through intact, everything else degrades to a string.
fn error_response(err: &SupervisorError) -> ControlResponse {
    match err {
        SupervisorError::Process(ProcessManagerError::Diag(diag)) => {
            ControlResponse::Diag(diag.clone())
        }
        SupervisorError::Process(ProcessManagerError::ServicesNotRunning {
            services,
        }) => {
            let diag = crate::diag::Diagnostic::error(
                crate::diag::SgCode::ProjectServicesNotUp,
                "one or more services did not reach their target state",
            )
            .note(format!("did not start: {}", services.join(", ")))
            .help_cmd("check status", "sysg status")
            .help_docs();
            ControlResponse::Diag(Box::new(diag))
        }
        other => ControlResponse::Error(other.to_string()),
    }
}

/// Daemon supervisor that handles CLI commands.
pub struct Supervisor {
    /// Canonical path associated with the primary project manifest.
    config_path: PathBuf,
    /// Primary project's process daemon.
    daemon: Daemon,
    /// Operator-controlled lifecycle timeout policy.
    timeouts: SupervisorTimeouts,
    /// Whether newly spawned services use legacy detached behavior.
    detach_children: bool,
    /// Scheduler shared by all registered projects.
    cron_manager: CronManager,
    /// Optional single-service filter applied to the primary boot.
    service_filter: Option<String>,
    /// Latest aggregate status snapshot.
    status_cache: StatusCache,
    /// Periodic status snapshot worker.
    status_refresher: Option<StatusRefresher>,
    /// Shared metrics history.
    metrics_store: MetricsHandle,
    /// Periodic metrics collection worker.
    metrics_collector: Option<MetricsCollector>,
    /// Dynamic child-process ownership and limits.
    spawn_manager: DynamicSpawnManager,
    /// Whether service stderr is forwarded to supervisor stdout.
    pipe_stderr: bool,
    /// Attachment mode of the primary project.
    primary_project_mode: ProjectRunMode,
    /// Whether the primary project remains registered.
    primary_active: bool,
    /// Additional registered project runtimes.
    extra_projects: BTreeMap<String, ProjectRuntime>,
    /// Scheduler routing snapshot for all cron-capable projects.
    cron_projects: Arc<RwLock<Vec<CronProjectRuntime>>>,
    /// Single mutable operation currently reported to clients.
    op_slot: OpSlot,
    /// Additional projects declared in the primary config file (a multi-project
    /// manifest), registered as extra projects once the primary has booted.
    pending_projects: Vec<Config>,
    /// Race-free record of the initial boot, streamed to a `BootStream` client.
    boot_journal: BootJournal,
    /// Journals for mutations currently in flight, keyed by operation id.
    ///
    /// The boot journal seals on its terminal frame and never reopens, so every
    /// later restart/stop that a client watches gets its own. Entries are
    /// dropped once their operation ends and the client has drained them.
    op_journals: Arc<RwLock<HashMap<String, BootJournal>>>,
    /// Journal of the command being handled right now, when one is watched.
    ///
    /// Lets a handler attach daemons it CREATES — `AddProject` builds them
    /// inside the command — to the journal the client already subscribed to.
    active_op: Option<(String, BootJournal)>,
    /// Lease on that journal, cloned by handlers that spawn work outliving the
    /// command so the stream stays open until the last of it finishes.
    op_lease: Option<Arc<OpWatch>>,
    /// Daemons visible to cancellation-aware boot requests.
    boot_projects: Arc<RwLock<HashMap<String, Daemon>>>,
    /// Latest queued project boot state exposed through status snapshots.
    boots: Arc<RwLock<HashMap<String, BootStatus>>>,
    /// Whether the control plane is quiescing for live re-execution.
    upgrading: Arc<AtomicBool>,
    /// Serializes cron due-state mutation with upgrade preflight.
    cron_gate: Arc<std::sync::Mutex<()>>,
    /// Inherited runtime state awaiting activation in a replacement image.
    handoff: Option<LoadedHandoff>,
}

/// Handoff record loaded by the replacement binary before its event loop starts.
struct LoadedHandoff {
    /// Private serialized handoff path removed after successful resume.
    path: PathBuf,
    /// Validated runtime state and inherited descriptor numbers.
    state: SupervisorHandoff,
}

/// A handoff project resolved against its manifest as parsed by THIS binary.
struct LoadedHandoffProject {
    /// The project as the current loader understands it.
    config: Config,
    /// The id the handoff recorded, when it differs from the loaded one — a
    /// pre-0.59 resident naming its loose project `__loose__`. `None` for an
    /// exact match.
    legacy_id: Option<String>,
}

/// Runtime state for an additional project managed by the resident supervisor.
struct ProjectRuntime {
    daemon: Daemon,
    mode: ProjectRunMode,
    config_path: PathBuf,
}

/// Runtime state used by the cron scheduler to route jobs to their project.
#[derive(Clone)]
struct CronProjectRuntime {
    project_id: String,
    daemon: Daemon,
    config: Arc<Config>,
    mode: ProjectRunMode,
    config_path: PathBuf,
}

/// How far up the process ancestry a spawn request is traced before giving up
/// on linking it to a managed unit.
const MAX_SPAWN_PARENT_WALK: usize = 32;

/// Parameters for spawning a child process.
struct SpawnParams {
    parent_pid: u32,
    name: String,
    command: Vec<String>,
    ttl: Option<u64>,
    log_level: Option<String>,
}

/// Parameters for streaming logs through the supervisor control socket.
struct SupervisorLogRequest<'a> {
    /// Latest status snapshot used to resolve service/project targets.
    snapshot: crate::status::StatusSnapshot,
    /// Optional service name, hash, or `project/service` selector.
    service: Option<String>,
    /// Optional stable project id used to filter log targets.
    project: Option<String>,
    /// Number of trailing log lines to include.
    lines: usize,
    /// Optional stream kind (`stdout`, `stderr`, or combined when absent).
    kind: Option<&'a str>,
    /// Whether to follow the log stream until the client disconnects.
    follow: bool,
    /// Post-capture time/pattern/history filter applied before display.
    filter: crate::logs::LogFilter,
    /// Whether the client renders structured output and consumes per-service
    /// marker lines, so multi-service views can be attributed to their unit.
    structured: bool,
    /// Supervisor-owned Unix stream connected to the CLI client.
    stream: &'a std::os::unix::net::UnixStream,
}

#[derive(Debug, Clone)]
/// Represents cron completion outcome.
struct CronCompletionOutcome {
    status: CronExecutionStatus,
    exit_code: Option<i32>,
}

/// Units left down by one project boot and the first concrete cause observed.
#[derive(Debug, Default)]
struct BootFailures {
    /// Sorted names of units that did not reach their declared boot target.
    services: Vec<String>,
    /// First concrete unit diagnostic, retained as the project's root cause.
    cause: Option<crate::diag::Diagnostic>,
}

impl BootFailures {
    /// Creates a stable, sorted failure report from the boot accumulator.
    fn new(mut services: Vec<String>, cause: Option<crate::diag::Diagnostic>) -> Self {
        services.sort_unstable();
        Self { services, cause }
    }

    /// Whether every requested unit reached its target boot state.
    fn is_empty(&self) -> bool {
        self.services.is_empty()
    }

    /// Unit names suitable for reconcile diagnostics and logs.
    fn services(&self) -> &[String] {
        &self.services
    }

    /// Converts the project result without discarding the originating diagnosis.
    fn into_error(self, project: &str) -> ProcessManagerError {
        let Self { services, cause } = self;
        let mut diag = cause
            .unwrap_or_else(|| crate::start::project_services_not_up(project, &services));
        if !services.is_empty() {
            diag = diag.note(format!(
                "project `{project}` also left these units down: {}",
                services.join(", ")
            ));
        }
        ProcessManagerError::Diag(Box::new(diag))
    }
}

/// Keeps a watched operation's journal reachable for the life of the operation.
///
/// Dropping it seals the journal with a terminal frame — so a client waiting on
/// the stream is released even when the operation failed early — and removes the
/// entry so the registry cannot grow without bound.
///
/// Held behind an `Arc` so work that outlives the command can take a lease: an
/// `AddProject` may queue several project boots onto their own threads, and the
/// stream must stay open until the LAST of them finishes. Sealing when the
/// command returned would end the tree at queue time, before a single unit ran.
struct OpWatch {
    op: String,
    journals: Arc<RwLock<HashMap<String, BootJournal>>>,
    _daemons: Vec<crate::daemon::WatchGuard>,
}

impl Drop for OpWatch {
    fn drop(&mut self) {
        remove_and_seal_journal(&self.journals, &self.op);
    }
}

/// Removes an operation's journal and seals it with a terminal frame.
///
/// Sealing is not optional: a subscriber holds its own clone of the journal,
/// so removal alone leaves it waiting forever on frames that can no longer
/// arrive. Every path that retires a journal — completion, a failed enqueue,
/// a supervisor that died before replying — must go through this.
fn remove_and_seal_journal(journals: &RwLock<HashMap<String, BootJournal>>, op: &str) {
    let journal = journals
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(op);
    if let Some(journal) = journal {
        let (started, failed) = journal.tally();
        journal.push(BootFrame::Done { started, failed });
    }
}

/// Cheap-to-clone handles the acceptor uses to answer read commands without
/// touching the supervisor's mutation state.
#[derive(Clone)]
struct ReadContext {
    status_cache: StatusCache,
    op_slot: OpSlot,
    version: String,
    boot_journal: BootJournal,
    op_journals: Arc<RwLock<HashMap<String, BootJournal>>>,
    boot_projects: Arc<RwLock<HashMap<String, Daemon>>>,
    boots: Arc<RwLock<HashMap<String, BootStatus>>>,
    /// Whether mutations are refused while a live upgrade is committing.
    upgrading: Arc<AtomicBool>,
}

/// A mutation command routed from the acceptor to the single-writer owner thread,
/// paired with a channel to return the owner's response to the waiting connection.
struct MutationRequest {
    /// Mutation routed to the supervisor owner thread.
    command: ControlCommand,
    /// Response returned to the connection worker.
    reply: mpsc::Sender<ControlResponse>,
    /// Acknowledges that the response reached the client socket.
    delivered: mpsc::Receiver<bool>,
}

/// Validated handoff ready to execute after the client receives acceptance.
struct PreparedUpgrade {
    /// Replacement executable.
    target: UpgradeTarget,
    /// Private serialized handoff record.
    path: PathBuf,
    /// Primary manifest path required by the internal supervisor command.
    config: PathBuf,
}

/// Handles fallback cron user.
fn fallback_cron_user(service_config: &crate::config::ServiceConfig) -> Option<String> {
    if let Some(user) = service_config.user.as_ref().filter(|user| !user.is_empty()) {
        return Some(user.clone());
    }

    User::from_uid(Uid::current())
        .ok()
        .flatten()
        .map(|user| user.name)
}

/// Returns metric samples collected during one cron execution.
fn cron_run_metrics(
    metrics_store: &MetricsHandle,
    service_hash: &str,
    started_at: SystemTime,
) -> Vec<MetricSample> {
    let started_at: chrono::DateTime<chrono::Utc> = started_at.into();
    metrics_store
        .try_read()
        .ok()
        .and_then(|store| store.snapshot_unit(service_hash))
        .unwrap_or_default()
        .into_iter()
        .filter(|sample| sample.timestamp >= started_at)
        .collect()
}

/// Persists the final lifecycle state for one cron execution.
fn persist_cron_state(
    daemon: &Daemon,
    service_hash: &str,
    service_name: &str,
    status: ServiceLifecycleStatus,
    exit_code: Option<i32>,
) {
    if let Ok(mut state_file) = ServiceStateFile::load(daemon.store())
        && let Err(err) = state_file.set(service_hash, status, None, exit_code, None)
    {
        warn!("Failed to persist cron job '{service_name}' exit state: {err}");
    }
}

/// Runs a unit's `onerr` hook for a cron run that did not succeed.
///
/// A cron run is judged in exactly one place — the completion path — so it is
/// announced from exactly one place too. The monitor skips its own hook for
/// these units; firing from both would alert twice for a single failure.
fn notify_cron_failure(
    daemon: &Daemon,
    service_name: &str,
    service_config: &crate::config::ServiceConfig,
    status: &CronExecutionStatus,
) {
    match status {
        CronExecutionStatus::Failed(_) | CronExecutionStatus::OverlapError => {
            daemon.run_onerr(service_name, service_config);
        }
        // An interrupted run reports a lost outcome, not a failed one: the
        // command may well have succeeded, and alerting on it would cry wolf.
        CronExecutionStatus::Success | CronExecutionStatus::Interrupted(_) => {}
    }
}

/// Clears the PID owned by a completed cron execution without removing a newer run.
fn clear_cron_pid(daemon: &Daemon, service_name: &str, expected_pid: u32) {
    let pid_file = daemon.pid_file_handle();
    let mut pid_file = pid_file
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(err) = pid_file.clear_pid_if_matches(service_name, expected_pid) {
        debug!("Failed to clear cron job '{service_name}' PID: {err}");
    }
}

/// Rejects direct control of a cron unit with a schedule-aware diagnostic.
fn reject_direct_cron_control(
    service_config: &crate::config::ServiceConfig,
    service_name: &str,
    target_project: &str,
    verb: &str,
) -> Result<(), SupervisorError> {
    if service_config.cron.is_some() {
        let diag = crate::diag::Diagnostic::error(
            crate::diag::SgCode::CronDirectControl,
            format!("cron unit `{service_name}` cannot be {verb} directly"),
        )
        .note("cron units run only when their schedule fires")
        .note("restarting the project reloads the schedule but does not run the job immediately")
        .help_cmd(
            "inspect the failed run",
            format!("sysg logs -p {target_project} -s {service_name}"),
        )
        .help_cmd(
            "reload the schedule",
            format!("sysg restart -p {target_project}"),
        )
        .help_docs();
        return Err(ProcessManagerError::Diag(Box::new(diag)).into());
    }
    Ok(())
}

/// Splits a qualified selector of the form `project_id/service_name`.
fn split_project_selector(selector: &str) -> Option<(&str, &str)> {
    let (project, service) = selector.split_once('/')?;
    if project.is_empty() || service.is_empty() {
        None
    } else {
        Some((project, service))
    }
}

/// Orders `root` and its transitive dependents so a dependency is always
/// restarted before anything that depends on it. `restart -s A` must bounce A
/// and everything that depends on A, because a dependent needs to re-handshake
/// the freshly-restarted A; a stale dependent left pointing at the old A is the
/// leaky state this cascade exists to prevent.
fn cascade_restart_order(config: &Config, root: &str) -> Vec<String> {
    let reverse = config.reverse_dependencies();
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(root.to_string());
    seen.insert(root.to_string());
    while let Some(service) = queue.pop_front() {
        order.push(service.clone());
        if let Some(dependents) = reverse.get(&service) {
            for dependent in dependents {
                if seen.insert(dependent.clone()) {
                    queue.push_back(dependent.clone());
                }
            }
        }
    }
    order
}

/// Returns whether a status unit belongs to the requested project id.
fn project_matches(unit: &crate::status::UnitStatus, project: Option<&str>) -> bool {
    project.is_none_or(|project_id| {
        unit.project.as_ref().map(|project| project.id.as_str()) == Some(project_id)
    })
}

/// Returns whether a status unit matches a service selector and optional project id.
fn unit_matches_selector(
    unit: &crate::status::UnitStatus,
    selector: &str,
    project: Option<&str>,
) -> bool {
    let (selector_project, service_selector) = split_project_selector(selector)
        .map(|(project_id, service_name)| (Some(project_id), service_name))
        .unwrap_or((None, selector));
    let requested_project = project.or(selector_project);

    project_matches(unit, requested_project)
        && (unit.name == service_selector || unit.hash == service_selector)
}

/// Groups non-orphan status units by project for supervisor log streaming.
fn log_project_groups<'a>(
    snapshot: &'a crate::status::StatusSnapshot,
    project: Option<&str>,
) -> Vec<(String, Vec<&'a crate::status::UnitStatus>)> {
    let mut groups: Vec<(String, String, Vec<&crate::status::UnitStatus>)> = Vec::new();

    for unit in snapshot
        .units
        .iter()
        .filter(|unit| !matches!(unit.kind, crate::status::UnitKind::Orphaned))
        .filter(|unit| project_matches(unit, project))
    {
        let (key, label) = unit
            .project
            .as_ref()
            .map(|project| {
                let base = if project.name == project.id {
                    project.name.clone()
                } else {
                    format!("{} ({})", project.name, project.id)
                };
                let mode = match project.mode {
                    ProjectRunMode::Daemon => "daemon",
                    ProjectRunMode::Foreground => "foreground",
                };
                let label = format!("{base} [{mode}]");
                (project.id.clone(), label)
            })
            .unwrap_or_else(|| ("__orphans__".to_string(), "Ungrouped".to_string()));

        if let Some((_, _, units)) = groups
            .iter_mut()
            .find(|(existing_key, _, _)| existing_key == &key)
        {
            units.push(unit);
        } else {
            groups.push((key, label, vec![unit]));
        }
    }

    groups
        .into_iter()
        .map(|(_, label, units)| (label, units))
        .collect()
}

/// One project's units driven through a boot.
///
/// Every unit that runs reports itself from its own worker; the decisions that
/// must stay in dependency order — a unit gated by a dependency, and which
/// diagnostic becomes the project's cause — are made by the dispatcher or
/// resolved from the topological order once the boot is done.
struct ProjectBoot<'a> {
    /// The project daemon holding these units.
    daemon: &'a Daemon,
    /// The config the schedule was built from.
    config: &'a Config,
    /// The project these units belong to, for journal frames.
    project_id: &'a str,
    /// Registry that dynamic-spawn units publish their PID to.
    spawn_manager: &'a DynamicSpawnManager,
    /// Journal for the initial boot; `None` for a live project add.
    boot_journal: Option<&'a BootJournal>,
    /// The boot generation this schedule belongs to.
    boot_epoch: u64,
    /// Diagnostics recorded per unit, resolved into one cause afterwards.
    causes: Mutex<HashMap<String, crate::diag::Diagnostic>>,
}

impl ProjectBoot<'_> {
    /// Records why a unit failed, keeping the first reason given for it.
    fn note(&self, service: &str, diagnostic: crate::diag::Diagnostic) {
        if let Ok(mut causes) = self.causes.lock() {
            causes.entry(service.to_string()).or_insert(diagnostic);
        }
    }

    /// Returns the diagnostic belonging to the earliest unit in dependency
    /// order.
    fn first_cause(self, order: &[String]) -> Option<crate::diag::Diagnostic> {
        let mut causes = self.causes.into_inner().ok()?;
        order.iter().find_map(|service| causes.remove(service))
    }

    /// Announces that a unit is being worked, to the boot journal and to any
    /// client watching the daemon directly.
    fn announce(&self, service: &str) {
        if let Some(journal) = self.boot_journal {
            journal.push(BootFrame::UnitStarting {
                project: self.project_id.to_string(),
                service: service.to_string(),
            });
        }
    }

    /// Records a unit's terminal outcome in the boot journal.
    fn record(&self, service: &str, outcome: start::Outcome) {
        if let Some(journal) = self.boot_journal {
            journal.record(self.project_id, service, outcome);
        }
    }

    /// Reports a unit that never ran, as both a journal frame pair and the
    /// project's candidate cause.
    fn report_failure(&self, service: &str, diagnostic: crate::diag::Diagnostic) {
        self.note(service, diagnostic.clone());
        self.announce(service);
        self.record(service, start::Outcome::Failed(diagnostic));
    }

    /// Applies the dependency conditions the scheduler does not decide.
    fn dependencies_met(
        &self,
        service_name: &str,
        service: &crate::config::ServiceConfig,
        deps: &HashMap<String, start::Resolution>,
    ) -> bool {
        let Some(declared) = &service.depends_on else {
            return true;
        };

        for dependency in declared {
            let dependency_name = dependency.service();
            let Some(state) = deps.get(dependency_name) else {
                continue;
            };

            // Whether a dependency has finished is read from what it actually
            // recorded, not only from the state this schedule saw it resolve
            // in. A finite unit that was alive when it resolved may have
            // exited since, and a dependent that trusted the stale view would
            // start behind a process that is gone.
            let mut completed = *state == start::Resolution::Completed
                || matches!(
                    self.daemon.recorded_status(dependency_name),
                    Some(ServiceLifecycleStatus::ExitedSuccessfully)
                );
            if dependency.condition() == crate::config::DependsOnCondition::Completed
                && !completed
            {
                if let Err(err) = self
                    .daemon
                    .wait_for_dependency_completion(service_name, dependency_name)
                {
                    error!(
                        "Skipping service '{service_name}' because dependency '{dependency_name}' did not complete: {err}"
                    );
                    self.report_failure(
                        service_name,
                        start::dependency_unavailable(
                            service_name,
                            dependency_name,
                            err.to_string(),
                        ),
                    );
                    return false;
                }
                completed = true;
            }

            let running = *state == start::Resolution::Running && !completed;
            let finite = self
                .config
                .services
                .get(dependency_name)
                .is_some_and(|dependency| !dependency.restarts_after_failure());
            if !Daemon::dependency_satisfied(dependency, running, completed, finite) {
                error!(
                    "Skipping service '{service_name}' because dependency '{dependency_name}' did not reach its target"
                );
                self.report_failure(
                    service_name,
                    start::dependency_unavailable(
                        service_name,
                        dependency_name,
                        format!(
                            "dependency `{dependency_name}` did not reach its required state"
                        ),
                    ),
                );
                return false;
            }
        }

        true
    }
}

impl start::Units for ProjectBoot<'_> {
    type Error = SupervisorError;

    fn static_resolution(
        &self,
        service_name: &str,
    ) -> Result<Option<start::Resolution>, Self::Error> {
        let Some(service) = self.config.services.get(service_name) else {
            return Ok(Some(start::Resolution::Skipped));
        };

        // A statically skipped unit is skipped whatever its kind. This is
        // checked BEFORE the cron hand-off below: a cron unit that returned
        // early here would be recorded healthy and completed, and its scheduler
        // entry would go on claiming a boundary every expression tick for a
        // command that is never allowed to run.
        if matches!(service.skip, Some(SkipConfig::Flag(true))) {
            info!("Skipping service '{service_name}' due to skip flag");
            self.daemon.mark_service_skipped(service_name)?;
            return Ok(Some(start::Resolution::Skipped));
        }

        if service.cron.is_some() {
            return Ok(Some(start::Resolution::Completed));
        }

        Ok(None)
    }

    fn start(
        &self,
        service_name: &str,
        deps: &HashMap<String, start::Resolution>,
    ) -> Result<start::Resolution, Self::Error> {
        let Some(service) = self.config.services.get(service_name) else {
            return Ok(start::Resolution::Skipped);
        };

        if let Some(SkipConfig::Command(skip_command)) = &service.skip {
            match self
                .daemon
                .evaluate_skip_condition(service_name, skip_command)
            {
                Ok(true) => {
                    info!("Skipping service '{service_name}' due to skip condition");
                    self.daemon.mark_service_skipped(service_name)?;
                    return Ok(start::Resolution::Skipped);
                }
                Ok(false) => {}
                Err(err) => {
                    error!(
                        "Failed to evaluate skip condition for '{service_name}': {err}"
                    );
                    self.report_failure(
                        service_name,
                        start::unit_start_failed(service_name, err.to_string()),
                    );
                    return Ok(start::Resolution::Failed);
                }
            }
        }

        if !self.dependencies_met(service_name, service, deps) {
            return Ok(start::Resolution::Failed);
        }

        self.announce(service_name);
        // Also emitted through the daemon so a resident `start` — which has no
        // boot journal — still streams to whoever is watching it.
        self.daemon.note_unit_starting(service_name);
        let mut service_to_start = service.clone();
        service_to_start.skip = None;
        let result = self.daemon.start_service(service_name, &service_to_start);

        if !self.daemon.boot_active(self.boot_epoch) {
            // A unit that came up after the boot was cancelled is still ours to
            // take back down. Serialized so a wide boot does not fire N
            // concurrent teardowns at a daemon that is already being stopped.
            static LATE_STOP: Mutex<()> = Mutex::new(());
            let _serialized = LATE_STOP.lock();
            if let Err(err) = self.daemon.stop_service(service_name) {
                error!(
                    "Failed to stop '{service_name}' after project boot cancellation: {err}"
                );
            }
            return Ok(start::Resolution::Failed);
        }

        let pid = self
            .daemon
            .pid_file_handle()
            .lock()
            .ok()
            .and_then(|pid_file| pid_file.services().get(service_name).copied());
        let resolution = match &result {
            Ok(ServiceReadyState::Running) => start::Resolution::Running,
            Ok(ServiceReadyState::CompletedSuccess) => start::Resolution::Completed,
            Ok(ServiceReadyState::Skipped) => start::Resolution::Skipped,
            Err(_) => start::Resolution::Failed,
        };
        let outcome = start::outcome_of(service_name, result, pid);
        if let Some(diagnostic) = outcome.diagnostic() {
            self.note(service_name, diagnostic.clone());
            error!(
                "Service '{service_name}' failed to start [{}]: {}.",
                diagnostic.code_str(),
                diagnostic.title
            );
        }
        self.daemon.note_unit_done(service_name, outcome.clone());
        self.record(service_name, outcome);

        if resolution != start::Resolution::Failed
            && let Some(spawn) = &service.spawn
            && let Some(SpawnMode::Dynamic) = spawn.mode
            && let Ok(pid_file) = self.daemon.pid_file_handle().lock()
            && let Some(&pid) = pid_file.services().get(service_name)
        {
            self.spawn_manager
                .register_service_pid(self.project_id, service_name, pid);
        }

        Ok(resolution)
    }

    fn gated(
        &self,
        service_name: &str,
        dependency: &str,
        gate: start::Gate,
    ) -> Result<start::Resolution, Self::Error> {
        match gate {
            start::Gate::DependencySkipped => {
                info!(
                    "Skipping service '{service_name}' because dependency '{dependency}' was skipped"
                );
                self.daemon.mark_service_skipped(service_name)?;
                Ok(start::Resolution::Skipped)
            }
            start::Gate::DependencyFailed => {
                error!(
                    "Skipping service '{service_name}' because dependency '{dependency}' did not start"
                );
                self.report_failure(
                    service_name,
                    start::dependency_unavailable(
                        service_name,
                        dependency,
                        format!(
                            "dependency `{dependency}` did not reach its required state"
                        ),
                    ),
                );
                Ok(start::Resolution::Failed)
            }
        }
    }

    fn active(&self) -> bool {
        self.daemon.boot_active(self.boot_epoch)
    }
}

impl Supervisor {
    /// Returns the configured status snapshot refresh interval.
    fn status_snapshot_interval(config: &Config) -> Duration {
        config.status.snapshot_interval()
    }

    /// Returns the configured status snapshot collection mode.
    fn status_snapshot_mode(config: &Config) -> StatusSnapshotMode {
        config.status.snapshot_mode
    }

    /// Returns the snapshot mode used by an explicit live request.
    fn live_status_snapshot_mode(config: &Config) -> StatusSnapshotMode {
        Self::live_snapshot_mode(config.status.snapshot_mode)
    }

    /// Promotes a configured mode to one that actually collects.
    ///
    /// `Off` disables the periodic refresher, not status itself: reads still
    /// have to be answered, so a collection made on their behalf substitutes
    /// the cheapest mode that produces a snapshot.
    fn live_snapshot_mode(mode: StatusSnapshotMode) -> StatusSnapshotMode {
        match mode {
            StatusSnapshotMode::Off => StatusSnapshotMode::Summary,
            mode => mode,
        }
    }

    /// Resolves dependency order and applies an optional single-unit filter.
    fn startup_service_order(
        config: &Config,
        service_filter: Option<&str>,
    ) -> Result<Vec<String>, SupervisorError> {
        let mut order = config.service_start_order()?;
        if let Some(filter) = service_filter {
            order.retain(|service_name| service_name == filter);
        }
        Ok(order)
    }

    /// Brings the spawn registry in line with a project config.
    ///
    /// `mode` is the switch and `limits` only tunes the ceilings. Registering on
    /// the presence of `limits` meant `spawn: {mode: dynamic}` on its own
    /// registered nothing, so every spawn the unit made was refused for having
    /// no tree — while `limits` written under a static unit silently authorized
    /// one. Both readings were wrong, and the two registration paths disagreed
    /// about which to use.
    fn register_spawn_limits_for_config(
        spawn_manager: &DynamicSpawnManager,
        config: &Config,
    ) -> Result<(), SupervisorError> {
        let project = &config.project.id;
        for (service_name, service_config) in &config.services {
            let dynamic = service_config
                .spawn
                .as_ref()
                .filter(|spawn| matches!(spawn.mode, Some(SpawnMode::Dynamic)));
            match dynamic {
                Some(spawn_config) => {
                    let limits = spawn_config.limits.clone().unwrap_or_default();
                    spawn_manager.register_service(project, service_name, &limits)?;
                }
                None => spawn_manager.unregister_service(project, service_name),
            }
        }

        Ok(())
    }

    /// Starts services for a project daemon without blocking the supervisor control loop.
    ///
    /// `boot_journal` is `Some` only during the initial boot, so per-unit
    /// outcomes stream to a `BootStream` client; live project adds pass `None`.
    fn start_project_services(
        daemon: &Daemon,
        config: &Config,
        service_filter: Option<&str>,
        spawn_manager: &DynamicSpawnManager,
        boot_journal: Option<&BootJournal>,
    ) -> Result<BootFailures, SupervisorError> {
        let boot_epoch = daemon.begin_boot();
        let order = Self::startup_service_order(config, service_filter)?;
        // A filtered start covers one unit, so it carries no edges: selecting a
        // unit starts that unit and never silently pulls its dependencies in.
        let schedule = start::Schedule::new(&order, |service| {
            config
                .services
                .get(service)
                .and_then(|service| service.depends_on.as_ref())
                .into_iter()
                .flatten()
                .map(crate::config::DependsOn::service)
                .collect()
        });

        let boot = ProjectBoot {
            daemon,
            config,
            project_id: &config.project.id,
            spawn_manager,
            boot_journal,
            boot_epoch,
            causes: Mutex::new(HashMap::new()),
        };

        let resolved = schedule.run(&boot, daemon.start_concurrency())?;

        let failed = resolved
            .iter()
            .filter(|(_, state)| **state == start::Resolution::Failed)
            .map(|(service, _)| service.clone())
            .collect();
        // The project's root cause is the earliest failing unit in dependency
        // order, so it does not change with which worker finished first.
        let cause = boot.first_cause(&order);

        if daemon.boot_active(boot_epoch) {
            daemon.ensure_monitoring()?;
        }
        Ok(BootFailures::new(failed, cause))
    }

    /// Combines per-project snapshots into the supervisor status view.
    fn aggregate_snapshots(mut snapshots: Vec<StatusSnapshot>) -> StatusSnapshot {
        let Some(mut aggregate) = snapshots.first().cloned() else {
            return StatusSnapshot::empty();
        };

        aggregate.units.clear();
        for snapshot in snapshots.drain(..) {
            aggregate.units.extend(snapshot.units);
        }

        aggregate.overall_health = compute_overall_health(&aggregate.units);

        aggregate
    }

    /// Tags all project-backed units in a snapshot with supervisor project metadata.
    fn apply_project_metadata(
        snapshot: &mut StatusSnapshot,
        mode: ProjectRunMode,
        config_path: &Path,
    ) {
        let config_path = config_path.to_string_lossy().to_string();
        for unit in &mut snapshot.units {
            if let Some(project) = unit.project.as_mut() {
                project.mode = mode;
                project.config_path = Some(config_path.clone());
            }
        }
    }

    /// Collects a status snapshot for one project daemon.
    fn collect_daemon_snapshot(
        daemon: &Daemon,
        metrics_store: &MetricsHandle,
        spawn_manager: &DynamicSpawnManager,
        mode: StatusSnapshotMode,
        run_mode: ProjectRunMode,
        config_path: &Path,
        valid_cron_hashes: Option<&HashSet<String>>,
    ) -> Result<StatusSnapshot, SupervisorError> {
        let config = daemon.config();
        let pid_handle = daemon.pid_file_handle();
        let state_handle = daemon.service_state_handle();

        let mut snapshot = match valid_cron_hashes {
            Some(valid_cron_hashes) => collect_runtime_snapshot_with_cron_hashes(
                Arc::clone(&config),
                &pid_handle,
                &state_handle,
                Some(metrics_store),
                Some(spawn_manager),
                mode,
                Some(valid_cron_hashes),
            ),
            None => collect_runtime_snapshot(
                Arc::clone(&config),
                &pid_handle,
                &state_handle,
                Some(metrics_store),
                Some(spawn_manager),
                mode,
            ),
        }
        .map_err(SupervisorError::Status)?;
        Self::apply_project_metadata(&mut snapshot, run_mode, config_path);
        Ok(snapshot)
    }

    /// Builds an aggregate status snapshot across every managed project from the
    /// shared project list, so the background refresher reflects extra projects
    /// (including those a multi-project file fanned out) without holding `&self`.
    fn collect_projects_snapshot(
        projects: &Arc<RwLock<Vec<CronProjectRuntime>>>,
        metrics_store: &MetricsHandle,
        spawn_manager: &DynamicSpawnManager,
        mode: StatusSnapshotMode,
    ) -> Result<StatusSnapshot, StatusError> {
        let runtimes = match projects.read() {
            Ok(guard) => guard.clone(),
            Err(_) => return Ok(StatusSnapshot::empty()),
        };

        let mut valid_cron_hashes = HashSet::new();
        for runtime in &runtimes {
            valid_cron_hashes.extend(cron_hashes_for_config(runtime.config.as_ref()));
        }

        let mut snapshots = Vec::with_capacity(runtimes.len());
        for runtime in &runtimes {
            // Best-effort per project: a single project momentarily failing to
            // collect (e.g. it is still recording PIDs from an async boot, or a
            // state file is being rewritten) must NOT discard the whole aggregate.
            // Aborting the tick would freeze the served cache for EVERY project —
            // making status lie that healthy processes are stopped — until the one
            // straggler recovered. Skip the straggler this tick; it converges on
            // the next one while its 119 healthy siblings stay honest.
            match Self::collect_daemon_snapshot(
                &runtime.daemon,
                metrics_store,
                spawn_manager,
                mode,
                runtime.mode,
                &runtime.config_path,
                Some(&valid_cron_hashes),
            ) {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(err) => error!(
                    "status refresh skipped project '{}' this tick: {err}",
                    runtime.project_id
                ),
            }
        }

        Ok(Self::aggregate_snapshots(snapshots))
    }

    /// Returns cron hashes for all projects currently managed by the supervisor.
    fn managed_cron_hashes(&self) -> HashSet<String> {
        let mut hashes = HashSet::new();
        if self.primary_active {
            hashes.extend(cron_hashes_for_config(self.daemon.config().as_ref()));
        }
        for project in self.extra_projects.values() {
            hashes.extend(cron_hashes_for_config(project.daemon.config().as_ref()));
        }
        hashes
    }

    /// Collects a fresh aggregate snapshot across all loaded projects.
    fn collect_aggregate_snapshot(
        &self,
        live_request: bool,
    ) -> Result<StatusSnapshot, SupervisorError> {
        let primary_config = self.daemon.config();
        let primary_mode = if live_request {
            Self::live_status_snapshot_mode(primary_config.as_ref())
        } else {
            Self::status_snapshot_mode(primary_config.as_ref())
        };
        let valid_cron_hashes = self.managed_cron_hashes();
        let mut snapshots = Vec::with_capacity(self.extra_projects.len() + 1);
        if self.primary_active {
            snapshots.push(Self::collect_daemon_snapshot(
                &self.daemon,
                &self.metrics_store,
                &self.spawn_manager,
                primary_mode,
                self.primary_project_mode,
                &self.config_path,
                Some(&valid_cron_hashes),
            )?);
        }

        for project in self.extra_projects.values() {
            let config = project.daemon.config();
            let mode = if live_request {
                Self::live_status_snapshot_mode(config.as_ref())
            } else {
                Self::status_snapshot_mode(config.as_ref())
            };
            snapshots.push(Self::collect_daemon_snapshot(
                &project.daemon,
                &self.metrics_store,
                &self.spawn_manager,
                mode,
                project.mode,
                &project.config_path,
                Some(&valid_cron_hashes),
            )?);
        }

        Ok(Self::aggregate_snapshots(snapshots))
    }

    /// Returns project ids whose loaded config defines the given service.
    fn projects_containing_service(&self, service_name: &str) -> Vec<String> {
        let mut projects = Vec::new();
        let primary_config = self.daemon.config();
        if primary_config.services.contains_key(service_name) {
            projects.push(primary_config.project.id.clone());
        }

        for (project_id, project) in &self.extra_projects {
            if project.daemon.config().services.contains_key(service_name) {
                projects.push(project_id.clone());
            }
        }

        projects
    }

    /// Clears captured logs for a service (or all services) inside the
    /// supervisor: truncates the on-disk files AND drops the in-memory live-log
    /// buffer the log reader serves from. Doing this CLI-side would leave the
    /// buffer intact, so the reader would keep replaying "cleared" lines.
    /// The single project declaring `service`, if exactly one does.
    ///
    /// Returns `None` when several projects declare the name: picking one would
    /// silently act on a project the user never named, which is how an unscoped
    /// clear came to wipe the wrong logs. The caller decides what an unresolved
    /// name means for its command.
    fn project_declaring_service(&self, service: &str) -> Option<String> {
        match self.projects_declaring_service(service).as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        }
    }

    /// Every project declaring `service`, primary first.
    fn projects_declaring_service(&self, service: &str) -> Vec<String> {
        let mut owners = Vec::new();

        let primary = self.daemon.config();
        if primary.services.contains_key(service) {
            owners.push(primary.project.id.clone());
        }
        for project in self.extra_projects.values() {
            let config = project.daemon.config();
            if config.services.contains_key(service) {
                owners.push(config.project.id.clone());
            }
        }

        owners
    }

    fn clear_logs(
        &self,
        service: Option<&str>,
        project_filter: Option<&str>,
    ) -> Result<(), SupervisorError> {
        let manager = LogManager::new();
        let mut targets: Vec<(String, String)> = Vec::new();
        match service {
            Some(name) => {
                // An unscoped clear must find the project that actually declares
                // the service. Defaulting to the primary silently cleared the
                // wrong project's logs — and reported success — for any service
                // living in one of the others.
                let project = match project_filter {
                    Some(project) => project.to_string(),
                    None => match self.project_declaring_service(name) {
                        Some(project) => project,
                        // Several projects declare this name, or none does.
                        // Falling back to the primary would destroy logs the
                        // request never named; make the caller disambiguate.
                        None => {
                            let owners = self.projects_declaring_service(name);
                            let diag = if owners.is_empty() {
                                crate::stop::service_not_found(name)
                            } else {
                                crate::logs_cmd::ambiguous_service(name, &owners)
                            };
                            return Err(ProcessManagerError::Diag(Box::new(diag)).into());
                        }
                    },
                };
                targets.push((project, name.to_string()));
            }
            None => {
                // A `-p` here scopes the clear to that project. Ignoring it
                // wiped every project's logs on a request that named one — the
                // same cross-project reach the loose rebuild exists to end.
                let wanted = |id: &str| project_filter.is_none_or(|want| want == id);

                let primary = self.daemon.config();
                if wanted(&primary.project.id) {
                    for name in primary.services.keys() {
                        targets.push((primary.project.id.clone(), name.clone()));
                    }
                }
                for project in self.extra_projects.values() {
                    let config = project.daemon.config();
                    if !wanted(&config.project.id) {
                        continue;
                    }
                    for name in config.services.keys() {
                        targets.push((config.project.id.clone(), name.clone()));
                    }
                }
            }
        }
        for (project, name) in targets {
            manager
                .clear_service_logs(&project, &name)
                .map_err(SupervisorError::from)?;
            crate::logs::clear_live_log(&project, &name);
        }
        Ok(())
    }

    /// Resolves the target project for a service request, rejecting ambiguous selectors.
    fn resolve_service_target_project(
        &self,
        service_name: &str,
        project: Option<&str>,
        selector_project: Option<&str>,
        config_project: Option<&str>,
    ) -> Result<String, SupervisorError> {
        if let (Some(flag), Some(selector_project)) = (project, selector_project)
            && flag != selector_project
        {
            return Err(ProcessManagerError::Diag(Box::new(start::project_mismatch(
                flag,
                selector_project,
            )))
            .into());
        }

        let requested_project = project.or(selector_project);
        let matching_projects = self.projects_containing_service(service_name);

        if let Some(requested) = requested_project {
            // A requested project is valid as long as it actually declares the
            // service. For a multi-project config the loaded config id is only
            // ONE of several projects, so comparing against it would wrongly
            // reject sibling projects — validate against the real membership.
            if matching_projects.iter().any(|p| p == requested) {
                return Ok(requested.to_string());
            }
            if let Some(config_project) = config_project
                && requested != config_project
            {
                return Err(ProcessManagerError::Diag(Box::new(
                    start::project_mismatch(requested, config_project),
                ))
                .into());
            }
            return Ok(requested.to_string());
        }

        if let Some(config_project) = config_project {
            return Ok(config_project.to_string());
        }

        match matching_projects.as_slice() {
            [project_id] => Ok(project_id.clone()),
            [] => Ok(self.daemon.config().project.id.clone()),
            projects => Err(ProcessManagerError::Diag(Box::new(
                start::ambiguous_service(service_name, projects),
            ))
            .into()),
        }
    }

    /// Starts one service in the selected project without loading or starting the whole project.
    fn start_single_service_target(
        &self,
        selector: &str,
        project: Option<&str>,
    ) -> Result<(String, String), SupervisorError> {
        let (selector_project, service_name) = split_project_selector(selector)
            .map(|(project_id, service_name)| (Some(project_id), service_name))
            .unwrap_or((None, selector));

        let target_project = self.resolve_service_target_project(
            service_name,
            project,
            selector_project,
            None,
        )?;
        let primary_project = self.daemon.config().project.id.clone();

        let (daemon, service_config) = if target_project == primary_project {
            let config_handle = self.daemon.config();
            let service_config = config_handle
                .services
                .get(service_name)
                .cloned()
                .ok_or_else(|| ProcessManagerError::DependencyError {
                    service: service_name.into(),
                    dependency: "service not defined".into(),
                })?;
            (&self.daemon, service_config)
        } else {
            let Some(project_runtime) = self.extra_projects.get(&target_project) else {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "project '{target_project}' is not managed by this supervisor"
                    ),
                )
                .into());
            };
            let config_handle = project_runtime.daemon.config();
            let service_config = config_handle
                .services
                .get(service_name)
                .cloned()
                .ok_or_else(|| ProcessManagerError::DependencyError {
                    service: service_name.into(),
                    dependency: "service not defined".into(),
                })?;
            (&project_runtime.daemon, service_config)
        };

        reject_direct_cron_control(
            &service_config,
            service_name,
            &target_project,
            "started",
        )?;

        // Explicitly naming a service is a direct order to run THIS one, so it
        // overrides a `skip` default — a skipped service you ask for by name must
        // actually start, not silently no-op.
        let mut service_config = service_config;
        service_config.skip = None;

        daemon.begin_boot();
        daemon.start_service(service_name, &service_config)?;
        daemon.ensure_monitoring()?;

        if let Some(ref spawn) = service_config.spawn
            && let Some(SpawnMode::Dynamic) = spawn.mode
            && let Ok(pid_file) = daemon.pid_file_handle().lock()
            && let Some(&pid) = pid_file.services().get(service_name)
        {
            self.spawn_manager
                .register_service_pid(&target_project, service_name, pid);
        }

        Ok((target_project, service_name.to_string()))
    }

    /// Starts all non-cron services in one managed project.
    fn start_project_target(&mut self, project_id: &str) -> Result<(), SupervisorError> {
        let primary_project = self.daemon.config().project.id.clone();
        if project_id == primary_project {
            if self.primary_active && !self.daemon.needs_start() {
                self.sync_cron_projects()?;
                return Ok(());
            }
            self.primary_active = true;
            let failed = Self::start_project_services(
                &self.daemon,
                self.daemon.config().as_ref(),
                None,
                &self.spawn_manager,
                None,
            )?;
            self.sync_cron_projects()?;
            if !failed.is_empty() {
                return Err(failed.into_error(project_id).into());
            }
            return Ok(());
        }

        let Some(project_runtime) = self.extra_projects.get(project_id) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' is not managed by this supervisor"),
            )
            .into());
        };

        if !project_runtime.daemon.needs_start() {
            return Ok(());
        }

        let failed = Self::start_project_services(
            &project_runtime.daemon,
            project_runtime.daemon.config().as_ref(),
            None,
            &self.spawn_manager,
            None,
        )?;
        if !failed.is_empty() {
            return Err(failed.into_error(project_id).into());
        }
        Ok(())
    }

    /// Restarts all non-cron services in one managed project.
    fn restart_project_target(
        &mut self,
        project_id: &str,
        config_path: Option<&Path>,
    ) -> Result<(), SupervisorError> {
        let primary_project = self.daemon.config().project.id.clone();
        let stored = match config_path {
            Some(path) => path.to_path_buf(),
            None if project_id == primary_project => self.config_path.clone(),
            None => self
                .extra_projects
                .get(project_id)
                .map(|runtime| runtime.config_path.clone())
                .ok_or_else(|| {
                    ProcessManagerError::Diag(Box::new(crate::stop::project_not_found(
                        project_id,
                    )))
                })?,
        };
        let (resolved, mut configs) = self.load_restart_manifest(&stored)?;
        let Some(index) = configs
            .iter()
            .position(|config| config.project.id == project_id)
        else {
            return Err(ProcessManagerError::Diag(Box::new(
                crate::stop::project_not_found(project_id),
            ))
            .into());
        };
        let config = configs.swap_remove(index);

        if project_id == primary_project {
            self.reconcile_primary_project(config)?;
            self.config_path = resolved;
            ipc::write_config_hint(&self.config_path)?;
            self.respawn_status_refresher()?;
        } else {
            self.reconcile_extra_project(config, resolved)?;
        }
        Ok(())
    }

    /// Resolves, trust-checks, parses, and validates a restart manifest before
    /// any managed process is touched.
    fn load_restart_manifest(
        &self,
        path: &Path,
    ) -> Result<(PathBuf, Vec<Config>), SupervisorError> {
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.config_path
                .parent()
                .unwrap_or_else(|| Path::new("/"))
                .join(path)
        };
        let resolved = resolved.canonicalize().unwrap_or(resolved);
        let loaded = (|| -> Result<Vec<Config>, SupervisorError> {
            let file = runtime::open_trusted_config(&resolved)?;
            let configs = load_projects_from_file(file, &resolved)?;
            if configs.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("config at {} declared no projects", resolved.display()),
                )
                .into());
            }
            let mut ids = BTreeSet::new();
            for config in &configs {
                if !ids.insert(config.project.id.clone()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("duplicate project id '{}'", config.project.id),
                    )
                    .into());
                }
            }
            Ok(configs)
        })();
        match loaded {
            Ok(configs) => Ok((resolved, configs)),
            Err(err) => Err(ProcessManagerError::Diag(Box::new(
                crate::restart::manifest_rejected(err.to_string()),
            ))
            .into()),
        }
    }

    /// Reconciles an active primary project by manifest delta and an inactive
    /// primary project to its complete target.
    fn reconcile_primary_project(
        &mut self,
        new_config: Config,
    ) -> Result<(), SupervisorError> {
        let old_config = self.daemon.config();
        let old_metrics = self.metrics_store.clone();
        let metrics_settings = new_config
            .metrics
            .to_settings(new_config.project_dir.as_deref().map(Path::new));
        let metrics_store = metrics::shared_store(metrics_settings)?;
        let diff =
            crate::restart::ManifestDiff::compute(old_config.as_ref(), &new_config);
        let affected = if self.primary_active {
            Self::reconcile_targets(&new_config, &diff)?
        } else {
            new_config.services.keys().cloned().collect()
        };

        self.stop_primary_workers();
        let mut stop_error = None;
        for name in &diff.removed {
            if let Err(err) = self.daemon.stop_service(name) {
                error!("Failed to stop removed service '{name}': {err}");
                stop_error.get_or_insert(err);
            }
        }
        if let Some(err) = stop_error {
            if let Err(restore_err) =
                self.restore_primary_project(old_config, old_metrics)
            {
                error!(
                    "Failed to restore primary project after stop failure: {restore_err}"
                );
            }
            return Err(err.into());
        }

        self.daemon.set_config(new_config);
        self.primary_active = true;
        self.daemon.begin_boot();
        let restart_result = self.daemon.restart_services_subset(&affected);
        let sync_result = self.sync_cron_projects();
        self.metrics_store = metrics_store;
        let workers_result = self.start_primary_workers();
        if let Err(failure) = restart_result {
            error!("Project reconcile did not complete: {}", failure.cause);
            let failed = Self::reconcile_failures(&failure);
            let cause = failure.cause.to_string();
            sync_result?;
            workers_result?;
            return Err(ProcessManagerError::Diag(Box::new(
                crate::restart::reconcile_incomplete(failed.as_deref(), Some(&cause)),
            ))
            .into());
        }
        sync_result?;
        workers_result?;
        Ok(())
    }

    /// Reconciles an additional project in place so unchanged services retain
    /// their identity and changed services use their configured deployment strategy.
    fn reconcile_extra_project(
        &mut self,
        new_config: Config,
        config_path: PathBuf,
    ) -> Result<(), SupervisorError> {
        let project_id = new_config.project.id.clone();
        let daemon = self
            .extra_projects
            .get(&project_id)
            .map(|runtime| runtime.daemon.clone())
            .ok_or_else(|| {
                ProcessManagerError::Diag(Box::new(crate::stop::project_not_found(
                    &project_id,
                )))
            })?;
        Self::register_spawn_limits_for_config(&self.spawn_manager, &new_config)?;
        let old_config = daemon.config();
        let diff =
            crate::restart::ManifestDiff::compute(old_config.as_ref(), &new_config);
        let affected = Self::reconcile_targets(&new_config, &diff)?;

        let mut stop_error = None;
        for name in &diff.removed {
            if let Err(err) = daemon.stop_service(name) {
                error!("Failed to stop removed service '{name}': {err}");
                stop_error.get_or_insert(err);
            }
        }
        if let Some(err) = stop_error {
            self.restore_extra_project(&project_id, &daemon)?;
            return Err(err.into());
        }

        daemon.set_config(new_config);
        // `AddProject` attaches nothing up front, so this daemon reports its
        // reconcile to nobody unless the operation attaches it here. Runs on
        // this thread, so no lease is needed to keep the journal open.
        let _watch = self
            .active_op
            .as_ref()
            .map(|(op, journal)| daemon.watch(op, journal.clone()));

        daemon.begin_boot();
        let restart_result = daemon.restart_services_subset(&affected);
        if let Some(runtime) = self.extra_projects.get_mut(&project_id) {
            runtime.config_path = config_path;
        }
        let sync_result = self.sync_cron_projects();
        self.refresh_status_cache();
        self.respawn_status_refresher()?;
        if let Err(failure) = restart_result {
            let failed = Self::reconcile_failures(&failure);
            let cause = failure.cause.to_string();
            sync_result?;
            return Err(ProcessManagerError::Diag(Box::new(
                crate::restart::reconcile_incomplete(failed.as_deref(), Some(&cause)),
            ))
            .into());
        }
        sync_result?;
        Ok(())
    }

    /// Stops primary-project background workers before its daemon state changes.
    fn stop_primary_workers(&mut self) {
        if let Some(collector) = self.metrics_collector.take() {
            collector.stop();
        }
        if let Some(refresher) = self.status_refresher.take() {
            refresher.stop();
        }
        self.daemon.shutdown_monitor();
    }

    /// Refreshes primary status and starts its status and metrics workers.
    fn start_primary_workers(&mut self) -> Result<(), SupervisorError> {
        self.refresh_status_cache();
        self.respawn_status_refresher()?;
        self.metrics_collector = Some(MetricsCollector::spawn(
            self.metrics_store.clone(),
            self.daemon.config(),
            self.daemon.pid_file_handle(),
            self.daemon.service_state_handle(),
        )?);
        Ok(())
    }

    /// Restores the previous primary manifest and workloads after a teardown
    /// failure prevents the new manifest from being applied safely.
    fn restore_primary_project(
        &mut self,
        config: Arc<Config>,
        metrics_store: MetricsHandle,
    ) -> Result<(), SupervisorError> {
        self.daemon.cancel_boot();
        self.daemon.shutdown_monitor();
        let _ = self.daemon.stop_services();
        self.daemon.set_config((*config).clone());
        self.primary_active = true;
        let failed = Self::start_project_services(
            &self.daemon,
            config.as_ref(),
            None,
            &self.spawn_manager,
            None,
        )?;
        let sync_result = self.sync_cron_projects();
        self.metrics_store = metrics_store;
        self.start_primary_workers()?;
        sync_result?;
        if failed.is_empty() {
            Ok(())
        } else {
            Err(ProcessManagerError::Diag(Box::new(
                crate::restart::reconcile_incomplete(Some(failed.services()), None),
            ))
            .into())
        }
    }

    /// Returns services directly changed by a manifest plus every transitive
    /// dependent whose lifecycle must be reevaluated.
    fn reconcile_targets(
        config: &Config,
        diff: &crate::restart::ManifestDiff,
    ) -> Result<HashSet<String>, ProcessManagerError> {
        let order = config.service_start_order()?;
        let mut affected: HashSet<String> = if diff.is_empty() {
            config.services.keys().cloned().collect()
        } else {
            diff.added.union(&diff.changed).cloned().collect()
        };
        for name in order {
            if config.services.get(&name).is_some_and(|service| {
                service.depends_on.as_ref().is_some_and(|dependencies| {
                    dependencies
                        .iter()
                        .any(|dependency| affected.contains(dependency.service()))
                })
            }) {
                affected.insert(name);
            }
        }
        Ok(affected)
    }

    /// Extracts stable failed-unit names from a reconcile error, falling back to
    /// every affected unit when the originating error carries no unit list.
    fn reconcile_failures(
        failure: &crate::daemon::RestartFailure,
    ) -> Option<Vec<String>> {
        let mut failed = match (&failure.failed_services, &failure.cause) {
            (Some(services), _) => services.clone(),
            (None, ProcessManagerError::ServicesNotRunning { services }) => {
                services.clone()
            }
            (None, _) => return None,
        };
        failed.sort_unstable();
        failed.dedup();
        (!failed.is_empty()).then_some(failed)
    }

    /// Replaces an extra project with a freshly loaded runtime and starts it.
    fn replace_extra_project_runtime(
        &mut self,
        config: Config,
        config_path: PathBuf,
    ) -> Result<(), SupervisorError> {
        let project_id = config.project.id.clone();
        let Some(existing) = self.extra_projects.get(&project_id) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' is not managed by this supervisor"),
            )
            .into());
        };
        let mode = existing.mode;
        let old_daemon = existing.daemon.clone();
        let old_path = existing.config_path.clone();
        Self::register_spawn_limits_for_config(&self.spawn_manager, &config)?;
        let mut replacement = Daemon::from_config(config, self.detach_children)?;
        replacement.set_timeouts(self.timeouts.clone());
        replacement.set_pipe_stderr(self.pipe_stderr);
        replacement.set_op_slot(self.op_slot.clone());

        old_daemon.cancel_boot();
        old_daemon.shutdown_monitor();
        if let Err(err) = old_daemon.stop_services() {
            if let Err(restore_err) = self.restore_extra_project(&project_id, &old_daemon)
            {
                error!(
                    "Failed to restore project '{project_id}' after stop failure: {restore_err}"
                );
            }
            return Err(err.into());
        }
        if let Ok(mut projects) = self.boot_projects.write() {
            projects.insert(project_id.clone(), replacement.clone());
        }

        // The replacement daemon is what reports this reboot's units, and it
        // did not exist when the command was watched. No lease is needed: the
        // start below runs on this thread, so the journal outlives it already.
        let _watch = self
            .active_op
            .as_ref()
            .map(|(op, journal)| replacement.watch(op, journal.clone()));

        let start_result = Self::start_project_services(
            &replacement,
            replacement.config().as_ref(),
            None,
            &self.spawn_manager,
            None,
        );
        let start_result = start_result.and_then(|failed| {
            if failed.is_empty() {
                Ok(())
            } else {
                Err(ProcessManagerError::Diag(Box::new(
                    crate::restart::reconcile_incomplete(Some(failed.services()), None),
                ))
                .into())
            }
        });
        if let Err(err) = start_result {
            replacement.cancel_boot();
            replacement.shutdown_monitor();
            let _ = replacement.stop_services();
            if let Err(restore_err) = self.restore_extra_project(&project_id, &old_daemon)
            {
                error!(
                    "Failed to restore project '{project_id}' after replacement failure: {restore_err}"
                );
            }
            return Err(err);
        }

        let surviving: std::collections::HashSet<String> =
            replacement.config().services.keys().cloned().collect();
        crate::logs::retain_project_live_logs(&project_id, &surviving);
        self.extra_projects.insert(
            project_id.clone(),
            ProjectRuntime {
                daemon: replacement,
                mode,
                config_path,
            },
        );
        if let Err(err) = self.sync_cron_projects() {
            if let Some(failed) = self.extra_projects.remove(&project_id) {
                failed.daemon.cancel_boot();
                failed.daemon.shutdown_monitor();
                let _ = failed.daemon.stop_services();
            }
            self.extra_projects.insert(
                project_id.clone(),
                ProjectRuntime {
                    daemon: old_daemon.clone(),
                    mode,
                    config_path: old_path,
                },
            );
            if let Err(restore_err) = self.restore_extra_project(&project_id, &old_daemon)
            {
                error!(
                    "Failed to restore project '{project_id}' after scheduler failure: {restore_err}"
                );
            }
            return Err(err);
        }
        Ok(())
    }

    fn restore_extra_project(
        &self,
        project_id: &str,
        daemon: &Daemon,
    ) -> Result<(), SupervisorError> {
        if let Ok(mut projects) = self.boot_projects.write() {
            projects.insert(project_id.to_string(), daemon.clone());
        }
        let failed = Self::start_project_services(
            daemon,
            daemon.config().as_ref(),
            None,
            &self.spawn_manager,
            None,
        )?;
        self.sync_cron_projects()?;
        if failed.is_empty() {
            Ok(())
        } else {
            Err(ProcessManagerError::Diag(Box::new(
                crate::restart::reconcile_incomplete(Some(failed.services()), None),
            ))
            .into())
        }
    }

    /// Creates supervisor with config.
    pub fn new(
        config_path: PathBuf,
        detach_children: bool,
        service_filter: Option<String>,
    ) -> Result<Self, SupervisorError> {
        Self::new_with_mode(
            config_path,
            detach_children,
            service_filter,
            ProjectRunMode::Daemon,
        )
    }

    /// Creates supervisor with config and project mode.
    pub fn new_with_mode(
        config_path: PathBuf,
        detach_children: bool,
        service_filter: Option<String>,
        primary_project_mode: ProjectRunMode,
    ) -> Result<Self, SupervisorError> {
        // Refuse to boot over a migration that stopped partway: the layout would
        // be part legacy and part migrated, and reading it would mean adopting
        // some services under their old identity and some under their new one.
        // A journal that cannot be read is treated the same way — it exists to
        // describe data movement, so being unable to tell whether one is in
        // flight is itself a reason not to start.
        match crate::migrate_state::pending_journal(&runtime::state_dir()) {
            Ok(Some(journal)) => {
                return Err(io::Error::other(format!(
                    "a state migration stopped after the {:?} phase; run `sysg migrate-state` to finish it",
                    journal.phase
                ))
                .into());
            }
            Err(err) => {
                return Err(io::Error::other(format!(
                    "the state migration journal could not be read ({err}); resolve it before starting"
                ))
                .into());
            }
            Ok(None) => {}
        }

        let config_path = if config_path.is_absolute() {
            config_path
        } else {
            std::env::current_dir()?.join(config_path)
        };
        let config_path = config_path.canonicalize().unwrap_or(config_path);
        let mut projects = {
            let trusted = runtime::open_trusted_config(&config_path)?;
            load_projects_from_file(trusted, &config_path)?
        };
        if projects.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("config at {} declared no projects", config_path.display()),
            )
            .into());
        }
        let config = projects.remove(0);
        Self::from_primary_config(
            config_path,
            config,
            projects,
            detach_children,
            service_filter,
            primary_project_mode,
        )
    }

    /// Builds a supervisor from an already parsed primary project and optional
    /// projects awaiting the normal initial boot.
    fn from_primary_config(
        config_path: PathBuf,
        config: Config,
        pending_projects: Vec<Config>,
        detach_children: bool,
        service_filter: Option<String>,
        primary_project_mode: ProjectRunMode,
    ) -> Result<Self, SupervisorError> {
        let cron_manager = CronManager::new();
        cron_manager.sync_from_config(&config)?;

        let op_slot = OpSlot::new();
        let mut daemon = Daemon::from_config(config.clone(), detach_children)?;
        daemon.set_op_slot(op_slot.clone());
        let config_arc = daemon.config();
        let cron_projects = Arc::new(RwLock::new(vec![CronProjectRuntime {
            project_id: config_arc.project.id.clone(),
            daemon: daemon.clone(),
            config: Arc::clone(&config_arc),
            mode: primary_project_mode,
            config_path: config_path.clone(),
        }]));
        let metrics_settings = config_arc
            .metrics
            .to_settings(config_arc.project_dir.as_deref().map(Path::new));
        let metrics_store = metrics::shared_store(metrics_settings)?;
        let status_cache = StatusCache::new(StatusSnapshot::empty());

        let spawn_manager = DynamicSpawnManager::new();
        Self::register_spawn_limits_for_config(&spawn_manager, &config)?;
        let boot_projects = Arc::new(RwLock::new(HashMap::from([(
            config_arc.project.id.clone(),
            daemon.clone(),
        )])));

        Ok(Self {
            config_path,
            daemon,
            timeouts: SupervisorTimeouts::default(),
            detach_children,
            cron_manager,
            service_filter,
            status_cache,
            status_refresher: None,
            metrics_store,
            metrics_collector: None,
            spawn_manager,
            pipe_stderr: false,
            primary_project_mode,
            primary_active: true,
            extra_projects: BTreeMap::new(),
            cron_projects,
            op_slot,
            pending_projects,
            boot_journal: BootJournal::new(),
            op_journals: Arc::new(RwLock::new(HashMap::new())),
            active_op: None,
            op_lease: None,
            boot_projects,
            boots: Arc::new(RwLock::new(HashMap::new())),
            upgrading: Arc::new(AtomicBool::new(false)),
            cron_gate: Arc::new(std::sync::Mutex::new(())),
            handoff: None,
        })
    }

    /// Loads the exact project named by a handoff record and verifies that its
    /// manifest did not change while the supervisor image was replaced.
    ///
    /// One identity is translated rather than matched: a pre-0.59 resident
    /// records its loose project as `__loose__`, an id no manifest can resolve
    /// to anymore — loose configs derive a per-file id now. The manifest itself
    /// is proven unchanged by the content hash, so when the record says
    /// `__loose__` and the file still parses to exactly one loose project, that
    /// project is what the resident was running, under its migrated name.
    fn load_handoff_project(
        project: &HandoffProject,
        snapshot: Option<&String>,
    ) -> Result<LoadedHandoffProject, SupervisorError> {
        let configs = match snapshot {
            Some(snapshot) => {
                match Self::assembled_manifest(&project.config_path) {
                    Ok(disk) if disk == *snapshot => {}
                    Ok(_) => warn!(
                        "manifest {} changed during supervisor handoff; resuming from its handoff snapshot — restart to apply the new manifest",
                        project.config_path.display()
                    ),
                    Err(err) => warn!(
                        "manifest {} did not load during supervisor handoff ({err}); resuming from its handoff snapshot",
                        project.config_path.display()
                    ),
                }
                crate::config::load_projects_from_snapshot(
                    snapshot,
                    &project.config_path,
                )?
            }
            None => {
                let actual_hash = ipc::manifest_content_hash(&project.config_path)
                    .map_err(|err| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "could not hash handed-off manifest {}: {err}",
                                project.config_path.display()
                            ),
                        )
                    })?;
                if actual_hash != project.config_hash {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "manifest {} changed during supervisor handoff",
                            project.config_path.display()
                        ),
                    )
                    .into());
                }
                let trusted = runtime::open_trusted_config(&project.config_path)?;
                load_projects_from_file(trusted, &project.config_path)?
            }
        };

        if let Some(config) = configs
            .iter()
            .find(|config| config.project.id == project.project_id)
        {
            return Ok(LoadedHandoffProject {
                config: config.clone(),
                legacy_id: None,
            });
        }

        if project.project_id == crate::state_store::LOOSE_PROJECT_ID
            && let [only] = configs.as_slice()
            && only.project.loose
        {
            info!(
                "Handoff project `{}` migrated to `{}` during upgrade of {}",
                project.project_id,
                only.project.id,
                project.config_path.display()
            );
            return Ok(LoadedHandoffProject {
                config: only.clone(),
                legacy_id: Some(project.project_id.clone()),
            });
        }

        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "project `{}` is absent from handed-off manifest {}",
                project.project_id,
                project.config_path.display()
            ),
        )
        .into())
    }

    /// Reconstructs a supervisor from a private state record created immediately
    /// before same-PID live re-execution.
    ///
    /// A project whose identity migrated during the upgrade (see
    /// `load_handoff_project`) has its on-disk state seeded into the derived
    /// project's store before its daemon is built: handoff adoption VERIFIES
    /// pids against the store, it never writes them, so an empty new-identity
    /// store would fail the resume that the translation just made possible.
    pub fn from_handoff(path: PathBuf) -> Result<Self, SupervisorError> {
        let state = SupervisorHandoff::load(&path)?;
        let current = LiveUpgradeInfo::current();
        if state.target_version != current.version {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "handoff targets {}, but replacement binary is {}",
                    state.target_version, current.version
                ),
            )
            .into());
        }
        let primary = state.primary.clone();

        // Resolve every project before touching anything. Identity translation
        // means a fresh id is no longer guaranteed unique by construction — a
        // migrated loose project could in principle derive an id some other
        // handed-off project already carries — and discovering that after
        // seeding or daemon construction would leave two projects silently
        // sharing one state and log namespace. Duplicates roll the upgrade back
        // before any mutation instead.
        let loaded_primary = Self::load_handoff_project(
            &primary,
            state.manifests.get(&primary.config_path),
        )?;
        let loaded_extras: Vec<(&crate::upgrade::HandoffProject, LoadedHandoffProject)> =
            state
                .projects
                .values()
                .map(|project| {
                    Self::load_handoff_project(
                        project,
                        state.manifests.get(&project.config_path),
                    )
                    .map(|loaded| (project, loaded))
                })
                .collect::<Result<_, _>>()?;
        {
            let mut fresh_ids = std::collections::BTreeSet::new();
            fresh_ids.insert(loaded_primary.config.project.id.clone());
            for (_, loaded) in &loaded_extras {
                if !fresh_ids.insert(loaded.config.project.id.clone()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "handed-off projects resolve to the same id `{}`",
                            loaded.config.project.id
                        ),
                    )
                    .into());
                }
            }
        }

        let mut remapped: Vec<(String, String)> = Vec::new();
        if let Some(legacy) = &loaded_primary.legacy_id {
            crate::migrate_state::seed_project_state_from_legacy(
                &runtime::state_dir(),
                legacy,
                &loaded_primary.config,
            )?;
            remapped.push((legacy.clone(), loaded_primary.config.project.id.clone()));
        }
        let mut supervisor = Self::from_primary_config(
            primary.config_path.clone(),
            loaded_primary.config,
            Vec::new(),
            false,
            state.service_filter.clone(),
            primary.mode,
        )?;
        supervisor.primary_active = primary.active;
        supervisor.pipe_stderr = state.pipe_stderr;
        supervisor.daemon.set_pipe_stderr(state.pipe_stderr);
        supervisor.daemon.adopt_handoff_state(&primary.daemon)?;
        if !primary.active
            && let Ok(mut projects) = supervisor.boot_projects.write()
        {
            projects.remove(&supervisor.daemon.config().project.id);
        }

        for (project, loaded) in loaded_extras {
            if let Some(legacy) = &loaded.legacy_id {
                crate::migrate_state::seed_project_state_from_legacy(
                    &runtime::state_dir(),
                    legacy,
                    &loaded.config,
                )?;
                remapped.push((legacy.clone(), loaded.config.project.id.clone()));
            }
            // Everything downstream keys off the freshly loaded id, so the map
            // entries must too. Registering under the record's key would leave a
            // migrated project addressable by a name its own config no longer
            // carries.
            let project_id = loaded.config.project.id.clone();
            Self::register_spawn_limits_for_config(
                &supervisor.spawn_manager,
                &loaded.config,
            )?;
            let mut daemon = Daemon::from_config(loaded.config, false)?;
            daemon.set_timeouts(supervisor.timeouts.clone());
            daemon.set_op_slot(supervisor.op_slot.clone());
            daemon.set_pipe_stderr(state.pipe_stderr);
            daemon.adopt_handoff_state(&project.daemon)?;
            if project.active
                && let Ok(mut projects) = supervisor.boot_projects.write()
            {
                projects.insert(project_id.clone(), daemon.clone());
            }
            supervisor.extra_projects.insert(
                project_id,
                ProjectRuntime {
                    daemon,
                    mode: project.mode,
                    config_path: project.config_path.clone(),
                },
            );
        }
        supervisor.sync_cron_projects()?;

        // Log-pipe records name the project their writers append under; a
        // migrated one must write where the new identity's readers look.
        let log_pipes: Vec<crate::upgrade::HandoffLogPipe> = state
            .log_pipes
            .iter()
            .map(|pipe| {
                let mut pipe = pipe.clone();
                if let Some((_, new_id)) =
                    remapped.iter().find(|(legacy, _)| *legacy == pipe.project)
                {
                    pipe.project = new_id.clone();
                }
                pipe
            })
            .collect();
        crate::logs::resume_log_pipe_handoff(&log_pipes)?;

        // The resident that wrote this handoff predates the loose registry, so
        // a migrated project was never recorded for cold-boot restore.
        for (_, new_id) in &remapped {
            let (config_path, mode) = if *new_id == supervisor.daemon.config().project.id
            {
                (
                    supervisor.config_path.clone(),
                    supervisor.primary_project_mode,
                )
            } else if let Some(project) = supervisor.extra_projects.get(new_id) {
                (project.config_path.clone(), project.mode)
            } else {
                continue;
            };
            supervisor.record_loose_manifest(&config_path, new_id, mode);
        }

        supervisor.handoff = Some(LoadedHandoff { path, state });
        Ok(supervisor)
    }

    /// Sets whether to pipe stderr from services to stdout.
    pub fn set_pipe_stderr(&mut self, pipe_stderr: bool) {
        self.pipe_stderr = pipe_stderr;
        self.daemon.set_pipe_stderr(pipe_stderr);
        for project in self.extra_projects.values_mut() {
            project.daemon.set_pipe_stderr(pipe_stderr);
        }
    }

    /// Returns the project runtimes that own cron-capable configs.
    fn cron_project_runtimes(&self) -> Vec<CronProjectRuntime> {
        let mut projects = Vec::new();
        if self.primary_active {
            projects.push(CronProjectRuntime {
                project_id: self.daemon.config().project.id.clone(),
                daemon: self.daemon.clone(),
                config: self.daemon.config(),
                mode: self.primary_project_mode,
                config_path: self.config_path.clone(),
            });
        }

        projects.extend(self.extra_projects.iter().map(|(project_id, project)| {
            CronProjectRuntime {
                project_id: project_id.clone(),
                daemon: project.daemon.clone(),
                config: project.daemon.config(),
                mode: project.mode,
                config_path: project.config_path.clone(),
            }
        }));

        projects
    }

    /// Synchronizes cron registration and scheduler routing for all managed projects.
    fn sync_cron_projects(&self) -> Result<(), SupervisorError> {
        let projects = self.cron_project_runtimes();

        match self.cron_projects.write() {
            Ok(mut guard) => *guard = projects.clone(),
            Err(err) => warn!("Failed to update cron project routing: {}", err),
        }

        self.cron_manager
            .sync_from_configs(projects.iter().map(|project| project.config.as_ref()))?;

        Ok(())
    }

    /// Runs the event loop.
    pub fn run(&mut self) -> Result<(), SupervisorError> {
        match self.run_internal() {
            Err(SupervisorError::Io(ref err))
                if err.kind() == io::ErrorKind::PermissionDenied =>
            {
                warn!(
                    "Supervisor IPC unavailable due to permissions; running direct mode"
                );
                self.daemon
                    .start_services_blocking()
                    .map_err(SupervisorError::Process)
            }
            Err(err) => Err(err),
            Ok(()) => Ok(()),
        }
    }

    /// Starts the primary project's services in dependency order, tolerating
    /// per-unit failures so one bad unit cannot abort the whole boot.
    fn boot_primary_services(&mut self) -> Result<(), SupervisorError> {
        let _op = self.op_slot.guard(format!(
            "starting project '{}'",
            self.daemon.config().project.id
        ));
        let config = self.daemon.config();
        Self::start_project_services(
            &self.daemon,
            &config,
            self.service_filter.as_deref(),
            &self.spawn_manager,
            Some(&self.boot_journal),
        )?;

        if self.daemon.boot_cancelled() {
            self.pending_projects.clear();
        }

        let pending = std::mem::take(&mut self.pending_projects);
        let config_path = self.config_path.clone();
        if !pending.is_empty() {
            info!(
                "Booting {} additional project(s) from multi-project config",
                pending.len()
            );
        }
        for extra in pending {
            let project_id = extra.project.id.clone();
            if let Err(err) = self.boot_extra_project(extra, &config_path) {
                error!(
                    "Failed to boot project '{project_id}' from multi-project config: {err}. Continuing with remaining projects."
                );
            }
        }

        self.boot_registered_loose_projects();

        Ok(())
    }

    /// Counts the units that came up and failed in the boot journal so far.
    fn boot_tally(&self) -> (usize, usize) {
        self.boot_journal
            .snapshot()
            .iter()
            .fold((0usize, 0usize), |(up, down), frame| match frame {
                BootFrame::Unit { outcome, .. } if outcome.succeeded() => (up + 1, down),
                BootFrame::Unit { .. } => (up, down + 1),
                _ => (up, down),
            })
    }

    /// Registers and synchronously starts one additional project from a
    /// multi-project manifest during boot. Unlike the live AddProject path this
    /// Re-registers every loose manifest recorded in the loose registry.
    ///
    /// A loose manifest is its own project, keyed by its path, and `config_hint`
    /// holds exactly one path — so without this a cold boot would restore only
    /// whichever loose config was started last and silently drop the rest. The
    /// registry is the durable set; boot replays it.
    ///
    /// Failures are logged and skipped rather than propagated: one loose
    /// manifest that has been deleted or gone invalid since it was registered
    /// must not take down the boot of every other project.
    fn boot_registered_loose_projects(&mut self) {
        let registry = match crate::loose_registry::LooseRegistry::load() {
            Ok(registry) => registry,
            Err(err) => {
                error!("Loose registry unreadable, skipping loose restore: {err}");
                return;
            }
        };

        let primary = self.daemon.config().project.id.clone();
        // The manifest that booted this supervisor is loose too, and it reached
        // here as the primary rather than through `add_project_config` — so
        // nothing has recorded it. Without this, restarting from some other
        // config would leave it out of the restore set entirely.
        if self.daemon.config().project.loose {
            let config_path = self.config_path.clone();
            let mode = self.primary_project_mode;
            self.record_loose_manifest(&config_path, &primary, mode);
        }
        for entry in registry.entries() {
            if entry.project_id == primary
                || self.extra_projects.contains_key(&entry.project_id)
            {
                continue;
            }
            let path = PathBuf::from(&entry.config_path);
            if !path.exists() {
                warn!(
                    "Loose manifest '{}' in the registry no longer exists; skipping",
                    entry.config_path
                );
                continue;
            }
            // The entry records what the file was when it was registered. A
            // manifest edited since then may now declare its own project, or
            // several — restoring it on the strength of the stored id would
            // register projects the registry never recorded, or reconcile an
            // unrelated one. Only replay a file that is still exactly the one
            // loose project this entry names.
            if !self.manifest_still_matches(&path, &entry.project_id) {
                warn!(
                    "Loose manifest '{}' no longer declares project '{}'; skipping restore",
                    entry.config_path, entry.project_id
                );
                continue;
            }
            info!(
                "Restoring loose project '{}' from {}",
                entry.project_id, entry.config_path
            );
            if let Err(err) = self.add_project_config(&path, None, entry.mode) {
                error!(
                    "Failed to restore loose project '{}': {err}. Continuing with the rest.",
                    entry.project_id
                );
            }
        }
    }

    /// Whether `path` still parses to exactly one loose project with `project_id`.
    ///
    /// Guards the registry replay against a manifest that changed shape after it
    /// was registered: a file that grew a `project:` key, or fanned out into
    /// several projects, is no longer the thing the entry describes.
    fn manifest_still_matches(&self, path: &Path, project_id: &str) -> bool {
        let trusted = match runtime::open_trusted_config(path) {
            Ok(trusted) => trusted,
            Err(err) => {
                warn!("registry replay skipped {}: {err}", path.display());
                return false;
            }
        };
        let configs = match load_projects_from_file(trusted, path) {
            Ok(configs) => configs,
            Err(err) => {
                warn!("registry replay skipped {}: {err}", path.display());
                return false;
            }
        };
        matches!(
            configs.as_slice(),
            [only] if only.project.loose && only.project.id == project_id
        )
    }

    /// Registers and synchronously starts one additional project from a
    /// multi-project manifest during boot. Unlike the live AddProject path this
    /// does not spawn a background boot thread — boot must be deterministic, and
    /// a failure here is isolated to this project by the caller.
    fn boot_extra_project(
        &mut self,
        config: Config,
        config_path: &Path,
    ) -> Result<(), SupervisorError> {
        let project_id = config.project.id.clone();
        if project_id == self.daemon.config().project.id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("duplicate project id '{project_id}' in multi-project config"),
            )
            .into());
        }
        if self.extra_projects.contains_key(&project_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("duplicate project id '{project_id}' in multi-project config"),
            )
            .into());
        }

        self.op_slot
            .begin(format!("starting project '{project_id}'"));
        Self::register_spawn_limits_for_config(&self.spawn_manager, &config)?;
        // Each project gets its OWN pid/state handles bound to its own store, so
        // one project's services never land in a sibling's pid.xml.
        let mut daemon = Daemon::from_config(config, self.detach_children)?;
        daemon.set_timeouts(self.timeouts.clone());
        daemon.set_pipe_stderr(self.pipe_stderr);
        daemon.set_op_slot(self.op_slot.clone());
        if let Ok(mut projects) = self.boot_projects.write() {
            projects.insert(project_id.clone(), daemon.clone());
        }

        let start_result = Self::start_project_services(
            &daemon,
            daemon.config().as_ref(),
            self.service_filter.as_deref(),
            &self.spawn_manager,
            Some(&self.boot_journal),
        );
        if let Err(err) = start_result {
            if let Ok(mut projects) = self.boot_projects.write() {
                projects.remove(&project_id);
            }
            return Err(err);
        }

        self.extra_projects.insert(
            project_id.clone(),
            ProjectRuntime {
                daemon,
                mode: ProjectRunMode::Daemon,
                config_path: config_path.to_path_buf(),
            },
        );
        self.sync_cron_projects()?;
        info!("Project '{project_id}' booted from multi-project config");
        Ok(())
    }

    /// Spawns the acceptor thread that owns the control socket. Each connection
    /// runs on its own worker so a slow client, a streaming log follow, or a
    /// long-running mutation cannot mute the socket for everyone else.
    fn spawn_acceptor(
        listener: std::os::unix::net::UnixListener,
        read_ctx: ReadContext,
        mutation_tx: mpsc::Sender<MutationRequest>,
    ) -> io::Result<()> {
        thread::Builder::new()
            .name("sysg-control".to_string())
            .spawn(move || {
                let _ = listener.set_nonblocking(false);
                loop {
                    match listener.accept() {
                        Ok((stream, _addr)) => {
                            let read_ctx = read_ctx.clone();
                            let mutation_tx = mutation_tx.clone();
                            if let Err(err) = thread::Builder::new()
                                .name("sysg-request".to_string())
                                .spawn(move || {
                                    Self::serve_connection(stream, read_ctx, mutation_tx);
                                })
                            {
                                error!(
                                    "Failed to start supervisor request worker: {err}"
                                );
                            }
                        }
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(err) => {
                            error!("Supervisor listener error: {err}");
                            thread::sleep(CONTROL_ACCEPT_RETRY_DELAY);
                        }
                    }
                }
            })?;
        Ok(())
    }

    /// Handles a single control connection: authenticates, reads one command, and
    /// dispatches it. Reads answer from the shared cache; mutations serialize
    /// through the owner thread.
    fn serve_connection(
        mut stream: std::os::unix::net::UnixStream,
        read_ctx: ReadContext,
        mutation_tx: mpsc::Sender<MutationRequest>,
    ) {
        if let Err(err) = ipc::authenticate_peer(&stream) {
            warn!("Rejected unauthorized control connection: {err}");
            let _ = ipc::write_response(
                &mut stream,
                &ControlResponse::Error(err.to_string()),
            );
            return;
        }

        let command = match ipc::read_command(&mut stream) {
            Ok(command) => command,
            Err(ipc::ControlError::Io(err))
                if err.kind() == io::ErrorKind::UnexpectedEof =>
            {
                return;
            }
            Err(err) => {
                warn!("Invalid supervisor command: {err}");
                let _ = ipc::write_response(
                    &mut stream,
                    &ControlResponse::Error(err.to_string()),
                );
                return;
            }
        };
        debug!("Supervisor received command: {:?}", command);
        match &command {
            ControlCommand::StopProject { project, .. }
            | ControlCommand::Stop {
                service: None,
                project: Some(project),
                ..
            } => {
                if let Ok(projects) = read_ctx.boot_projects.read()
                    && let Some(daemon) = projects.get(project)
                {
                    daemon.cancel_boot();
                }
            }
            ControlCommand::Shutdown
            | ControlCommand::Stop {
                service: None,
                project: None,
                ..
            } => {
                if let Ok(projects) = read_ctx.boot_projects.read() {
                    for daemon in projects.values() {
                        daemon.cancel_boot();
                    }
                }
            }
            _ => {}
        }
        if matches!(&command, ControlCommand::Shutdown) {
            match ipc::peer_pid(&stream) {
                Ok(pid) => info!("Supervisor shutdown requested by client PID {pid}"),
                Err(err) => {
                    warn!("Supervisor shutdown requested by unknown client: {err}")
                }
            }
        }

        if let ControlCommand::Logs { .. } = command {
            Self::serve_logs(stream, command, &read_ctx);
            return;
        }

        if let ControlCommand::BootStream = command {
            Self::serve_boot_stream(stream, &read_ctx.boot_journal);
            return;
        }

        if let ControlCommand::OpStream { op } = &command {
            // The client subscribes BEFORE sending its mutation, so that it
            // cannot miss the opening frames — which means the journal usually
            // does not exist yet. Waiting for it to appear is the whole point:
            // closing on the first miss made rendering a race that a fast
            // operation lost, silently, leaving no tree at all.
            let deadline = Instant::now() + OP_STREAM_REGISTER_TIMEOUT;
            let journal = loop {
                let found = read_ctx
                    .op_journals
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(op)
                    .cloned();
                match found {
                    Some(journal) => break Some(journal),
                    None if Instant::now() < deadline => {
                        thread::sleep(OP_STREAM_REGISTER_POLL);
                    }
                    // Never registered: rejected before it began, or unknown to
                    // this supervisor. Close so the client stops waiting on a
                    // stream that cannot carry a frame.
                    None => break None,
                }
            };
            if let Some(journal) = journal {
                Self::serve_boot_stream(stream, &journal);
            }
            return;
        }

        if let Some(response) = Self::answer_read(&command, &read_ctx) {
            let _ = ipc::write_response(&mut stream, &response);
            return;
        }

        if read_ctx.upgrading.load(Ordering::Acquire) {
            let response =
                ControlResponse::Diag(Box::new(crate::upgrade::environment_unsafe(
                    "the supervisor is committing another live upgrade",
                )));
            let _ = ipc::write_response(&mut stream, &response);
            return;
        }

        // The journal is registered HERE, at enqueue, not when the event loop
        // dequeues the mutation: a command queued behind a slow one waits
        // longer than the subscriber's registration timeout, and its watching
        // client rendered a bare spinner with no tree at all.
        let queued_op = Self::op_id(&command);
        if let Some(op) = &queued_op {
            read_ctx
                .op_journals
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(op.clone())
                .or_default();
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        let (delivered_tx, delivered_rx) = mpsc::channel();
        let request = MutationRequest {
            command,
            reply: reply_tx,
            delivered: delivered_rx,
        };
        if mutation_tx.send(request).is_err() {
            if let Some(op) = &queued_op {
                remove_and_seal_journal(&read_ctx.op_journals, op);
            }
            let _ = ipc::write_response(
                &mut stream,
                &ControlResponse::Error("supervisor is shutting down".into()),
            );
            return;
        }
        match reply_rx.recv() {
            Ok(response) => {
                let delivered = ipc::write_response(&mut stream, &response).is_ok();
                let _ = delivered_tx.send(delivered);
            }
            Err(_) => {
                if let Some(op) = &queued_op {
                    remove_and_seal_journal(&read_ctx.op_journals, op);
                }
                let delivered = ipc::write_response(
                    &mut stream,
                    &ControlResponse::Error(
                        "supervisor dropped the command before replying".into(),
                    ),
                )
                .is_ok();
                let _ = delivered_tx.send(delivered);
            }
        }
    }

    /// Answers read-only commands directly from shared state, or returns `None`
    /// when the command must go through the single-writer owner thread.
    fn answer_read(
        command: &ControlCommand,
        read_ctx: &ReadContext,
    ) -> Option<ControlResponse> {
        match command {
            ControlCommand::Status { live: false } => {
                let mut snapshot = read_ctx.status_cache.snapshot();
                Self::apply_boots(&mut snapshot, &read_ctx.boots);
                Some(ControlResponse::Status(snapshot))
            }
            ControlCommand::Version => {
                Some(ControlResponse::DaemonVersion(read_ctx.version.clone()))
            }
            ControlCommand::CurrentOp => {
                Some(ControlResponse::CurrentOp(read_ctx.op_slot.report()))
            }
            ControlCommand::Inspect {
                unit,
                project,
                live: false,
                ..
            } => Some(Self::inspect_from_cache(unit, project.as_deref(), read_ctx)),
            _ => None,
        }
    }

    /// Attaches the latest queued boot result to every unit in its project.
    fn apply_boots(
        snapshot: &mut StatusSnapshot,
        boots: &Arc<RwLock<HashMap<String, BootStatus>>>,
    ) {
        let boots = boots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for unit in &mut snapshot.units {
            if let Some(project) = unit.project.as_mut() {
                project.boot = boots.get(&project.id).cloned();
            }
        }
    }

    /// Builds an inspect response from the cached snapshot without touching
    /// mutation state or the metrics store.
    fn inspect_from_cache(
        unit: &str,
        project: Option<&str>,
        read_ctx: &ReadContext,
    ) -> ControlResponse {
        let snapshot = read_ctx.status_cache.snapshot();
        let matching: Vec<_> = snapshot
            .units
            .iter()
            .filter(|status| unit_matches_selector(status, unit, project))
            .cloned()
            .collect();
        if project.is_none() && matching.len() > 1 {
            let projects = matching
                .iter()
                .filter_map(|unit| {
                    unit.project.as_ref().map(|project| project.id.as_str())
                })
                .collect::<BTreeSet<_>>();
            if projects.len() > 1 {
                return ControlResponse::Error(format!(
                    "service '{unit}' exists in multiple projects ({}); pass --project to choose one",
                    projects.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        ControlResponse::Inspect(Box::new(InspectPayload {
            unit: matching.into_iter().next(),
            samples: Vec::new(),
        }))
    }

    /// Streams logs on the connection worker using the cached snapshot for target
    /// resolution, so a wedged mutation never blocks a `logs` request.
    /// Streams boot progress to a subscriber: replays every frame recorded so
    /// far, then follows live frames until the terminal `Done`. Race-free — a
    /// client that connects after boot still receives the whole journal.
    fn serve_boot_stream(
        mut stream: std::os::unix::net::UnixStream,
        journal: &BootJournal,
    ) {
        let mut seen = 0usize;
        loop {
            let batch = journal.wait_from(seen);
            if batch.is_empty() {
                break;
            }
            seen += batch.len();
            let mut done = false;
            for frame in batch {
                done |= frame.is_done();
                let Ok(line) = serde_json::to_string(&frame) else {
                    return;
                };
                if writeln!(stream, "{line}").is_err() {
                    return;
                }
            }
            let _ = stream.flush();
            if done {
                break;
            }
        }
    }

    fn serve_logs(
        mut stream: std::os::unix::net::UnixStream,
        command: ControlCommand,
        read_ctx: &ReadContext,
    ) {
        let ControlCommand::Logs {
            service,
            project,
            lines,
            kind,
            follow,
            since,
            until,
            grep,
            all,
            structured,
        } = command
        else {
            return;
        };

        let filter = match crate::logs::LogFilter::from_parts(
            since.as_deref(),
            until.as_deref(),
            grep.as_deref(),
            all,
            chrono::Utc::now(),
        ) {
            Ok(filter) => filter,
            Err(err) => {
                let _ = writeln!(stream, "{err}");
                return;
            }
        };

        let request = SupervisorLogRequest {
            snapshot: read_ctx.status_cache.snapshot(),
            service,
            project,
            lines,
            kind: kind.as_deref(),
            follow,
            filter,
            structured,
            stream: &stream,
        };
        if let Err(err) = Supervisor::handle_logs_command(request) {
            error!("Supervisor logs command failed: {err}");
            let _ = writeln!(stream, "{err}");
        }
    }

    /// Sets or clears close-on-exec for one supervisor-owned descriptor.
    fn set_descriptor_cloexec(fd: libc::c_int, enabled: bool) -> io::Result<()> {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let next = if enabled {
            flags | libc::FD_CLOEXEC
        } else {
            flags & !libc::FD_CLOEXEC
        };
        if unsafe { libc::fcntl(fd, libc::F_SETFD, next) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Captures one project's parsed manifest and quiesced daemon state.
    fn project_handoff(
        daemon: &Daemon,
        config_path: &Path,
        snapshot: &str,
        mode: ProjectRunMode,
        active: bool,
    ) -> Result<HandoffProject, ProcessManagerError> {
        let state = daemon.handoff_state()?;
        let config = daemon.config();
        for process in &state.processes {
            let Some(service) = config.services.get(&process.service) else {
                return Err(ProcessManagerError::ServiceStartError {
                    service: process.service.clone(),
                    source: io::Error::other(
                        "managed process is absent from the active manifest",
                    ),
                });
            };
            if service.effective_logs(&config.logs).sink == LogSink::File
                && !crate::logs::service_log_handoff_ready(
                    &config.project.id,
                    &process.service,
                )
            {
                return Err(ProcessManagerError::ServiceStartError {
                    service: process.service.clone(),
                    source: io::Error::other(
                        "managed stdout or stderr pipe is unavailable for handoff",
                    ),
                });
            }
        }
        Ok(HandoffProject {
            project_id: config.project.id.clone(),
            config_path: config_path.to_path_buf(),
            config_hash: ipc::manifest_fingerprint(snapshot)?,
            mode,
            active,
            daemon: state,
        })
    }

    /// Reads a manifest through its trust-validated descriptor and assembles
    /// its includes, so snapshotting and hashing operate on one set of bytes.
    fn assembled_manifest(path: &Path) -> Result<String, ProcessManagerError> {
        use std::io::Read;
        let mut file = runtime::open_trusted_config(path)?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;
        crate::config::resolve_includes(&content, path)
    }

    /// Stops monitor threads so process identity and restart bookkeeping cannot
    /// change while a handoff record is being built.
    fn quiesce_project_monitors(&self) {
        self.daemon.shutdown_monitor();
        for project in self.extra_projects.values() {
            project.daemon.shutdown_monitor();
        }
    }

    /// Restarts project monitors after a handoff attempt is cancelled.
    fn resume_project_monitors(&self) {
        if self.primary_active
            && let Err(err) = self.daemon.ensure_monitoring()
        {
            error!("Failed to resume primary monitor after upgrade cancellation: {err}");
        }
        for (project_id, project) in &self.extra_projects {
            if let Err(err) = project.daemon.ensure_monitoring() {
                error!(
                    "Failed to resume monitor for project '{project_id}' after upgrade cancellation: {err}"
                );
            }
        }
    }

    /// Validates runtime suitability, quiesces mutable ownership, and persists a
    /// complete descriptor-backed supervisor handoff.
    fn prepare_upgrade(
        &self,
        binary: &Path,
        runtime_lock: &File,
        listener: &std::os::unix::net::UnixListener,
    ) -> Result<PreparedUpgrade, Box<crate::diag::Diagnostic>> {
        let target = UpgradeTarget::inspect(binary, &LiveUpgradeInfo::current())?;
        if self.pipe_stderr {
            return Err(Box::new(crate::upgrade::environment_unsafe(
                "service stderr is attached to supervisor stdout; restart without `--stderr` before upgrading",
            )));
        }
        let cron_gate = self
            .cron_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .upgrading
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Box::new(crate::upgrade::environment_unsafe(
                "another live upgrade is already committing",
            )));
        }
        if let Some(job) = self
            .cron_manager
            .get_all_jobs()
            .into_iter()
            .find(|job| job.currently_running)
        {
            self.upgrading.store(false, Ordering::Release);
            return Err(Box::new(crate::upgrade::environment_unsafe(format!(
                "cron unit `{}` is currently running",
                job.service_name
            ))));
        }
        let active_children = self.spawn_manager.active_child_count();
        if active_children > 0 {
            self.upgrading.store(false, Ordering::Release);
            return Err(Box::new(crate::upgrade::environment_unsafe(format!(
                "{active_children} dynamic child process(es) are still active"
            ))));
        }
        drop(cron_gate);

        self.quiesce_project_monitors();
        let prepared = (|| {
            let mut manifests = BTreeMap::new();
            for path in std::iter::once(&self.config_path).chain(
                self.extra_projects
                    .values()
                    .map(|project| &project.config_path),
            ) {
                if manifests.contains_key(path) {
                    continue;
                }
                let snapshot = Self::assembled_manifest(path).map_err(|err| {
                    Box::new(crate::upgrade::handoff_failed(format!(
                        "could not snapshot manifest {}: {err}",
                        path.display()
                    )))
                })?;
                manifests.insert(path.clone(), snapshot);
            }
            let primary = Self::project_handoff(
                &self.daemon,
                &self.config_path,
                &manifests[&self.config_path],
                self.primary_project_mode,
                self.primary_active,
            )
            .map_err(|err| {
                Box::new(crate::upgrade::environment_unsafe(err.to_string()))
            })?;
            let mut projects = BTreeMap::new();
            for (project_id, project) in &self.extra_projects {
                let handoff = Self::project_handoff(
                    &project.daemon,
                    &project.config_path,
                    &manifests[&project.config_path],
                    project.mode,
                    true,
                )
                .map_err(|err| {
                    Box::new(crate::upgrade::environment_unsafe(err.to_string()))
                })?;
                projects.insert(project_id.clone(), handoff);
            }
            let log_pipes = crate::logs::prepare_log_pipe_handoff().map_err(|err| {
                Box::new(crate::upgrade::handoff_failed(err.to_string()))
            })?;
            Self::set_descriptor_cloexec(runtime_lock.as_raw_fd(), false).map_err(
                |err| Box::new(crate::upgrade::handoff_failed(err.to_string())),
            )?;
            Self::set_descriptor_cloexec(listener.as_raw_fd(), false).map_err(|err| {
                Box::new(crate::upgrade::handoff_failed(err.to_string()))
            })?;
            let source_binary = std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .map_err(|err| {
                Box::new(crate::upgrade::handoff_failed(format!(
                    "could not resolve the resident binary for rollback: {err}"
                )))
            })?;
            let state = SupervisorHandoff {
                schema: HANDOFF_SCHEMA_VERSION,
                protocol: LIVE_REEXEC_PROTOCOL,
                source_binary,
                source_version: LiveUpgradeInfo::current().version,
                target_version: target.info.version.clone(),
                rollback_reason: None,
                lock_fd: runtime_lock.as_raw_fd(),
                listener_fd: listener.as_raw_fd(),
                service_filter: self.service_filter.clone(),
                pipe_stderr: self.pipe_stderr,
                primary,
                projects,
                log_pipes,
                manifests,
            };
            let path = state.persist().map_err(|err| {
                Box::new(crate::upgrade::handoff_failed(err.to_string()))
            })?;
            Ok(PreparedUpgrade {
                target,
                path,
                config: self.config_path.clone(),
            })
        })();
        if prepared.is_err() {
            crate::logs::cancel_log_pipe_handoff();
            let _ = Self::set_descriptor_cloexec(runtime_lock.as_raw_fd(), true);
            let _ = Self::set_descriptor_cloexec(listener.as_raw_fd(), true);
            self.resume_project_monitors();
            self.upgrading.store(false, Ordering::Release);
        }
        prepared
    }

    /// Executes the validated replacement binary in the current supervisor PID.
    fn execute_upgrade(prepared: &PreparedUpgrade) -> io::Result<()> {
        let mut values = vec![prepared.target.path.to_string_lossy().to_string()];
        if crate::runtime::mode() == crate::runtime::RuntimeMode::System {
            values.push("--sys".to_string());
        }
        values.extend([
            "supervise".to_string(),
            "--config".to_string(),
            prepared.config.to_string_lossy().to_string(),
            "--handoff".to_string(),
            prepared.path.to_string_lossy().to_string(),
        ]);
        let args = values
            .iter()
            .map(|value| {
                CString::new(value.as_str()).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "upgrade argument contains a NUL byte",
                    )
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        nix::unistd::execv(&args[0], &args)
            .map(|_| ())
            .map_err(io::Error::other)
    }

    /// Restores the resident image after its final `exec` call failed.
    fn recover_upgrade(
        &self,
        prepared: &PreparedUpgrade,
        runtime_lock: &File,
        listener: &std::os::unix::net::UnixListener,
    ) {
        if let Err(err) = std::fs::remove_file(&prepared.path)
            && err.kind() != io::ErrorKind::NotFound
        {
            warn!(
                "Failed to remove cancelled handoff {:?}: {err}",
                prepared.path
            );
        }
        crate::logs::cancel_log_pipe_handoff();
        let _ = Self::set_descriptor_cloexec(runtime_lock.as_raw_fd(), true);
        let _ = Self::set_descriptor_cloexec(listener.as_raw_fd(), true);
        self.resume_project_monitors();
        self.upgrading.store(false, Ordering::Release);
    }

    /// Applies one lifecycle timeout policy to every managed project daemon.
    fn apply_timeouts(&mut self, timeouts: SupervisorTimeouts) {
        self.daemon.set_timeouts(timeouts.clone());
        for project in self.extra_projects.values() {
            project.daemon.set_timeouts(timeouts.clone());
        }
        self.timeouts = timeouts;
    }

    /// Runs the supervisor event loop.
    fn run_internal(&mut self) -> Result<(), SupervisorError> {
        let loaded = self.handoff.take();
        let resumed = loaded.is_some();
        let (runtime_lock, listener, handoff_path) = match loaded {
            Some(LoadedHandoff { path, state }) => {
                if let Some(reason) = &state.rollback_reason {
                    error!(
                        "Replacement supervisor failed; resumed {} after rollback: {reason}",
                        state.source_version
                    );
                }
                let lock = unsafe { File::from_raw_fd(state.lock_fd) };
                let listener = unsafe {
                    std::os::unix::net::UnixListener::from_raw_fd(state.listener_fd)
                };
                Self::set_descriptor_cloexec(lock.as_raw_fd(), true)?;
                Self::set_descriptor_cloexec(listener.as_raw_fd(), true)?;
                (lock, listener, Some(path))
            }
            None => {
                let lock = ipc::lock_supervisor_runtime()?;
                ipc::cleanup_runtime()?;
                let listener = ipc::bind_control_socket()?;
                (lock, listener, None)
            }
        };

        // Load (or create with defaults) the supervisor's OWN config — distinct
        // from any project manifest — and apply its log-rotation defaults as the
        // process-wide fallback beneath per-service/per-project `logs` blocks,
        // before any service launches and opens its log files.
        let supervisor_config =
            crate::config::supervisor::SupervisorConfig::load_or_create();
        self.apply_timeouts(supervisor_config.timeouts.clone());
        crate::config::set_log_defaults(
            supervisor_config.logs.max_bytes,
            supervisor_config.logs.max_files,
        );

        ipc::write_config_hint(&self.config_path)?;
        ipc::write_supervisor_pid(unsafe { libc::getpid() })?;

        match self.collect_aggregate_snapshot(false) {
            Ok(snapshot) => self.status_cache.replace(snapshot),
            Err(err) => error!("failed to build pre-boot status snapshot: {err}"),
        }

        let (mutation_tx, mutation_rx) = mpsc::channel::<MutationRequest>();
        let read_ctx = ReadContext {
            status_cache: self.status_cache.clone(),
            op_slot: self.op_slot.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            boot_journal: self.boot_journal.clone(),
            op_journals: Arc::clone(&self.op_journals),
            boot_projects: Arc::clone(&self.boot_projects),
            boots: Arc::clone(&self.boots),
            upgrading: Arc::clone(&self.upgrading),
        };
        Self::spawn_acceptor(listener.try_clone()?, read_ctx, mutation_tx)?;

        if let Ok(socket_path) = ipc::socket_path() {
            info!("systemg supervisor listening on {:?}", socket_path);
        }

        let (started, failed) = if resumed {
            if self.primary_active {
                self.daemon.ensure_monitoring()?;
            }
            for project in self.extra_projects.values() {
                project.daemon.ensure_monitoring()?;
            }
            (0, 0)
        } else {
            self.boot_primary_services()?;
            self.daemon.ensure_monitoring()?;
            self.boot_tally()
        };

        let config_handle = self.daemon.config();
        let pid_handle = self.daemon.pid_file_handle();
        let state_handle = self.daemon.service_state_handle();

        // Seed the cache from ALL managed projects, not just the primary, so a
        // multi-project boot (which registers extra projects before this point)
        // is reflected in status from the first read.
        //
        // This MUST precede `BootFrame::Done`. `Done` is what releases a waiting
        // `sysg start` to exit 0, so publishing after it lets the CLI return
        // while `status` still serves the pre-boot snapshot — every unit reading
        // as stopped with no pid while its process is already running.
        //
        // A failure here is reported, never fatal: a resumed supervisor has
        // already adopted the running services, so aborting would strand them.
        // But it must not be silent either — announcing `Done` over an unwritten
        // cache is exactly the false success this ordering exists to prevent, so
        // the boot carries the failure out to the caller and supervision lives.
        let (started, failed) = match self.publish_boot_snapshot() {
            Ok(()) => (started, failed),
            Err(err) => {
                error!("failed to publish post-boot status snapshot: {err}");
                self.boot_journal.push(BootFrame::Unit {
                    project: self.daemon.config().project.id.clone(),
                    service: "status".to_string(),
                    outcome: crate::start::Outcome::Failed(
                        crate::status::diagnostics::snapshot_unavailable(err.to_string()),
                    ),
                });
                (started, failed + 1)
            }
        };

        self.boot_journal.push(BootFrame::Done { started, failed });

        let cache_clone = self.status_cache.clone();
        let refresh_interval = Self::status_snapshot_interval(config_handle.as_ref());
        let refresh_mode = Self::status_snapshot_mode(config_handle.as_ref());
        let refresh_projects = Arc::clone(&self.cron_projects);
        let refresh_metrics = self.metrics_store.clone();
        let refresh_spawn = self.spawn_manager.clone();
        if !matches!(refresh_mode, StatusSnapshotMode::Off) {
            self.status_refresher = Some(StatusRefresher::spawn(
                cache_clone,
                refresh_interval,
                move || {
                    Supervisor::collect_projects_snapshot(
                        &refresh_projects,
                        &refresh_metrics,
                        &refresh_spawn,
                        refresh_mode,
                    )
                },
            )?);
        }

        let metrics_handle = self.metrics_store.clone();
        self.metrics_collector = Some(MetricsCollector::spawn(
            metrics_handle,
            Arc::clone(&config_handle),
            pid_handle,
            state_handle,
        )?);

        let cron_manager = self.cron_manager.clone();
        let cron_projects = Arc::clone(&self.cron_projects);
        let metrics_store = self.metrics_store.clone();
        let upgrading = Arc::clone(&self.upgrading);
        let cron_gate = Arc::clone(&self.cron_gate);

        thread::Builder::new()
            .name("sysg-cron".to_string())
            .spawn(move || loop {
                thread::sleep(CRON_TICK_INTERVAL);
                let _gate = cron_gate
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if upgrading.load(Ordering::Acquire) {
                    continue;
                }

                let due_jobs = cron_manager.get_due_job_refs();
                let overlaps = cron_manager.take_overlaps();
                if !due_jobs.is_empty() || !overlaps.is_empty() {
                    let projects = match cron_projects.read() {
                        Ok(projects) => projects.clone(),
                        Err(err) => {
                            error!("Failed to read cron project routing: {}", err);
                            Vec::new()
                        }
                    };

                    for overlap in overlaps {
                        let Some(project) = projects.iter().find(|project| {
                            project.config.state_key(&overlap.service_name)
                                == overlap.service_hash
                        }) else {
                            continue;
                        };
                        if let Some(service_config) =
                            project.config.services.get(&overlap.service_name)
                        {
                            notify_cron_failure(
                                &project.daemon,
                                &overlap.service_name,
                                service_config,
                                &CronExecutionStatus::OverlapError,
                            );
                        }
                    }

                    for due_job in due_jobs {
                        let project = projects.iter().find(|project| {
                            project.config.services.contains_key(&due_job.service_name)
                                && project.config.state_key(&due_job.service_name)
                                    == due_job.service_hash
                        });

                        let Some(project) = project else {
                            if !cron_manager.contains_job_hash(&due_job.service_hash) {
                                // The job went away between the claim and here.
                                // Its ownership must go with it, or re-adding
                                // the same service finds itself permanently
                                // claimed and never runs again.
                                cron_manager.abandon_job_run(&due_job);
                                continue;
                            }
                            error!(
                                "Failed to resolve cron job '{}' ({}) to a managed project",
                                due_job.service_name, due_job.service_hash
                            );
                            cron_manager.complete_job_run(
                                &due_job.service_hash,
                                due_job.started_at,
                                CronExecutionStatus::Failed(
                                    "Cron job project is not managed".to_string(),
                                ),
                                None,
                                vec![],
                            );
                            continue;
                        };

                        if project.daemon.boot_cancelled()
                            || !cron_manager.contains_job_hash(&due_job.service_hash)
                        {
                            cron_manager.abandon_job_run(&due_job);
                            continue;
                        }

                        if let Some(service_config) =
                            project.config.services.get(&due_job.service_name).cloned()
                        {
                            info!(
                                "Running cron job '{}' in project '{}'",
                                due_job.service_name, project.project_id
                            );
                            let command = Some(service_config.command.clone());
                            let user = fallback_cron_user(&service_config);
                            let cron_manager_clone = cron_manager.clone();
                            let job_name_clone = due_job.service_name.clone();
                            let project_id_clone = project.project_id.clone();
                            let daemon = project.daemon.clone();
                            let metrics_store_clone = metrics_store.clone();
                            let service_hash = due_job.service_hash.clone();
                            let run_started_at = due_job.started_at;
                            let withdraw_claim = due_job.clone();

                            let failed_manager = cron_manager_clone.clone();
                            let failed_hash = service_hash.clone();
                            if let Err(err) = thread::Builder::new()
                                .name(format!("sysg-cron-{job_name_clone}"))
                                .spawn(move || {
                                let _completion_claim =
                                    daemon.claim_completion(&job_name_clone);
                                // Anything still addressed to this unit belongs
                                // to a run nobody is waiting on; clearing it
                                // before the spawn keeps it from being read as
                                // this run's outcome.
                                crate::reaper::drop_claims(&service_hash);
                                match daemon
                                    .start_service(&job_name_clone, &service_config)
                                {
                                    Ok(ServiceReadyState::Skipped) => {
                                        info!(
                                            "Cron job '{}' was skipped; recording no execution",
                                            job_name_clone
                                        );
                                        cron_manager_clone
                                            .withdraw_job_run(&withdraw_claim);
                                    }
                                    Ok(ServiceReadyState::CompletedSuccess) => {
                                        cron_manager_clone.annotate_job_run(
                                            &service_hash,
                                            run_started_at,
                                            None,
                                            user.clone(),
                                            command.clone(),
                                        );
                                        info!(
                                            "Cron job '{}' completed successfully",
                                            job_name_clone
                                        );

                                        let metrics = cron_run_metrics(
                                            &metrics_store_clone,
                                            &service_hash,
                                            run_started_at,
                                        );
                                        persist_cron_state(
                                            &daemon,
                                            &service_hash,
                                            &job_name_clone,
                                            ServiceLifecycleStatus::ExitedSuccessfully,
                                            Some(0),
                                        );
                                        cron_manager_clone.complete_job_run(
                                            &service_hash,
                                            run_started_at,
                                            CronExecutionStatus::Success,
                                            Some(0),
                                            metrics,
                                        );
                                    }
                                    Ok(ServiceReadyState::Running) => {
                                        let pid = daemon
                                            .pid_file_handle()
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                                            .pid_for(&job_name_clone);
                                        if let Some(pid) = pid {
                                                    cron_manager_clone
                                                        .annotate_job_run(
                                                            &service_hash,
                                                            run_started_at,
                                                            Some(pid),
                                                            user.clone(),
                                                            command.clone(),
                                                        );
                                                    let result =
                                                        Self::wait_for_cron_completion(
                                                            pid,
                                                            &job_name_clone,
                                                            &service_hash,
                                                        );

                                                    match result {
                                                        Ok(outcome) => {
                                                            let CronCompletionOutcome {
                                                                status,
                                                                exit_code,
                                                            } = outcome;

                                                            match &status {
                                                                            CronExecutionStatus::Success => info!(
                                                                                "Cron job '{}' completed successfully",
                                                                                job_name_clone
                                                                            ),
                                                                            CronExecutionStatus::Failed(reason) => warn!(
                                                                                "Cron job '{}' failed: {}",
                                                                                job_name_clone, reason
                                                                            ),
                                                                            CronExecutionStatus::Interrupted(reason) => warn!(
                                                                                "Cron job '{}' was interrupted: {}",
                                                                                job_name_clone, reason
                                                                            ),
                                                                            CronExecutionStatus::OverlapError => warn!(
                                                                                "Cron job '{}' reported overlap state unexpectedly",
                                                                                job_name_clone
                                                                            ),
                                                                        }

                                                            let metrics = cron_run_metrics(
                                                                &metrics_store_clone,
                                                                &service_hash,
                                                                run_started_at,
                                                            );
                                                            let lifecycle_status = match status {
                                                                CronExecutionStatus::Success => ServiceLifecycleStatus::ExitedSuccessfully,
                                                                CronExecutionStatus::Failed(_) | CronExecutionStatus::OverlapError => ServiceLifecycleStatus::ExitedWithError,
                                                                CronExecutionStatus::Interrupted(_) => ServiceLifecycleStatus::Stopped,
                                                            };
                                                            persist_cron_state(
                                                                &daemon,
                                                                &service_hash,
                                                                &job_name_clone,
                                                                lifecycle_status,
                                                                exit_code,
                                                            );
                                                            clear_cron_pid(
                                                                &daemon,
                                                                &job_name_clone,
                                                                pid,
                                                            );
                                                            notify_cron_failure(
                                                                &daemon,
                                                                &job_name_clone,
                                                                &service_config,
                                                                &status,
                                                            );
                                                            cron_manager_clone.complete_job_run(
                                                                &service_hash,
                                                                run_started_at,
                                                                status,
                                                                exit_code,
                                                                metrics,
                                                            );
                                                        }
                                                        Err(e) => {
                                                            error!(
                                                                "Error waiting for cron job '{}': {}",
                                                                job_name_clone, e
                                                            );
                                                            let metrics = cron_run_metrics(
                                                                &metrics_store_clone,
                                                                &service_hash,
                                                                run_started_at,
                                                            );
                                                            persist_cron_state(
                                                                &daemon,
                                                                &service_hash,
                                                                &job_name_clone,
                                                                ServiceLifecycleStatus::ExitedWithError,
                                                                None,
                                                            );
                                                            clear_cron_pid(
                                                                &daemon,
                                                                &job_name_clone,
                                                                pid,
                                                            );
                                                            let status =
                                                                CronExecutionStatus::Failed(
                                                                    e.to_string(),
                                                                );
                                                            notify_cron_failure(
                                                                &daemon,
                                                                &job_name_clone,
                                                                &service_config,
                                                                &status,
                                                            );
                                                            cron_manager_clone.complete_job_run(
                                                                &service_hash,
                                                                run_started_at,
                                                                status,
                                                                None,
                                                                metrics,
                                                            );
                                                        }
                                                    }
                                                } else {
                                                    let already_completed = if let Ok(
                                                        state_file,
                                                    ) =
                                                        ServiceStateFile::load(
                                                            daemon.store(),
                                                        )
                                                        && let Some(entry) =
                                                            state_file.get(&service_hash)
                                                    {
                                                        matches!(entry.status, ServiceLifecycleStatus::ExitedSuccessfully)
                                                                        || (entry.status == ServiceLifecycleStatus::ExitedWithError && entry.exit_code == Some(0))
                                                    } else {
                                                        false
                                                    };

                                                    if already_completed {
                                                        debug!(
                                                            "Cron job '{}' already completed before PID tracking",
                                                            job_name_clone
                                                        );
                                                        cron_manager_clone
                                                                        .annotate_job_run(
                                                                            &service_hash,
                                                                            run_started_at,
                                                                            None,
                                                                            user.clone(),
                                                                            command.clone(),
                                                                        );
                                                        let metrics = cron_run_metrics(
                                                            &metrics_store_clone,
                                                            &service_hash,
                                                            run_started_at,
                                                        );

                                                        cron_manager_clone
                                                            .complete_job_run(
                                                            &service_hash,
                                                            run_started_at,
                                                            CronExecutionStatus::Success,
                                                            Some(0),
                                                            metrics,
                                                        );
                                                    } else {
                                                        error!(
                                                            "Failed to find PID for cron job '{}' in project '{}' and job has not completed",
                                                            job_name_clone,
                                                            project_id_clone
                                                        );
                                                        persist_cron_state(
                                                            &daemon,
                                                            &service_hash,
                                                            &job_name_clone,
                                                            ServiceLifecycleStatus::ExitedWithError,
                                                            None,
                                                        );
                                                        let status =
                                                            CronExecutionStatus::Failed(
                                                                "Failed to get PID from PID file"
                                                                    .to_string(),
                                                            );
                                                        notify_cron_failure(
                                                            &daemon,
                                                            &job_name_clone,
                                                            &service_config,
                                                            &status,
                                                        );
                                                        cron_manager_clone.complete_job_run(
                                                            &service_hash,
                                                            run_started_at,
                                                            status,
                                                            None,
                                                            vec![],
                                                        );
                                                    }
                                        }
                                    }
                                    Err(e) => {
                                        error!(
                                            "Failed to start cron job '{}' in project '{}': {}",
                                            job_name_clone, project_id_clone, e
                                        );
                                        cron_manager_clone
                                            .annotate_job_run(
                                                &service_hash,
                                                run_started_at,
                                                None,
                                                user.clone(),
                                                command.clone(),
                                            );
                                        let status =
                                            CronExecutionStatus::Failed(e.to_string());
                                        notify_cron_failure(
                                            &daemon,
                                            &job_name_clone,
                                            &service_config,
                                            &status,
                                        );
                                        cron_manager_clone.complete_job_run(
                                            &service_hash,
                                            run_started_at,
                                            status,
                                            None,
                                            vec![],
                                        );
                                    }
                                }
                                })
                            {
                                error!("Failed to start cron worker: {err}");
                                let status = CronExecutionStatus::Failed(format!(
                                    "Failed to start cron worker: {err}"
                                ));
                                if let Some(service_config) =
                                    project.config.services.get(&due_job.service_name)
                                {
                                    notify_cron_failure(
                                        &project.daemon,
                                        &due_job.service_name,
                                        service_config,
                                        &status,
                                    );
                                }
                                failed_manager.complete_job_run(
                                    &failed_hash,
                                    run_started_at,
                                    status,
                                    None,
                                    vec![],
                                );
                            }
                        }
                    }
                }
            })?;

        if let Some(path) = handoff_path
            && let Err(err) = std::fs::remove_file(&path)
            && err.kind() != io::ErrorKind::NotFound
        {
            warn!("Failed to remove completed supervisor handoff {path:?}: {err}");
        }

        loop {
            let request = match mutation_rx.recv() {
                Ok(request) => request,
                Err(err) => {
                    error!("Supervisor control plane ended unexpectedly: {err}");
                    break;
                }
            };
            let MutationRequest {
                command,
                reply,
                delivered,
            } = request;
            if let ControlCommand::Upgrade { binary } = command {
                if crate::runtime::init_mode() {
                    let _ = reply.send(ControlResponse::Error(
                        "SG0714: live upgrade is forbidden in container-init mode; a failed exec as PID 1 kills the container. Upgrade the image instead."
                            .to_string(),
                    ));
                    continue;
                }
                let _op = self.op_slot.guard("upgrading supervisor");
                match self.prepare_upgrade(Path::new(&binary), &runtime_lock, &listener) {
                    Ok(prepared) => {
                        let version = prepared.target.info.version.to_string();
                        let _ = reply.send(ControlResponse::UpgradeAccepted { version });
                        let accepted = delivered
                            .recv_timeout(UPGRADE_ACCEPT_TIMEOUT)
                            .unwrap_or(false);
                        if accepted {
                            if let Err(err) = Self::execute_upgrade(&prepared) {
                                error!(
                                    "Failed to execute live supervisor upgrade: {err}"
                                );
                                self.recover_upgrade(&prepared, &runtime_lock, &listener);
                            }
                        } else {
                            warn!(
                                "Upgrade client disconnected before acceptance was delivered"
                            );
                            self.recover_upgrade(&prepared, &runtime_lock, &listener);
                        }
                    }
                    Err(diag) => {
                        let _ = reply.send(ControlResponse::Diag(diag));
                    }
                }
                continue;
            }
            let should_shutdown = matches!(command, ControlCommand::Shutdown);
            let owns_slot = !matches!(command, ControlCommand::AddProject { .. });
            let _op = owns_slot.then(|| {
                let label = Self::mutation_label(&command);
                match Self::mutation_parts(&command) {
                    Some(parts) => self.op_slot.guard_parts(label, parts),
                    None => self.op_slot.guard(label),
                }
            });
            // Every watched mutation gets its own journal: the boot journal
            // seals on its terminal frame, so reusing it would silently discard
            // the progress of everything after the first boot.
            let _watch = Self::op_id(&command).map(|op| {
                // Reuses the journal the connection thread registered at
                // enqueue; subscribers may already be streaming it, so a fresh
                // one here would strand them on an object nobody writes to.
                let journal = self
                    .op_journals
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entry(op.clone())
                    .or_default()
                    .clone();
                // Published for the length of the command so a handler that
                // spawns work outliving it — `AddProject` queues its boots onto
                // their own threads — can attach those daemons to this journal
                // and hold a lease keeping the stream open until they finish.
                self.active_op = Some((op.clone(), journal.clone()));
                // The TARGET's daemon is what emits the frames, and it is not
                // always the primary: watching only the primary left
                // extra-project operations rendering a head line with an empty
                // tree under it.
                //
                // A daemon broadcasts each frame to everything watching IT, so
                // attaching to more daemons than the command touches is how one
                // tree ends up showing another's units. Attach as narrowly as
                // the command allows.
                let daemons = match &command {
                    // Its projects do not exist yet — it creates them, and
                    // attaches each as it goes. Attaching to whichever daemons
                    // happen to exist now would only capture work this command
                    // is not doing.
                    ControlCommand::AddProject { .. } => Vec::new(),
                    _ => match Self::op_project(&command) {
                        Some(target) => self
                            .extra_projects
                            .get(target)
                            .map(|project| {
                                vec![project.daemon.watch(&op, journal.clone())]
                            })
                            .unwrap_or_else(|| {
                                vec![self.daemon.watch(&op, journal.clone())]
                            }),
                        // An unscoped op legitimately spans every project.
                        None => {
                            let mut all = vec![self.daemon.watch(&op, journal.clone())];
                            all.extend(self.extra_projects.values().map(|project| {
                                project.daemon.watch(&op, journal.clone())
                            }));
                            all
                        }
                    },
                };
                Arc::new(OpWatch {
                    op,
                    journals: Arc::clone(&self.op_journals),
                    _daemons: daemons,
                })
            });
            self.op_lease = _watch.clone();
            let response = match self.handle_command(command) {
                Ok(response) => response,
                Err(err) => {
                    error!("Supervisor command failed: {err}");
                    error_response(&err)
                }
            };
            // The handler has taken its own lease if it spawned work that
            // outlives this command; dropping the supervisor's copy here keeps
            // an unspawned operation sealing on return as it always did.
            self.op_lease = None;
            self.active_op = None;
            let _ = reply.send(response);
            if should_shutdown {
                info!("Supervisor shutdown request completed; ending event loop");
                break;
            }
        }

        info!("Supervisor event loop ended; stopping managed projects");
        self.shutdown_runtime()?;
        Ok(())
    }

    /// Short label describing a mutation, shown by `sysg` when the supervisor is
    /// busy so a slow command names itself instead of spinning opaquely.
    fn mutation_label(command: &ControlCommand) -> String {
        match command {
            ControlCommand::Start {
                service, project, ..
            } => Self::target_label("starting", service.as_deref(), project.as_deref()),
            ControlCommand::Stop {
                service, project, ..
            } => Self::target_label("stopping", service.as_deref(), project.as_deref()),
            ControlCommand::Restart {
                service, project, ..
            } => Self::target_label("restarting", service.as_deref(), project.as_deref()),
            ControlCommand::StopProject { project, .. } => {
                format!("stopping project '{project}'")
            }
            ControlCommand::Spawn { name, .. } => format!("spawning '{name}'"),
            ControlCommand::Upgrade { .. } => "upgrading supervisor".to_string(),
            ControlCommand::Shutdown => "shutting down".to_string(),
            other => format!("{other:?}"),
        }
    }

    /// Builds a "<verb> <service|all services>[ in project '<p>']" label.
    fn target_label(verb: &str, service: Option<&str>, project: Option<&str>) -> String {
        let subject = match service {
            Some(service) => format!("'{service}'"),
            None => "all services".to_string(),
        };
        match project {
            Some(project) => format!("{verb} {subject} in project '{project}'"),
            None => format!("{verb} {subject}"),
        }
    }

    /// Structured form of [`Self::mutation_label`], letting the CLI nest the
    /// operation instead of printing one long prose line.
    fn mutation_parts(command: &ControlCommand) -> Option<OpParts> {
        let (verb, service, project) = match command {
            ControlCommand::Start {
                service, project, ..
            } => ("starting", service.as_deref(), project.as_deref()),
            ControlCommand::Stop {
                service, project, ..
            } => ("stopping", service.as_deref(), project.as_deref()),
            ControlCommand::Restart {
                service, project, ..
            } => ("restarting", service.as_deref(), project.as_deref()),
            ControlCommand::StopProject { project, .. } => {
                ("stopping", None, Some(project.as_str()))
            }
            _ => return None,
        };
        Some(Self::target_parts(verb, service, project))
    }

    /// The journal key for a command a client may watch.
    ///
    /// The client mints this id and carries it on the mutation, so the
    /// supervisor registers exactly the journal the client already subscribed
    /// to. Deriving it from the target instead would cross-wire two identical
    /// concurrent commands and could not tell a re-run from the one still in
    /// flight. A command with no nonce is unwatched.
    fn op_id(command: &ControlCommand) -> Option<String> {
        match command {
            ControlCommand::Restart { watch, .. }
            | ControlCommand::Start { watch, .. }
            | ControlCommand::Stop { watch, .. }
            | ControlCommand::StopProject { watch, .. }
            | ControlCommand::AddProject { watch, .. } => watch.clone(),
            _ => None,
        }
    }

    /// The project a watched command targets, when it names one.
    ///
    /// Scopes the journal to the daemon that will actually run the work, so a
    /// project booting in the background — `AddProject` hands its boot to a
    /// `sysg-boot-*` thread that emits through its own daemon — cannot leak its
    /// units into an unrelated operation's tree.
    ///
    /// A `project/service` selector names its project too, so it is read from
    /// the service field when the explicit one is absent.
    fn op_project(command: &ControlCommand) -> Option<&str> {
        let (service, project) = match command {
            ControlCommand::Restart {
                service, project, ..
            }
            | ControlCommand::Start {
                service, project, ..
            }
            | ControlCommand::Stop {
                service, project, ..
            } => (service.as_deref(), project.as_deref()),
            ControlCommand::StopProject { project, .. } => (None, Some(project.as_str())),
            _ => return None,
        };
        project
            .or_else(|| service.and_then(|s| split_project_selector(s).map(|(p, _)| p)))
    }

    /// Splits a target into head line and nested unit: the service owns the
    /// head line when one is named — a targeted restart must never read as a
    /// project-wide one — and the project heads the line only for project-wide
    /// operations.
    fn target_parts(verb: &str, service: Option<&str>, project: Option<&str>) -> OpParts {
        match (project, service) {
            (Some(project), Some(service)) => OpParts {
                verb: verb.to_string(),
                target: service.to_string(),
                unit: None,
                project: Some(project.to_string()),
                service: Some(service.to_string()),
            },
            (Some(project), None) => OpParts {
                verb: verb.to_string(),
                target: project.to_string(),
                unit: None,
                project: Some(project.to_string()),
                service: None,
            },
            (None, Some(service)) => OpParts {
                verb: verb.to_string(),
                target: service.to_string(),
                unit: None,
                project: None,
                service: Some(service.to_string()),
            },
            (None, None) => OpParts {
                verb: verb.to_string(),
                target: "all services".to_string(),
                unit: None,
                project: None,
                service: None,
            },
        }
    }

    /// Streams logs through the supervisor-owned control socket.
    fn handle_logs_command(
        request: SupervisorLogRequest<'_>,
    ) -> Result<(), SupervisorError> {
        let manager = LogManager::new();
        let requested_kind = request.kind;

        if let Some(service_name) = request.service {
            crate::logs::validate_service_name(&service_name)
                .map_err(SupervisorError::from)?;
            let matching_units: Vec<_> = request
                .snapshot
                .units
                .iter()
                .filter(|unit| {
                    unit_matches_selector(unit, &service_name, request.project.as_deref())
                })
                .collect();

            if request.project.is_none() && matching_units.len() > 1 {
                let projects = matching_units
                    .iter()
                    .filter_map(|unit| {
                        unit.project.as_ref().map(|project| project.id.as_str())
                    })
                    .collect::<BTreeSet<_>>();
                if projects.len() > 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "service '{service_name}' exists in multiple projects ({}); pass --project to choose one",
                            projects.into_iter().collect::<Vec<_>>().join(", ")
                        ),
                    )
                    .into());
                }
            }

            if let Some(unit) = matching_units.first() {
                let pid = unit.process.as_ref().and_then(|process| {
                    if matches!(process.state, crate::status::ProcessState::Running) {
                        Some(process.pid)
                    } else {
                        None
                    }
                });
                let unit_project = unit
                    .project
                    .as_ref()
                    .map(|project| project.id.clone())
                    .unwrap_or_else(|| crate::state_store::LOOSE_PROJECT_ID.to_string());
                return manager
                    .stream_log_to_socket(
                        &unit_project,
                        &unit.name,
                        pid,
                        request.lines,
                        requested_kind,
                        request.follow,
                        false,
                        &request.filter,
                        request.stream,
                    )
                    .map_err(SupervisorError::from);
            }

            if request.project.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Service '{service_name}' not found in requested project"),
                )
                .into());
            }

            let loose = crate::state_store::LOOSE_PROJECT_ID;
            let combined_exists = get_service_log_path(loose, &service_name).exists();
            let stdout_exists = resolve_log_path(loose, &service_name, "stdout").exists();
            let stderr_exists = resolve_log_path(loose, &service_name, "stderr").exists();
            if combined_exists || stdout_exists || stderr_exists {
                return manager
                    .stream_log_to_socket(
                        loose,
                        &service_name,
                        None,
                        request.lines,
                        requested_kind,
                        request.follow,
                        false,
                        &request.filter,
                        request.stream,
                    )
                    .map_err(SupervisorError::from);
            }

            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Service '{service_name}' not found"),
            )
            .into());
        }

        let project_groups =
            log_project_groups(&request.snapshot, request.project.as_deref());
        if request.follow {
            let mut projects: Vec<String> = request.project.clone().into_iter().collect();
            let mut backlog_services = std::collections::HashSet::new();
            for (_, units) in &project_groups {
                for unit in units {
                    let running = unit.process.as_ref().is_some_and(|process| {
                        process.state == crate::status::ProcessState::Running
                    });
                    let project = unit
                        .project
                        .as_ref()
                        .map(|project| project.id.clone())
                        .unwrap_or_else(|| {
                            crate::state_store::LOOSE_PROJECT_ID.to_string()
                        });
                    if !projects.contains(&project) {
                        projects.push(project.clone());
                    }
                    if running || request.lines > 0 {
                        backlog_services.insert((project, unit.name.clone()));
                    }
                }
            }
            projects.sort_unstable();
            projects.dedup();
            if projects.is_empty() {
                return Ok(());
            }
            return manager
                .stream_project_logs_to_socket(
                    &projects,
                    &backlog_services,
                    request.lines,
                    requested_kind,
                    true,
                    &request.filter,
                    request.stream,
                    request.structured,
                )
                .map_err(SupervisorError::from);
        }
        let render_project_groups = project_groups.len() > 1
            || project_groups
                .iter()
                .any(|(label, _)| label.as_str() != "Ungrouped");
        let show_headers = project_groups
            .iter()
            .flat_map(|(_, units)| units.iter())
            .filter(|unit| !matches!(unit.kind, crate::status::UnitKind::Orphaned))
            .count()
            > 1;

        for (group_index, (project_label, group_units)) in
            project_groups.into_iter().enumerate()
        {
            let mut running_units = Vec::new();
            let mut offline_units = Vec::new();

            if render_project_groups {
                if group_index > 0 {
                    writeln!(request.stream.try_clone()?)?;
                }
                writeln!(request.stream.try_clone()?, "Project: {project_label}")?;
            }

            for unit in group_units
                .iter()
                .filter(|unit| !matches!(unit.kind, crate::status::UnitKind::Orphaned))
            {
                let pid = unit.process.as_ref().and_then(|process| {
                    if matches!(process.state, crate::status::ProcessState::Running) {
                        Some(process.pid)
                    } else {
                        None
                    }
                });

                let unit_project = unit
                    .project
                    .as_ref()
                    .map(|project| project.id.clone())
                    .unwrap_or_else(|| crate::state_store::LOOSE_PROJECT_ID.to_string());

                if pid.is_some() {
                    running_units.push((unit_project, unit.name.clone(), pid));
                } else {
                    offline_units.push((unit_project, unit.name.clone(), pid));
                }
            }

            running_units.sort_unstable_by(|left, right| left.1.cmp(&right.1));
            running_units.dedup_by(|left, right| left.1 == right.1);
            offline_units.sort_unstable_by(|left, right| left.1.cmp(&right.1));
            offline_units.dedup_by(|left, right| left.1 == right.1);

            for (section, units) in [
                (LogSection::Running, running_units),
                (LogSection::Offline, offline_units),
            ] {
                if units.is_empty() {
                    continue;
                }

                write_log_section_header(request.stream.try_clone()?, section)?;

                for (unit_project, service_name, pid) in units {
                    if request.structured {
                        request.stream.try_clone()?.write_all(
                            &crate::logs::service_marker_line(&service_name),
                        )?;
                    }
                    manager.stream_log_to_socket(
                        &unit_project,
                        &service_name,
                        pid,
                        request.lines,
                        requested_kind,
                        false,
                        show_headers,
                        &request.filter,
                        request.stream,
                    )?;
                }
            }
        }

        Ok(())
    }

    /// Handles handle command.
    fn handle_command(
        &mut self,
        command: ControlCommand,
    ) -> Result<ControlResponse, SupervisorError> {
        match command {
            ControlCommand::Start {
                service, project, ..
            } => {
                if let Some(service_name) = service {
                    let selector_has_project =
                        split_project_selector(&service_name).is_some();
                    let (project_id, service_name) = self
                        .start_single_service_target(&service_name, project.as_deref())?;
                    self.refresh_status_cache();
                    if project.is_some() || selector_has_project {
                        Ok(ControlResponse::Message(format!(
                            "Service '{service_name}' started in project '{project_id}'"
                        )))
                    } else {
                        Ok(ControlResponse::Message(format!(
                            "Service '{service_name}' started"
                        )))
                    }
                } else {
                    if let Some(project_id) = project.as_deref() {
                        self.start_project_target(project_id)?;
                        self.refresh_status_cache();
                        return Ok(ControlResponse::Message(format!(
                            "Project '{project_id}' started"
                        )));
                    }
                    let mut projects = vec![self.daemon.config().project.id.clone()];
                    projects.extend(self.extra_projects.keys().cloned());
                    let mut first_error = None;
                    for project_id in projects {
                        if let Err(err) = self.start_project_target(&project_id) {
                            error!("Failed to start project '{project_id}': {err}");
                            first_error.get_or_insert(err);
                        }
                    }
                    self.refresh_status_cache();
                    if let Some(err) = first_error {
                        return Err(err);
                    }
                    Ok(ControlResponse::Message("All services started".into()))
                }
            }
            ControlCommand::AddProject {
                config,
                service,
                mode,
                ..
            } => {
                let project_id =
                    self.add_project_config(Path::new(&config), service, mode)?;
                Ok(ControlResponse::Message(format!(
                    "Project '{project_id}' loaded"
                )))
            }
            ControlCommand::StopProject { project, .. } => {
                self.stop_project(&project)?;
                self.refresh_status_cache();
                Ok(ControlResponse::Message(format!(
                    "Project '{project}' stopped"
                )))
            }
            ControlCommand::Stop {
                service, project, ..
            } => {
                if service.is_none()
                    && let Some(project_id) = project.as_deref()
                {
                    self.stop_project(project_id)?;
                    self.refresh_status_cache();
                    return Ok(ControlResponse::Message(format!(
                        "Project '{project_id}' stopped"
                    )));
                }
                if let Some(service) = service {
                    let (project_id, service_name) =
                        self.stop_single_service_target(&service, project.as_deref())?;
                    self.refresh_status_cache();
                    if project.is_some() || split_project_selector(&service).is_some() {
                        Ok(ControlResponse::Message(format!(
                            "Service '{service_name}' stopped in project '{project_id}'"
                        )))
                    } else {
                        Ok(ControlResponse::Message(format!(
                            "Service '{service_name}' stopped"
                        )))
                    }
                } else {
                    self.stop_all_projects()?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message("All services stopped".into()))
                }
            }
            ControlCommand::Restart {
                config,
                service,
                project,
                ..
            } => {
                if let Some(service) = service {
                    self.restart_single_service_target(
                        &service,
                        project.as_deref(),
                        config.as_deref().map(Path::new),
                    )?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message(format!(
                        "Service '{service}' restarted"
                    )))
                } else if let Some(project_id) = project.as_deref() {
                    self.restart_project_target(
                        project_id,
                        config.as_deref().map(Path::new),
                    )?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message(format!(
                        "Project '{project_id}' restarted"
                    )))
                } else {
                    self.restart_all_targets(config.as_deref().map(Path::new))?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message("All services restarted".into()))
                }
            }
            ControlCommand::Inspect {
                unit,
                project,
                samples,
                live,
            } => {
                let snapshot = if live {
                    self.collect_live_snapshot_for_request()?
                } else {
                    self.collect_configured_snapshot()?
                };
                self.status_cache.replace(snapshot.clone());
                let limit = samples as usize;
                let matching_units: Vec<_> = snapshot
                    .units
                    .iter()
                    .filter(|status| {
                        unit_matches_selector(status, &unit, project.as_deref())
                    })
                    .cloned()
                    .collect();
                if project.is_none() && matching_units.len() > 1 {
                    let projects = matching_units
                        .iter()
                        .filter_map(|unit| {
                            unit.project.as_ref().map(|project| project.id.as_str())
                        })
                        .collect::<BTreeSet<_>>();
                    if projects.len() > 1 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!(
                                "service '{unit}' exists in multiple projects ({}); pass --project to choose one",
                                projects.into_iter().collect::<Vec<_>>().join(", ")
                            ),
                        )
                        .into());
                    }
                }
                let matching_unit = matching_units.into_iter().next();

                let metrics_samples = if let Some(ref unit_status) = matching_unit {
                    self.metrics_store
                        .try_read()
                        .ok()
                        .map(|store| store.latest_samples(&unit_status.hash, limit))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                Ok(ControlResponse::Inspect(Box::new(InspectPayload {
                    unit: matching_unit,
                    samples: metrics_samples,
                })))
            }
            ControlCommand::Logs { .. } => Ok(ControlResponse::Error(
                "logs command is streamed separately".into(),
            )),
            ControlCommand::DeclaringProjects { service } => Ok(
                ControlResponse::Projects(self.projects_declaring_service(&service)),
            ),
            ControlCommand::ClearLogs { service, project } => {
                self.clear_logs(service.as_deref(), project.as_deref())?;
                Ok(ControlResponse::Message(match service {
                    Some(name) => format!("Cleared logs for '{name}'"),
                    None => "Cleared logs for all services".into(),
                }))
            }
            ControlCommand::BootStream | ControlCommand::OpStream { .. } => Ok(
                ControlResponse::Error("progress streams are served separately".into()),
            ),
            ControlCommand::Spawn {
                parent_pid,
                name,
                command,
                ttl,
                log_level,
            } => {
                let params = SpawnParams {
                    parent_pid,
                    name,
                    command,
                    ttl,
                    log_level,
                };
                match self.handle_spawn(params) {
                    Ok(pid) => Ok(ControlResponse::Spawned { pid }),
                    Err(err) => Ok(ControlResponse::Error(err.to_string())),
                }
            }
            ControlCommand::Shutdown => {
                Ok(ControlResponse::Message("Supervisor shutting down".into()))
            }
            ControlCommand::Status { live } => {
                let mut snapshot = if live {
                    self.collect_live_snapshot_for_request()?
                } else {
                    self.collect_configured_snapshot()?
                };
                Self::apply_boots(&mut snapshot, &self.boots);
                self.status_cache.replace(snapshot.clone());
                Ok(ControlResponse::Status(snapshot))
            }
            ControlCommand::Version => Ok(ControlResponse::DaemonVersion(
                env!("CARGO_PKG_VERSION").to_string(),
            )),
            ControlCommand::Upgrade { .. } => Ok(ControlResponse::Error(
                "upgrade command must be handled by the supervisor owner loop".into(),
            )),
            ControlCommand::CurrentOp => {
                Ok(ControlResponse::CurrentOp(self.op_slot.report()))
            }
        }
    }

    /// Returns the daemon owning `project`, primary or extra.
    ///
    /// A dynamic child belongs to the project that spawned it. Resolving its
    /// definition, its service hash and its pid row through the primary daemon
    /// meant an extra project's `worker` could inherit the primary `worker`'s
    /// privileges and write its spawn rows into the wrong project's `pid.xml`.
    fn daemon_for_project(&self, project: &str) -> Option<&Daemon> {
        if self.daemon.config().project.id == project {
            return Some(&self.daemon);
        }
        self.extra_projects
            .get(project)
            .map(|runtime| &runtime.daemon)
    }

    /// Links a requesting pid to the unit that owns it, registering the unit's
    /// generation pid if the boot has not done so yet.
    ///
    /// The boot registers a unit's pid only once it has been judged started, so
    /// a unit that spawns as soon as it runs — the whole point of a dynamic
    /// orchestrator — raced that registration and had its first children
    /// refused with "no spawn tree found". Walking up from the requester also
    /// covers the ordinary case where the process asking is a descendant of the
    /// unit's own process rather than the recorded pid itself.
    fn bind_spawn_parent(&self, parent_pid: u32) -> u32 {
        // Already linked: either the registered generation pid itself or a
        // tracked child of one, both of which authorize as they stand.
        if self.spawn_manager.root_pid_for(parent_pid).is_some() {
            return parent_pid;
        }

        let mut owners: Vec<(String, HashMap<String, u32>, Arc<Config>)> = Vec::new();
        let primary = self.daemon.config();
        if let Ok(pid_file) = self.daemon.pid_file_handle().lock() {
            owners.push((
                primary.project.id.clone(),
                pid_file.services().clone(),
                primary.clone(),
            ));
        }
        for (project_id, runtime) in &self.extra_projects {
            let config = runtime.daemon.config();
            if let Ok(pid_file) = runtime.daemon.pid_file_handle().lock() {
                owners.push((
                    project_id.clone(),
                    pid_file.services().clone(),
                    config.clone(),
                ));
            }
        }

        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);
        let mut current = parent_pid;
        for _ in 0..MAX_SPAWN_PARENT_WALK {
            for (project_id, services, config) in &owners {
                let Some((service, _)) =
                    services.iter().find(|(_, pid)| **pid == current)
                else {
                    continue;
                };
                let dynamic = config.services.get(service).is_some_and(|declared| {
                    declared.spawn.as_ref().is_some_and(|spawn| {
                        matches!(spawn.mode, Some(SpawnMode::Dynamic))
                    })
                });
                if dynamic {
                    self.spawn_manager
                        .register_service_pid(project_id, service, current);
                    return current;
                }
                return parent_pid;
            }

            let Some(parent) = system
                .process(sysinfo::Pid::from_u32(current))
                .and_then(|process| process.parent())
            else {
                return parent_pid;
            };
            current = parent.as_u32();
        }
        parent_pid
    }

    /// Handles handle spawn.
    fn handle_spawn(&mut self, params: SpawnParams) -> Result<u32, SupervisorError> {
        let Some(program) = params.command.first() else {
            return Err(SupervisorError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "spawn command must not be empty",
            )));
        };

        // A requester is usually the unit's own process, but a shell wrapper
        // makes it a descendant; either way the tree is keyed by the unit's
        // recorded generation pid.
        let parent_pid = self.bind_spawn_parent(params.parent_pid);
        let spawn_auth = self
            .spawn_manager
            .authorize_spawn(parent_pid, &params.name)?;
        let depth = spawn_auth.depth;

        let auth_unit = spawn_auth
            .root_service
            .as_deref()
            .and_then(crate::spawn::split_unit_key)
            .map(|(project, service)| (project.to_string(), service.to_string()));
        let privilege = auth_unit
            .as_ref()
            .and_then(|(project, service)| {
                let daemon = self.daemon_for_project(project)?;
                let config = daemon.config();
                let declared = config.services.get(service).cloned()?;
                Some((service.clone(), declared, config.version.is_fail_closed()))
            })
            .map(|(service, service_config, fail_closed)| {
                // A dynamic child inherits its unit's security posture. Passing
                // `false` here meant a v3 manifest's fail-closed guarantee
                // stopped at the unit boundary: the service was refused for an
                // unenforceable key while the children it spawned ran with the
                // same key silently ignored.
                crate::privilege::PrivilegeContext::from_service(
                    &service,
                    &service_config,
                    fail_closed,
                )
            })
            .transpose()
            .map_err(|source| SupervisorError::from(io::Error::other(source)))?;

        // Same contract as a manifest unit: the ceiling is built before the
        // fork, so the child is inside it before it can create anything.
        let privilege = match privilege {
            Some(mut privilege) => {
                privilege.prepare_resources().map_err(SupervisorError::Io)?;
                Some(privilege)
            }
            None => None,
        };

        let mut cmd = std::process::Command::new(program);
        if params.command.len() > 1 {
            cmd.args(&params.command[1..]);
        }

        let drops_privileges = privilege
            .as_ref()
            .is_some_and(|p| p.user.drops_privileges());
        if drops_privileges {
            cmd.env_clear();
            cmd.env(
                "PATH",
                std::env::var("PATH").unwrap_or_else(|_| {
                    crate::constants::DEFAULT_SERVICE_PATH.to_string()
                }),
            );
            if let Some(privilege) = &privilege {
                for (key, value) in privilege.user.env_overrides() {
                    cmd.env(key, value);
                }
            }
        }

        cmd.env("SPAWN_DEPTH", depth.to_string());
        cmd.env("SPAWN_PARENT_PID", parent_pid.to_string());

        if let Some(log_level) = params.log_level {
            cmd.env("RUST_LOG", log_level);
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        // A dynamic child is forked by the SUPERVISOR, so without this it
        // inherits the supervisor's session and process group — the two the
        // teardown sweep deliberately refuses to signal. It was therefore
        // unreachable by every kill path, and its own forked tree with it.
        // Leading its own session makes the child sweepable as a unit.
        let privilege_pre_exec = privilege.clone();
        unsafe {
            // No logging or allocation here: this runs after fork, where taking
            // the logger lock can deadlock the child.
            cmd.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(privilege) = &privilege_pre_exec {
                    privilege.apply_pre_exec().map_err(|fault| {
                        std::io::Error::from_raw_os_error(fault.errno)
                    })?;
                }
                Ok(())
            });
        }

        let mut child = cmd.spawn()?;
        let child_pid = child.id();

        #[cfg(target_os = "linux")]
        if let Some(privilege) = &privilege
            && let Err(err) = privilege.apply_post_spawn(child_pid as libc::pid_t)
        {
            warn!(
                "Post-spawn reporting failed for dynamic child '{}': {err}",
                params.name
            );
        }

        let command_string = params.command.join(" ");
        let child_name = params.name.clone();
        let started_at = SystemTime::now();

        let spawned_child = SpawnedChild {
            name: child_name.clone(),
            pid: child_pid,
            parent_pid,
            command: command_string.clone(),
            started_at,
            ttl: params.ttl.map(Duration::from_secs),
            depth,
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
            user: None,
            kind: SpawnedChildKind::Spawned,
        };

        let root_service = self.spawn_manager.record_spawn(
            parent_pid,
            spawned_child,
            spawn_auth.root_service.clone(),
        )?;
        let effective_root = root_service.or(spawn_auth.root_service);
        // The registry key is `{project}:{service}`; log paths and service
        // hashes are addressed by the bare service name, resolved against the
        // project that actually owns the unit.
        let root_unit = effective_root
            .as_deref()
            .and_then(crate::spawn::split_unit_key)
            .map(|(project, service)| (project.to_string(), service.to_string()));
        let root_service_name = root_unit.as_ref().map(|(_, service)| service.clone());

        let echo_to_console = !self.detach_children;
        let log_result = (|| -> io::Result<()> {
            if let Some(stdout) = child.stdout.take() {
                spawn_dynamic_child_log_writer(
                    root_service_name.as_deref(),
                    &child_name,
                    child_pid,
                    stdout,
                    "stdout",
                    echo_to_console,
                )?;
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_dynamic_child_log_writer(
                    root_service_name.as_deref(),
                    &child_name,
                    child_pid,
                    stderr,
                    "stderr",
                    echo_to_console,
                )?;
            }
            Ok(())
        })();
        if let Err(err) = log_result {
            let _ = Daemon::terminate_process_tree(&child_name, child_pid, None);
            let _ = child.wait();
            self.spawn_manager.remove_subtree(child_pid);
            return Err(err.into());
        }

        let owning_daemon = root_unit
            .as_ref()
            .and_then(|(project, _)| self.daemon_for_project(project))
            .unwrap_or(&self.daemon);
        let pid_file_handle = owning_daemon.pid_file_handle();
        if let Ok(mut pid_file) = pid_file_handle.lock() {
            let service_hash = root_service_name
                .as_deref()
                .and_then(|name| owning_daemon.get_service_hash(name));
            let persisted = PersistedSpawnChild {
                pid: child_pid,
                name: child_name.clone(),
                command: command_string.clone(),
                started_at,
                ttl_secs: params.ttl,
                depth,
                parent_pid: params.parent_pid,
                service_hash,
                cpu_percent: None,
                rss_bytes: None,
                last_exit: None,
            };
            let _ = pid_file.record_spawn(persisted);
        }

        // `--ttl` was recorded and reported but never acted on, so a child the
        // docs promised would terminate after its deadline ran forever. The
        // timer is armed here rather than in a global sweep because the waiter
        // below still holds the unreaped child: signalling its pid cannot hit a
        // recycled process. A TTL is an explicit instruction about THIS child,
        // so it overrides `termination_policy` — that policy governs what
        // happens when the parent goes away, not a deadline the caller set.
        // Set once the waiter has reaped the child. After that point its pid may
        // belong to an unrelated process, so the TTL must never signal it.
        let reaped = Arc::new(AtomicBool::new(false));
        if let Some(ttl) = params.ttl.map(Duration::from_secs).filter(|d| !d.is_zero()) {
            let ttl_manager = self.spawn_manager.clone();
            let ttl_pid_file = Arc::clone(&pid_file_handle);
            let ttl_name = child_name.clone();
            let ttl_reaped = Arc::clone(&reaped);
            if let Err(err) = thread::Builder::new()
                .name(format!("sysg-ttl-{child_pid}"))
                .spawn(move || {
                    thread::sleep(ttl);
                    // The waiter reaps the child, so once it has run the pid is
                    // free to be reused and signalling it could hit an unrelated
                    // process. Tracking alone is not a safe test: a non-cascade
                    // policy leaves the entry in place after the child is gone.
                    if ttl_reaped.load(Ordering::SeqCst) {
                        return;
                    }
                    info!("Dynamic child '{ttl_name}' (pid {child_pid}) reached its TTL");
                    // `remove_subtree` returns the root as well as its
                    // descendants, so the root must not be listed again.
                    let removed = ttl_manager.remove_subtree(child_pid);
                    for target in removed
                        .iter()
                        .map(|c| (c.name.clone(), c.pid))
                        .chain(
                            removed
                                .iter()
                                .all(|c| c.pid != child_pid)
                                .then(|| (ttl_name.clone(), child_pid)),
                        )
                    {
                        if let Err(err) =
                            Daemon::terminate_process_tree(&target.0, target.1, None)
                        {
                            warn!(
                                "Failed to terminate '{}' (pid {}) at its TTL: {err}",
                                target.0, target.1
                            );
                        }
                    }
                    if let Ok(mut pid_file) = ttl_pid_file.lock()
                        && let Err(err) = pid_file.remove_spawn_subtree(child_pid)
                    {
                        warn!("Failed to clear TTL-expired spawn rows for {child_pid}: {err}");
                    }
                })
            {
                warn!("Could not arm the TTL timer for '{child_name}': {err}");
            }
        }

        let spawn_manager_for_exit = self.spawn_manager.clone();
        let pid_file_for_exit = Arc::clone(&pid_file_handle);
        let child_name_for_exit = child_name.clone();
        let reaped_for_exit = Arc::clone(&reaped);
        if let Err(err) = thread::Builder::new()
            .name(format!("sysg-child-{child_pid}"))
            .spawn(move || match child.wait() {
                Ok(status) => {
                    // From here the pid is reaped and may be reused; stand the
                    // TTL timer down before touching any tracking.
                    reaped_for_exit.store(true, Ordering::SeqCst);
                    let exit = SpawnedExit {
                        exit_code: status.code(),
                        #[cfg(unix)]
                        signal: status.signal(),
                        #[cfg(not(unix))]
                        signal: None,
                        finished_at: Some(SystemTime::now()),
                    };

                    spawn_manager_for_exit.record_spawn_exit(child_pid, exit.clone());

                    let termination_policy = spawn_manager_for_exit
                        .termination_policy_for(child_pid)
                        .unwrap_or(TerminationPolicy::Cascade);

                    if matches!(termination_policy, TerminationPolicy::Cascade) {
                        let removed = spawn_manager_for_exit.remove_subtree(child_pid);

                        if let Ok(mut pid_file) = pid_file_for_exit.lock()
                            && let Err(err) = pid_file.remove_spawn_subtree(child_pid)
                        {
                            warn!(
                                "Failed to remove spawn subtree rooted at {} from pid file: {}",
                                child_pid, err
                            );
                        }

                        // Every dynamic child is forked by the supervisor, so
                        // depth-2 children are siblings of depth-1 ones in
                        // process terms — ancestry does not chain and killing
                        // only the direct children left the rest running.
                        for descendant in removed.iter().filter(|c| c.pid != child_pid) {
                            if let Err(err) = Daemon::terminate_process_tree(
                                &descendant.name,
                                descendant.pid,
                                None,
                            ) {
                                warn!(
                                    "Failed to terminate descendant {} (pid {}) of '{}' after cascade: {}",
                                    descendant.name, descendant.pid, child_name_for_exit, err
                                );
                            }
                        }
                    } else if let Ok(mut pid_file) = pid_file_for_exit.lock()
                        && let Err(err) = pid_file.record_spawn_exit(child_pid, exit.clone())
                    {
                        warn!(
                            "Failed to record spawn exit for {} in pid file: {}",
                            child_pid, err
                        );
                    }
                }
                Err(err) => {
                    reaped_for_exit.store(true, Ordering::SeqCst);
                    error!("Failed to wait for spawned child {child_pid}: {err}");
                }
            })
        {
            let _ = Daemon::terminate_process_tree(&child_name, child_pid, None);
            let _ = self.spawn_manager.remove_subtree(child_pid);
            if let Ok(mut pid_file) = pid_file_handle.lock() {
                let _ = pid_file.remove_spawn_subtree(child_pid);
            }
            return Err(err.into());
        }

        info!(
            "Spawned child '{}' (PID: {}) from parent {}",
            child_name, child_pid, params.parent_pid
        );

        Ok(child_pid)
    }

    /// Validates and reconciles every project declared by one manifest.
    fn reload_config(&mut self, path: &Path) -> Result<(), SupervisorError> {
        let (resolved, configs) = self.load_restart_manifest(path)?;
        let owned = self
            .extra_projects
            .iter()
            .filter(|(_, runtime)| runtime.config_path == self.config_path)
            .map(|(project_id, _)| project_id.clone())
            .collect();
        self.apply_restart_manifest(resolved, configs, true, owned)
    }

    /// Reloads all registered manifests on a bare restart, validating every
    /// file before the first project mutation.
    fn restart_all_targets(
        &mut self,
        config_path: Option<&Path>,
    ) -> Result<(), SupervisorError> {
        if let Some(path) = config_path {
            return self.reload_config(path);
        }

        let primary_path = self.config_path.clone();
        let mut paths = BTreeSet::from([primary_path.clone()]);
        paths.extend(
            self.extra_projects
                .values()
                .map(|runtime| runtime.config_path.clone()),
        );
        let mut loaded = Vec::with_capacity(paths.len());
        let mut declared = BTreeMap::<String, PathBuf>::new();
        for path in paths {
            let (resolved, configs) = self.load_restart_manifest(&path)?;
            for config in &configs {
                if let Some(other) =
                    declared.insert(config.project.id.clone(), resolved.clone())
                    && other != resolved
                {
                    return Err(ProcessManagerError::Diag(Box::new(
                        crate::restart::manifest_rejected(format!(
                            "project '{}' is declared by both {} and {}",
                            config.project.id,
                            other.display(),
                            resolved.display()
                        )),
                    ))
                    .into());
                }
            }
            let owned = self
                .extra_projects
                .iter()
                .filter(|(_, runtime)| runtime.config_path == resolved)
                .map(|(project_id, _)| project_id.clone())
                .collect();
            loaded.push((resolved, configs, path == primary_path, owned));
        }
        loaded.sort_by_key(|(_, _, owns_primary, _)| !*owns_primary);
        for (resolved, configs, owns_primary, owned) in loaded {
            self.apply_restart_manifest(resolved, configs, owns_primary, owned)?;
        }
        Ok(())
    }

    /// Applies one fully validated manifest to the runtimes sourced from it.
    fn apply_restart_manifest(
        &mut self,
        resolved: PathBuf,
        mut configs: Vec<Config>,
        owns_primary: bool,
        owned_extras: BTreeSet<String>,
    ) -> Result<(), SupervisorError> {
        info!("Reloading configuration from {:?}", resolved);
        let declared = configs
            .iter()
            .map(|config| config.project.id.clone())
            .collect::<BTreeSet<_>>();

        if owns_primary {
            let primary_id = self.daemon.config().project.id.clone();
            let index = configs
                .iter()
                .position(|config| config.project.id == primary_id)
                .unwrap_or(0);
            let primary = configs.remove(index);
            if primary.project.id == primary_id {
                self.reconcile_primary_project(primary)?;
                self.config_path = resolved.clone();
                ipc::write_config_hint(&self.config_path)?;
            } else {
                if self.extra_projects.contains_key(&primary.project.id) {
                    return Err(ProcessManagerError::Diag(Box::new(
                        crate::restart::manifest_rejected(format!(
                            "project '{}' cannot replace the primary while it is already registered",
                            primary.project.id
                        )),
                    ))
                    .into());
                }
                self.replace_primary_project_runtime(primary, resolved.clone())?;
            }
        }

        let primary_id = self.daemon.config().project.id.clone();
        for config in configs {
            let project_id = config.project.id.clone();
            if project_id == primary_id {
                return Err(ProcessManagerError::Diag(Box::new(
                    crate::restart::manifest_rejected(format!(
                        "project '{project_id}' is declared by multiple registered manifests"
                    )),
                ))
                .into());
            }
            if self.extra_projects.contains_key(&project_id) {
                self.reconcile_extra_project(config, resolved.clone())?;
            } else {
                self.add_extra_project(config, resolved.clone())?;
            }
        }

        for project_id in owned_extras {
            if !declared.contains(&project_id)
                && self.extra_projects.contains_key(&project_id)
            {
                self.stop_project(&project_id)?;
            }
        }
        self.sync_cron_projects()?;
        self.refresh_status_cache();
        self.respawn_status_refresher()?;
        Ok(())
    }

    /// Registers a new project synchronously so restart can report its outcome.
    fn add_extra_project(
        &mut self,
        config: Config,
        config_path: PathBuf,
    ) -> Result<(), SupervisorError> {
        let project_id = config.project.id.clone();
        Self::register_spawn_limits_for_config(&self.spawn_manager, &config)?;
        let mut daemon = Daemon::from_config(config, self.detach_children)?;
        daemon.set_timeouts(self.timeouts.clone());
        daemon.set_pipe_stderr(self.pipe_stderr);
        daemon.set_op_slot(self.op_slot.clone());
        if let Ok(mut projects) = self.boot_projects.write() {
            projects.insert(project_id.clone(), daemon.clone());
        }
        // Created here, so the watch set built when the command arrived could
        // not have covered it. Synchronous, so it needs no lease.
        let _watch = self
            .active_op
            .as_ref()
            .map(|(op, journal)| daemon.watch(op, journal.clone()));
        let result = Self::start_project_services(
            &daemon,
            daemon.config().as_ref(),
            None,
            &self.spawn_manager,
            None,
        );
        let failed = match result {
            Ok(failed) => failed,
            Err(err) => {
                let _ = daemon.stop_services();
                if let Ok(mut projects) = self.boot_projects.write() {
                    projects.remove(&project_id);
                }
                return Err(err);
            }
        };
        self.extra_projects.insert(
            project_id.clone(),
            ProjectRuntime {
                daemon,
                mode: ProjectRunMode::Daemon,
                config_path,
            },
        );
        self.sync_cron_projects()?;
        if !failed.is_empty() {
            return Err(ProcessManagerError::Diag(Box::new(
                crate::restart::reconcile_incomplete(Some(failed.services()), None),
            ))
            .into());
        }
        Ok(())
    }

    /// Replaces the primary runtime when its manifest renames or replaces the project.
    fn replace_primary_project_runtime(
        &mut self,
        config: Config,
        config_path: PathBuf,
    ) -> Result<(), SupervisorError> {
        let old_id = self.daemon.config().project.id.clone();
        let old_config = self.daemon.config();
        let old_daemon = self.daemon.clone();
        let old_metrics = self.metrics_store.clone();
        let metrics_settings = config
            .metrics
            .to_settings(config.project_dir.as_deref().map(Path::new));
        let metrics_store = metrics::shared_store(metrics_settings)?;
        Self::register_spawn_limits_for_config(&self.spawn_manager, &config)?;
        let new_id = config.project.id.clone();
        let mut replacement = Daemon::from_config(config, self.detach_children)?;
        replacement.set_timeouts(self.timeouts.clone());
        replacement.set_pipe_stderr(self.pipe_stderr);
        replacement.set_op_slot(self.op_slot.clone());

        self.stop_primary_workers();
        old_daemon.cancel_boot();
        old_daemon.shutdown_monitor();
        if let Err(err) = old_daemon.stop_services() {
            self.restore_primary_project(old_config, old_metrics)?;
            return Err(err.into());
        }

        let result = Self::start_project_services(
            &replacement,
            replacement.config().as_ref(),
            None,
            &self.spawn_manager,
            None,
        );
        let failed = match result {
            Ok(failed) => failed,
            Err(err) => {
                let _ = replacement.stop_services();
                self.restore_primary_project(old_config, old_metrics)?;
                return Err(err);
            }
        };
        if !failed.is_empty() {
            let _ = replacement.stop_services();
            self.restore_primary_project(old_config, old_metrics)?;
            return Err(ProcessManagerError::Diag(Box::new(
                crate::restart::reconcile_incomplete(Some(failed.services()), None),
            ))
            .into());
        }

        self.daemon = replacement;
        self.primary_active = true;
        self.config_path = config_path;
        self.metrics_store = metrics_store;
        if let Ok(mut projects) = self.boot_projects.write() {
            projects.remove(&old_id);
            projects.insert(new_id, self.daemon.clone());
        }
        ipc::write_config_hint(&self.config_path)?;
        self.sync_cron_projects()?;
        self.start_primary_workers()?;
        Ok(())
    }

    /// Adds another project config to the resident supervisor and starts its services.
    fn add_project_config(
        &mut self,
        path: &Path,
        service_filter: Option<String>,
        mode: ProjectRunMode,
    ) -> Result<String, SupervisorError> {
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let resolved = resolved.canonicalize().unwrap_or(resolved);
        let trusted = runtime::open_trusted_config(&resolved)?;
        let configs = load_projects_from_file(trusted, &resolved)?;

        let mut last_id = None;
        let mut loose_ids = Vec::new();
        for config in configs {
            let is_loose = config.project.loose;
            let id =
                self.register_one_project(config, &resolved, &service_filter, mode)?;
            if is_loose {
                loose_ids.push(id.clone());
            }
            last_id = Some(id);
        }
        for project_id in loose_ids {
            self.record_loose_manifest(&resolved, &project_id, mode);
        }
        last_id.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("config at {} declared no projects", resolved.display()),
            )
            .into()
        })
    }

    /// Records a loose manifest so a cold boot can restore it.
    ///
    /// Best-effort: a registry the supervisor cannot write is a lost restore on
    /// the next boot, not a reason to fail a project that has already started.
    fn record_loose_manifest(
        &self,
        config_path: &Path,
        project_id: &str,
        mode: ProjectRunMode,
    ) {
        use crate::loose_registry::{LooseEntry, LooseRegistry};

        let mut registry = match LooseRegistry::load() {
            Ok(registry) => registry,
            Err(err) => {
                warn!("Loose registry unreadable, not recording '{project_id}': {err}");
                return;
            }
        };
        registry.insert(LooseEntry {
            config_path: config_path.to_string_lossy().to_string(),
            project_id: project_id.to_string(),
            mode,
        });
        if let Err(err) = registry.save() {
            warn!("Could not record loose project '{project_id}': {err}");
        }
    }

    /// Publishes a settled boot verdict for `project`.
    ///
    /// `settled` is what releases a waiting `sysg start`, and a caller that
    /// never sees one is left judging raw unit states — where a one-shot caught
    /// between its reap and its lifecycle stamp reads `Lost` and is blamed for a
    /// boot it completed. The primary branch below returns without queueing a
    /// background boot, so it must publish this verdict itself.
    fn settle_boot(
        &self,
        project: &str,
        failed: Vec<String>,
        cause: Option<crate::diag::Diagnostic>,
    ) {
        self.boots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                project.to_string(),
                BootStatus {
                    settled: true,
                    failed,
                    cause,
                },
            );
    }

    /// Registers and starts a single already-parsed project config, the unit of
    /// work `add_project_config` loops over once per project a file declares.
    fn register_one_project(
        &mut self,
        config: Config,
        resolved: &Path,
        service_filter: &Option<String>,
        mode: ProjectRunMode,
    ) -> Result<String, SupervisorError> {
        let service_filter = service_filter.clone();
        let resolved = resolved.to_path_buf();
        let project_id = config.project.id.clone();
        let primary_project = self.daemon.config().project.id.clone();
        self.boots
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&project_id);

        if project_id == primary_project {
            // `AddProject` attaches nothing up front because its projects
            // usually do not exist yet — but a config naming the PRIMARY is
            // handled here, by the daemon that already exists. Unattached, this
            // whole branch reported to nobody and drew a head line with an
            // empty tree under it. Everything below runs on this thread, so no
            // lease is needed to keep the journal open.
            let _watch = self
                .active_op
                .as_ref()
                .map(|(op, journal)| self.daemon.watch(op, journal.clone()));

            // Every exit from this branch must leave a settled verdict — an
            // early `?` that publishes nothing strands a waiting `sysg start`
            // on the poll's guesswork for the whole grace — so the work is
            // delegated and its result, success or error, is settled once here.
            let outcome =
                self.register_primary_project(config, resolved, service_filter, mode);
            return match outcome {
                Ok(failed) => {
                    self.settle_boot(
                        &project_id,
                        failed.services.clone(),
                        failed.cause.clone(),
                    );
                    if !failed.is_empty() {
                        return Err(failed.into_error(&project_id).into());
                    }
                    Ok(project_id)
                }
                Err(err) => {
                    let cause = match error_response(&err) {
                        ControlResponse::Diag(diag) => Some(*diag),
                        ControlResponse::Error(message) => {
                            Some(crate::start::unit_start_failed(&project_id, message))
                        }
                        _ => None,
                    };
                    self.settle_boot(&project_id, Vec::new(), cause);
                    Err(err)
                }
            };
        }

        self.register_extra_project(config, resolved, service_filter, mode, project_id)
    }

    /// Starts or reconciles the PRIMARY project on the caller's thread.
    ///
    /// Returns the boot's failure report; the caller settles it. Errors here are
    /// settled by the caller too, so no exit path can leave the boot unresolved.
    fn register_primary_project(
        &mut self,
        config: Config,
        resolved: PathBuf,
        service_filter: Option<String>,
        mode: ProjectRunMode,
    ) -> Result<BootFailures, SupervisorError> {
        let unchanged =
            crate::restart::ManifestDiff::compute(self.daemon.config().as_ref(), &config)
                .is_empty();
        if self.primary_active
            && service_filter.is_none()
            && unchanged
            && !self.daemon.needs_start()
        {
            self.sync_cron_projects()?;
            self.primary_project_mode = mode;
            self.config_path = resolved;
            let _ = ipc::write_config_hint(&self.config_path);
            self.refresh_status_cache();
            return Ok(BootFailures::new(Vec::new(), None));
        }
        if !unchanged {
            self.reconcile_primary_project(config)?;
            self.primary_project_mode = mode;
            self.config_path = resolved;
            let _ = ipc::write_config_hint(&self.config_path);
            return Ok(BootFailures::new(Vec::new(), None));
        }
        self.primary_active = true;
        let failed = Self::start_project_services(
            &self.daemon,
            self.daemon.config().as_ref(),
            service_filter.as_deref(),
            &self.spawn_manager,
            None,
        )?;
        self.sync_cron_projects()?;
        self.refresh_status_cache();
        if self.daemon.boot_cancelled() {
            return Ok(BootFailures::new(Vec::new(), None));
        }
        if !failed.is_empty() {
            return Ok(failed);
        }
        self.primary_project_mode = mode;
        self.config_path = resolved;
        let _ = ipc::write_config_hint(&self.config_path);
        Ok(failed)
    }

    /// Registers a NON-primary project, queueing its boot onto its own thread.
    fn register_extra_project(
        &mut self,
        config: Config,
        resolved: PathBuf,
        service_filter: Option<String>,
        mode: ProjectRunMode,
        project_id: String,
    ) -> Result<String, SupervisorError> {
        if !self.extra_projects.contains_key(&project_id) {
            Self::register_spawn_limits_for_config(&self.spawn_manager, &config)?;
            // Own pid/state handles bound to this project's store, so a
            // separately-added project never leaks services into a sibling's
            // pid.xml.
            let mut daemon = Daemon::from_config(config.clone(), self.detach_children)?;
            daemon.set_timeouts(self.timeouts.clone());
            daemon.set_pipe_stderr(self.pipe_stderr);
            daemon.set_op_slot(self.op_slot.clone());
            if let Ok(mut projects) = self.boot_projects.write() {
                projects.insert(project_id.clone(), daemon.clone());
            }
            self.extra_projects.insert(
                project_id.clone(),
                ProjectRuntime {
                    daemon,
                    mode,
                    config_path: resolved.clone(),
                },
            );
        } else if let Some(project) = self.extra_projects.get_mut(&project_id) {
            // Idempotent re-registration of an already-managed extra project: if
            // its manifest is unchanged, update routing metadata and return
            // without re-booting the services that are already running.
            let unchanged = crate::restart::ManifestDiff::compute(
                project.daemon.config().as_ref(),
                &config,
            )
            .is_empty();
            project.mode = mode;
            project.config_path = resolved.clone();
            if unchanged && !project.daemon.needs_start() {
                self.sync_cron_projects()?;
                self.refresh_status_cache();
                self.respawn_status_refresher()?;
                return Ok(project_id);
            }
            self.replace_extra_project_runtime(config, resolved)?;
            self.refresh_status_cache();
            self.respawn_status_refresher()?;
            return Ok(project_id);
        }

        let project = self.extra_projects.get(&project_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' was not registered"),
            )
        })?;

        if matches!(mode, ProjectRunMode::Daemon) {
            let daemon = project.daemon.clone();
            // This project's daemon did not exist when the command was watched,
            // so it attaches here — and the boot runs on its own thread, so the
            // guard and a lease on the journal move onto it. Without the lease
            // the stream would seal the moment the command returned, ending the
            // tree at queue time before a single unit had run.
            let boot_watch = self.active_op.as_ref().map(|(op, journal)| {
                (daemon.watch(op, journal.clone()), self.op_lease.clone())
            });
            let spawn_manager = self.spawn_manager.clone();
            let op_slot = self.op_slot.clone();
            let boot_project = project_id.clone();
            let boot_filter = service_filter.clone();
            // The refresher's periodic ticks will eventually reflect this project,
            // but the boot records PIDs AFTER this method returns and re-seeds the
            // cache — so the served snapshot would report this project's services
            // as `stopped` until the next tick. For the LAST project added there is
            // no subsequent synchronous refresh to mask that gap, so the boot itself
            // must re-seed the cache once its PIDs are on disk. These handles are the
            // same live Arcs the refresher uses, so this converges the served cache
            // to truth the instant the boot finishes.
            let boot_cache = self.status_cache.clone();
            let boot_projects = Arc::clone(&self.cron_projects);
            let boot_metrics = self.metrics_store.clone();
            let boot_spawn = self.spawn_manager.clone();
            let boot_mode = Self::status_snapshot_mode(self.daemon.config().as_ref());
            let boots = Arc::clone(&self.boots);
            boots
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    project_id.clone(),
                    BootStatus {
                        settled: false,
                        failed: Vec::new(),
                        cause: None,
                    },
                );
            if let Err(err) = thread::Builder::new()
                .name(format!("sysg-boot-{project_id}"))
                .spawn(move || {
                    let _op = op_slot.guard(format!("starting project '{boot_project}'"));
                    // Held for the whole boot: dropping either detaches this
                    // daemon's journal or seals the stream early.
                    let _watch = boot_watch;
                    let result = Self::start_project_services(
                        &daemon,
                        daemon.config().as_ref(),
                        boot_filter.as_deref(),
                        &spawn_manager,
                        None,
                    );
                    let mut boot = match &result {
                        Ok(failed) => BootStatus {
                            settled: true,
                            failed: failed.services.clone(),
                            cause: failed.cause.clone(),
                        },
                        Err(err) => {
                            let cause = match error_response(err) {
                                ControlResponse::Diag(diag) => Some(*diag),
                                ControlResponse::Error(message) => Some(
                                    crate::start::unit_start_failed(
                                        &boot_project,
                                        message,
                                    ),
                                ),
                                _ => None,
                            };
                            BootStatus {
                                settled: true,
                                failed: Vec::new(),
                                cause,
                            }
                        }
                    };
                    // Publish before settling, for the same reason the primary
                    // boot publishes before `BootFrame::Done`: `settled` is what
                    // releases a waiting `sysg start`, so a snapshot taken after
                    // it lets the caller observe the pre-boot world.
                    //
                    // This runs even under `snapshot_mode: off`, which disables
                    // only the periodic refresher — a default `status` read is
                    // still served straight from this cache, so skipping the
                    // publication here would leave it pre-boot indefinitely.
                    //
                    // Collection is best-effort per project and the owner thread
                    // may not have added this one to the routing table yet, so a
                    // snapshot that silently omits it is not a publication: say
                    // so on the boot rather than settle over a cache that never
                    // described the project the caller is waiting for.
                    match Self::collect_projects_snapshot(
                        &boot_projects,
                        &boot_metrics,
                        &boot_spawn,
                        Self::live_snapshot_mode(boot_mode),
                    ) {
                        Ok(snapshot) if snapshot.has_project(&boot_project) => {
                            boot_cache.replace(snapshot);
                        }
                        outcome => {
                            let detail = match outcome {
                                Err(err) => err.to_string(),
                                _ => format!(
                                    "project '{boot_project}' was not in the collected snapshot"
                                ),
                            };
                            error!(
                                "failed to publish status snapshot for project '{boot_project}': {detail}"
                            );
                            boot.cause.get_or_insert_with(|| {
                                crate::status::diagnostics::snapshot_unavailable(detail)
                            });
                        }
                    }

                    boots
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(boot_project.clone(), boot);
                    match result {
                        Ok(failed) if !failed.is_empty() => error!(
                            "Background boot of project '{boot_project}' left services down: {}",
                            failed.services().join(", ")
                        ),
                        Err(err) => {
                            error!("Background boot of project '{boot_project}' failed: {err}")
                        }
                        _ => {}
                    }
                })
            {
                self.extra_projects.remove(&project_id);
                self.boot_projects
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&project_id);
                self.boots
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&project_id);
                self.sync_cron_projects()?;
                return Err(err.into());
            }
            self.sync_cron_projects()?;
            self.refresh_status_cache();
            self.respawn_status_refresher()?;
            return Ok(project_id);
        }

        let _op = self
            .op_slot
            .guard(format!("starting project '{project_id}'"));
        let start_result = Self::start_project_services(
            &project.daemon,
            project.daemon.config().as_ref(),
            service_filter.as_deref(),
            &self.spawn_manager,
            None,
        );
        let failed = start_result?;
        let boot_cancelled = project.daemon.boot_cancelled();
        self.sync_cron_projects()?;
        // Seed the cache only AFTER this project's PIDs are on disk. Refreshing
        // before the boot settles served a snapshot with no units for it, so a
        // caller polling status right after an attach saw the project as absent
        // — which reads as the wrong run mode, or as a project that never
        // started, rather than one still recording its PIDs.
        if failed.is_empty() {
            self.await_project_pids(&project_id);
        }
        self.refresh_status_cache();
        self.respawn_status_refresher()?;
        if boot_cancelled {
            return Ok(project_id);
        }
        if !failed.is_empty() {
            return Err(failed.into_error(&project_id).into());
        }
        Ok(project_id)
    }

    /// Blocks briefly until a freshly-booted project has recorded at least one
    /// PID, so a status snapshot taken right after registration reflects it.
    /// Bounded: a project whose services all exit immediately never records one,
    /// and must not wedge the caller.
    fn await_project_pids(&self, project_id: &str) {
        let Some(project) = self.extra_projects.get(project_id) else {
            return;
        };
        let deadline =
            std::time::Instant::now() + crate::constants::PROJECT_PID_SETTLE_TIMEOUT;
        while std::time::Instant::now() < deadline {
            if project.daemon.boot_cancelled() {
                return;
            }
            let recorded = project
                .daemon
                .pid_file_handle()
                .lock()
                .map(|guard| !guard.services().is_empty())
                .unwrap_or(false);
            if recorded {
                return;
            }
            thread::sleep(crate::constants::SERVICE_POLL_INTERVAL);
        }
    }

    /// Restarts one service in the selected project without reloading unrelated projects.
    /// Restarts `root` and its transitive dependents in dependency order.
    /// A dependent must re-handshake the freshly-restarted dependency, so
    /// `restart -s A` bounces A then everything that depends on A. A dependent
    /// carrying `skip: true` is honored — it is not launched by the cascade.
    /// Stops a unit a cascade decided to skip, then records it as skipped.
    ///
    /// A unit that was already running when the cascade reached it keeps its
    /// process unless it is stopped here: marking it skipped while it runs
    /// leaves an untracked process behind and makes status describe a unit that
    /// is not in the state it reports.
    /// A unit that is not running already stops cleanly, so a real error here
    /// means the process is still alive; it propagates rather than being logged,
    /// because recording `Skipped` over a live process is the exact lie this is
    /// meant to prevent.
    fn retire_skipped_unit(daemon: &Daemon, name: &str) -> Result<(), SupervisorError> {
        daemon.stop_service(name)?;
        daemon.mark_service_skipped(name)?;
        Ok(())
    }

    fn cascade_restart(
        daemon: &Daemon,
        config: &Config,
        root: &str,
        target_project: &str,
    ) -> Result<(), SupervisorError> {
        daemon.begin_boot();
        // A skipped unit does not satisfy its dependents, so a cascade must
        // carry that verdict downstream: restarting a dependent whose
        // dependency was skipped starts it against something that never came
        // up. `skipped` accumulates those roots as the BFS order is walked,
        // which is ordered dependency-before-dependent.
        let mut skipped: HashSet<String> = HashSet::new();
        // The cascade starts at `root`, so a unit skipped ABOVE it is never
        // visited and its verdict would be lost: `restart -s B` where B depends
        // on an already-skipped A must still leave B unstarted. Seed the set
        // from the manifest AND from what actually happened — a conditional
        // skip is only knowable from the lifecycle its last evaluation wrote,
        // and its predicate is deliberately NOT re-run here: this cascade is
        // not restarting A, so A's verdict stands as recorded.
        for (name, service_config) in &config.services {
            let statically_skipped =
                matches!(service_config.skip, Some(SkipConfig::Flag(true)));
            let recorded_skipped = matches!(
                daemon.recorded_status(name),
                Some(ServiceLifecycleStatus::Skipped)
            );
            if statically_skipped || recorded_skipped {
                skipped.insert(name.clone());
            }
        }
        for name in cascade_restart_order(config, root) {
            let Some(service_config) = config.services.get(&name) else {
                continue;
            };
            // Opened before the skip branches so every unit the cascade visits
            // gets a row: a unit that only ever resolves has nothing to resolve
            // against, and the watching client would draw it out of order.
            daemon.note_unit_starting(&name);
            if let Some(blocker) = service_config.depends_on.as_ref().and_then(|deps| {
                deps.iter()
                    .map(|dependency| dependency.service())
                    .find(|dependency| skipped.contains(*dependency))
            }) {
                info!(
                    "Skipping dependent '{name}' during cascade restart (dependency '{blocker}' was skipped)"
                );
                Self::retire_skipped_unit(daemon, &name).inspect_err(|err| {
                    daemon.note_unit_done(&name, Self::cascade_failure(&name, err));
                })?;
                daemon.note_unit_done(&name, start::Outcome::Skipped);
                skipped.insert(name);
                continue;
            }
            if matches!(service_config.skip, Some(SkipConfig::Flag(true))) {
                info!("Skipping dependent '{name}' during cascade restart (skip flag)");
                Self::retire_skipped_unit(daemon, &name).inspect_err(|err| {
                    daemon.note_unit_done(&name, Self::cascade_failure(&name, err));
                })?;
                daemon.note_unit_done(&name, start::Outcome::Skipped);
                skipped.insert(name);
                continue;
            }
            reject_direct_cron_control(
                service_config,
                &name,
                target_project,
                "restarted",
            )
            .inspect_err(|err| {
                daemon.note_unit_done(&name, Self::cascade_failure(&name, err));
            })?;
            // A conditional skip is only known once its predicate has run, so
            // the restart's own verdict is what says whether this unit came up.
            // Without it a unit skipped by its condition still satisfies its
            // dependents, which is the leak this cascade is fixing. The verdict
            // also RETIRES the seeded guess: a predicate that has flipped back
            // off makes the recorded `Skipped` stale, and leaving it in the set
            // would strand every dependent behind a unit that just came up.
            let ready = daemon
                .restart_service(&name, service_config)
                .map_err(SupervisorError::from)
                .inspect_err(|err| {
                    daemon.note_unit_done(&name, Self::cascade_failure(&name, err));
                })?;
            daemon.note_unit_done(&name, Self::cascade_outcome(daemon, &name, ready));
            if matches!(ready, ServiceReadyState::Skipped) {
                skipped.insert(name);
            } else {
                skipped.remove(&name);
            }
        }
        Ok(())
    }

    /// The terminal frame for a unit the cascade restarted successfully.
    fn cascade_outcome(
        daemon: &Daemon,
        service: &str,
        ready: ServiceReadyState,
    ) -> start::Outcome {
        match ready {
            ServiceReadyState::Running => {
                let pid = daemon
                    .pid_file_handle()
                    .lock()
                    .ok()
                    .and_then(|pid_file| pid_file.services().get(service).copied());
                match pid {
                    Some(pid) => start::Outcome::Up(start::Liveness { pid }),
                    None => start::Outcome::Failed(start::unit_start_failed(
                        service,
                        "the service reported running but no PID was recorded",
                    )),
                }
            }
            ServiceReadyState::CompletedSuccess => start::Outcome::Completed,
            ServiceReadyState::Skipped => start::Outcome::Skipped,
        }
    }

    /// The terminal frame for a unit the cascade could not restart.
    ///
    /// The daemon's own SG01xx diagnostic is kept when it carries one, so the
    /// tree marks the row with the specific failure rather than a generic one.
    fn cascade_failure(service: &str, err: &SupervisorError) -> start::Outcome {
        match err {
            SupervisorError::Process(ProcessManagerError::Diag(diag)) => {
                start::Outcome::Failed((**diag).clone())
            }
            other => start::Outcome::Failed(start::unit_start_failed(
                service,
                other.to_string(),
            )),
        }
    }

    fn restart_single_service_target(
        &mut self,
        selector: &str,
        project: Option<&str>,
        config_path: Option<&Path>,
    ) -> Result<(), SupervisorError> {
        let (selector_project, service_name) = split_project_selector(selector)
            .map(|(project_id, service_name)| (Some(project_id), service_name))
            .unwrap_or((None, selector));
        let requested_project = project.or(selector_project);
        if let (Some(flag), Some(prefix)) = (project, selector_project)
            && flag != prefix
        {
            return Err(ProcessManagerError::Diag(Box::new(start::project_mismatch(
                flag, prefix,
            )))
            .into());
        }

        let paths = if let Some(path) = config_path {
            BTreeSet::from([path.to_path_buf()])
        } else if let Some(project_id) = requested_project {
            let path = if self.daemon.config().project.id == project_id {
                Some(self.config_path.clone())
            } else {
                self.extra_projects
                    .get(project_id)
                    .map(|runtime| runtime.config_path.clone())
            }
            .ok_or_else(|| {
                ProcessManagerError::Diag(Box::new(crate::stop::project_not_found(
                    project_id,
                )))
            })?;
            BTreeSet::from([path])
        } else {
            let mut paths = BTreeSet::from([self.config_path.clone()]);
            paths.extend(
                self.extra_projects
                    .values()
                    .map(|runtime| runtime.config_path.clone()),
            );
            paths
        };

        let mut candidates = Vec::new();
        for path in paths {
            let (resolved, configs) = self.load_restart_manifest(&path)?;
            candidates.extend(configs.into_iter().filter_map(|config| {
                let matches_project = requested_project
                    .is_none_or(|project_id| config.project.id == project_id);
                (matches_project && config.services.contains_key(service_name))
                    .then_some((resolved.clone(), config))
            }));
        }
        if candidates.is_empty() {
            return Err(ProcessManagerError::Diag(Box::new(
                crate::stop::service_not_found(service_name),
            ))
            .into());
        }
        let mut projects = candidates
            .iter()
            .map(|(_, config)| config.project.id.clone())
            .collect::<Vec<_>>();
        projects.sort_unstable();
        projects.dedup();
        if requested_project.is_none() && projects.len() > 1 {
            return Err(ProcessManagerError::Diag(Box::new(start::ambiguous_service(
                service_name,
                &projects,
            )))
            .into());
        }
        if candidates.len() > 1 {
            return Err(ProcessManagerError::Diag(Box::new(
                crate::restart::manifest_rejected(format!(
                    "project '{}' is declared by multiple registered manifests",
                    projects[0]
                )),
            ))
            .into());
        }

        let (resolved, config) = candidates.remove(0);
        let target_project = config.project.id.clone();
        let service = config.services.get(service_name).ok_or_else(|| {
            ProcessManagerError::Diag(Box::new(crate::stop::service_not_found(
                service_name,
            )))
        })?;
        reject_direct_cron_control(service, service_name, &target_project, "restarted")?;

        let primary_project = self.daemon.config().project.id.clone();
        if target_project == primary_project {
            if let Some(adopted) =
                Self::adopt_service_config(&self.daemon.config(), &config, service_name)
            {
                Self::validate_adoption(&self.daemon.config(), &adopted, service_name)?;
                Self::sync_adopted_spawn_limits(
                    &self.spawn_manager,
                    &adopted,
                    service_name,
                )?;
                self.daemon.set_config(adopted);
                self.daemon.refresh_monitor()?;
                self.respawn_metrics_collector()?;
            }
            let live = self.daemon.config();
            return Self::cascade_restart(
                &self.daemon,
                live.as_ref(),
                service_name,
                &target_project,
            );
        }

        if !self.extra_projects.contains_key(&target_project) {
            return self.add_extra_project(config, resolved);
        }
        let runtime = self.extra_projects.get(&target_project).ok_or_else(|| {
            ProcessManagerError::Diag(Box::new(crate::stop::project_not_found(
                &target_project,
            )))
        })?;
        if let Some(adopted) =
            Self::adopt_service_config(&runtime.daemon.config(), &config, service_name)
        {
            Self::validate_adoption(&runtime.daemon.config(), &adopted, service_name)?;
            Self::sync_adopted_spawn_limits(&self.spawn_manager, &adopted, service_name)?;
            runtime.daemon.set_config(adopted);
            runtime.daemon.refresh_monitor()?;
        }
        let live = runtime.daemon.config();
        Self::cascade_restart(
            &runtime.daemon,
            live.as_ref(),
            service_name,
            &target_project,
        )
    }

    /// Refuses an adoption that would change the project's structure rather
    /// than one service's own definition.
    ///
    /// The adopted config is live-plus-target, so a manifest that also ADDED
    /// a new dependency produces a hybrid where the dependency exists on disk
    /// but not in the running set — starting the target against it would wait
    /// on a service nothing is going to run. The same goes for a service that
    /// switched between cron and plain kinds (the scheduler's routing would go
    /// stale), and for a dependency graph the hybrid can no longer order.
    /// Structural changes belong to a project-wide restart, and the refusal
    /// says so before anything is touched.
    fn validate_adoption(
        live: &Config,
        adopted: &Config,
        service: &str,
    ) -> Result<(), SupervisorError> {
        let Some(declared) = adopted.services.get(service) else {
            return Ok(());
        };
        let was_cron = live
            .services
            .get(service)
            .is_some_and(|running| running.cron.is_some());
        if was_cron != declared.cron.is_some() {
            return Err(ProcessManagerError::Diag(Box::new(
                crate::restart::manifest_rejected(format!(
                    "service '{service}' changed between cron and plain kinds; \
restart the project to apply structural changes"
                )),
            ))
            .into());
        }
        if let Some(dependencies) = &declared.depends_on {
            for dependency in dependencies {
                let name = dependency.service();
                if !adopted.services.contains_key(name) {
                    return Err(ProcessManagerError::Diag(Box::new(
                        crate::restart::manifest_rejected(format!(
                            "service '{service}' now depends on '{name}', which the \
running project does not declare; restart the project to apply structural changes"
                        )),
                    ))
                    .into());
                }
            }
        }
        adopted.service_start_order().map_err(|err| {
            ProcessManagerError::Diag(Box::new(crate::restart::manifest_rejected(
                err.to_string(),
            )))
        })?;
        Ok(())
    }

    /// Brings the spawn manager in line with the adopted service definition:
    /// new limits replace the old tree, and a definition that dropped its
    /// limits — or its dynamic mode — retires the tree instead of leaving the
    /// stale authorization behind.
    fn sync_adopted_spawn_limits(
        spawn_manager: &DynamicSpawnManager,
        adopted: &Config,
        service: &str,
    ) -> Result<(), SupervisorError> {
        let spawn = adopted
            .services
            .get(service)
            .and_then(|declared| declared.spawn.as_ref());
        let project = &adopted.project.id;
        match spawn.filter(|spawn| matches!(spawn.mode, Some(SpawnMode::Dynamic))) {
            Some(spawn) => {
                let limits = spawn.limits.clone().unwrap_or_default();
                spawn_manager.register_service(project, service, &limits)?
            }
            None => spawn_manager.unregister_service(project, service),
        }
        Ok(())
    }

    /// Respawns the primary metrics collector so it samples with the current
    /// config. The collector captures its `Arc<Config>` at spawn, so an
    /// adoption that changed a unit's hash would otherwise leave it sampling
    /// under the old identity. Deliberately does NOT touch the daemon monitor:
    /// shutting that down without a path that respawns it is how services end
    /// up unmanaged.
    fn respawn_metrics_collector(&mut self) -> Result<(), SupervisorError> {
        if let Some(collector) = self.metrics_collector.take() {
            collector.stop();
        }
        self.metrics_collector = Some(MetricsCollector::spawn(
            self.metrics_store.clone(),
            self.daemon.config(),
            self.daemon.pid_file_handle(),
            self.daemon.service_state_handle(),
        )?);
        Ok(())
    }

    /// The live config with only `service`'s declaration replaced by its
    /// definition in `manifest`, or `None` when the two already agree.
    ///
    /// A targeted restart adopts the target's own changed config on the bounce,
    /// and nothing else: the selector decides the blast radius, the manifest
    /// only supplies the target's definition. Adopting the rest of the manifest
    /// here is what silently escalated `restart -s X` into a project-wide
    /// reconcile whenever anything on disk had drifted.
    fn adopt_service_config(
        live: &Config,
        manifest: &Config,
        service: &str,
    ) -> Option<Config> {
        let declared = manifest.services.get(service)?;
        let changed = live
            .services
            .get(service)
            .is_none_or(|running| running.compute_hash() != declared.compute_hash());
        changed.then(|| {
            let mut adopted = live.clone();
            adopted
                .services
                .insert(service.to_string(), declared.clone());
            adopted
        })
    }

    /// Stops one service in the selected project without touching unrelated projects.
    fn stop_single_service_target(
        &self,
        selector: &str,
        project: Option<&str>,
    ) -> Result<(String, String), SupervisorError> {
        let (selector_project, service_name) = split_project_selector(selector)
            .map(|(project_id, service_name)| (Some(project_id), service_name))
            .unwrap_or((None, selector));

        // A stop that names a service no project declares is a false success
        // waiting to happen — refuse it with a typed diagnostic (SG0202).
        let known = match project.or(selector_project) {
            Some(project_id) => {
                let in_primary = self.daemon.config().project.id == project_id
                    && self.daemon.config().services.contains_key(service_name);
                let in_extra =
                    self.extra_projects.get(project_id).is_some_and(|runtime| {
                        runtime.daemon.config().services.contains_key(service_name)
                    });
                in_primary || in_extra
            }
            None => !self.projects_containing_service(service_name).is_empty(),
        };
        if !known {
            return Err(ProcessManagerError::Diag(Box::new(
                crate::stop::service_not_found(service_name),
            ))
            .into());
        }

        let target_project = self.resolve_service_target_project(
            service_name,
            project,
            selector_project,
            None,
        )?;
        let primary_project = self.daemon.config().project.id.clone();

        if target_project == primary_project {
            Self::stop_watched(
                &self.spawn_manager,
                &self.daemon,
                &target_project,
                service_name,
            )?;
            return Ok((target_project, service_name.to_string()));
        }

        let Some(project_runtime) = self.extra_projects.get(&target_project) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{target_project}' is not managed by this supervisor"),
            )
            .into());
        };

        if !project_runtime
            .daemon
            .config()
            .services
            .contains_key(service_name)
        {
            return Err(ProcessManagerError::DependencyError {
                service: service_name.into(),
                dependency: "service not defined".into(),
            }
            .into());
        }

        Self::stop_watched(
            &self.spawn_manager,
            &project_runtime.daemon,
            &target_project,
            service_name,
        )?;
        Ok((target_project, service_name.to_string()))
    }

    /// Stops one service, reporting it to whoever is watching the operation.
    ///
    /// The frames are emitted here rather than inside `stop_service` for the
    /// reason the bulk path documents: a restart stops each unit before
    /// starting it, and instrumenting the inner call would resolve a unit ✔ the
    /// moment it went down.
    fn stop_watched(
        spawn_manager: &DynamicSpawnManager,
        daemon: &Daemon,
        project: &str,
        service: &str,
    ) -> Result<(), SupervisorError> {
        daemon.note_unit_starting(service);
        // Read the generation's pid before the stop clears it; afterwards there
        // is nothing left to tie the tracked children to this run of the unit.
        let root_pid = daemon
            .pid_file_handle()
            .lock()
            .ok()
            .and_then(|pid_file| pid_file.services().get(service).copied());
        let result = daemon.stop_service(service);
        // Only reclaim children when the parent is actually down: a failed stop
        // leaves the unit running, and its workers with it.
        if let (Ok(()), Some(root_pid)) = (&result, root_pid) {
            Self::sweep_dynamic_children(
                spawn_manager,
                daemon,
                project,
                service,
                root_pid,
            );
        }
        match result {
            Ok(()) => {
                daemon.note_unit_done(service, start::Outcome::Stopped);
                Ok(())
            }
            Err(err) => {
                let err = SupervisorError::from(err);
                daemon.note_unit_done(service, Self::cascade_failure(service, &err));
                Err(err)
            }
        }
    }

    /// Reclaims the dynamic children one generation of a unit spawned.
    ///
    /// The ordinary teardown sweep cannot reach them: they are forked by the
    /// supervisor, so they are not descendants of the service pid and — before
    /// they were given their own session — sat in the supervisor's own session
    /// and process group, which the sweep refuses to signal. Stopping a unit
    /// left every worker it had spawned running, and nothing but a full
    /// supervisor shutdown collected them.
    ///
    /// Scoped to `root_pid` so a rolling restart cannot kill the replacement's
    /// children, and honours the unit's `termination_policy`: `orphan` and
    /// `reparent` mean the children are meant to outlive the parent, so they are
    /// only dropped from tracking.
    fn sweep_dynamic_children(
        spawn_manager: &DynamicSpawnManager,
        daemon: &Daemon,
        project: &str,
        service: &str,
        root_pid: u32,
    ) {
        let orphaned = spawn_manager.take_generation_children(project, service, root_pid);
        if orphaned.is_empty() {
            return;
        }
        let policy = spawn_manager.policy_for_unit(project, service);
        let cascade = matches!(policy, TerminationPolicy::Cascade);

        for child in &orphaned {
            if cascade
                && let Err(err) =
                    Daemon::terminate_process_tree(&child.name, child.pid, None)
            {
                warn!(
                    "Failed to terminate dynamic child '{}' (pid {}) of '{service}': {err}",
                    child.name, child.pid
                );
                continue;
            }
            if let Ok(mut pid_file) = daemon.pid_file_handle().lock()
                && let Err(err) = pid_file.remove_spawn(child.pid)
            {
                warn!(
                    "Failed to clear spawn row for pid {} of '{service}': {err}",
                    child.pid
                );
            }
        }
        if cascade {
            info!(
                "Reclaimed {} dynamic child process(es) of '{service}'",
                orphaned.len()
            );
        }
    }

    /// Handles refresh status cache.
    fn refresh_status_cache(&mut self) {
        match self.collect_aggregate_snapshot(false) {
            Ok(snapshot) => self.status_cache.replace(snapshot),
            Err(err) => error!("failed to refresh status snapshot: {err}"),
        }
    }

    /// Publishes the post-boot snapshot, returning why it could not be served.
    ///
    /// Retries briefly so a state file caught mid-rewrite does not leave the
    /// cache holding the pre-boot world for a whole refresh interval, which is
    /// what made a freshly started project read back as stopped with no pid.
    /// A poisoned lock will not clear on retry; it fails out to the caller.
    fn publish_boot_snapshot(&mut self) -> Result<(), SupervisorError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.collect_aggregate_snapshot(false) {
                Ok(snapshot) => {
                    self.status_cache.replace(snapshot);
                    return Ok(());
                }
                Err(err) if attempt == BOOT_SNAPSHOT_ATTEMPTS => return Err(err),
                Err(err) => {
                    warn!("post-boot status snapshot failed ({err}); retrying");
                    thread::sleep(BOOT_SNAPSHOT_RETRY_DELAY);
                }
            }
        }
    }

    /// (Re)starts the background status refresher over EVERY managed project.
    ///
    /// The refresher is what keeps the served (cached) snapshot honest: without a
    /// live loop, a cache seeded before an async project boot recorded its PIDs
    /// would report running services as `stopped` forever. Adding a project or
    /// reloading the config must therefore re-spawn this — never leave it dead.
    fn respawn_status_refresher(&mut self) -> Result<(), SupervisorError> {
        if let Some(refresher) = self.status_refresher.take() {
            refresher.stop();
        }

        let refresh_mode = Self::status_snapshot_mode(self.daemon.config().as_ref());
        if matches!(refresh_mode, StatusSnapshotMode::Off) {
            return Ok(());
        }

        let cache_clone = self.status_cache.clone();
        let refresh_interval =
            Self::status_snapshot_interval(self.daemon.config().as_ref());
        let refresh_projects = Arc::clone(&self.cron_projects);
        let refresh_metrics = self.metrics_store.clone();
        let refresh_spawn = self.spawn_manager.clone();
        self.status_refresher = Some(StatusRefresher::spawn(
            cache_clone,
            refresh_interval,
            move || {
                Supervisor::collect_projects_snapshot(
                    &refresh_projects,
                    &refresh_metrics,
                    &refresh_spawn,
                    refresh_mode,
                )
            },
        )?);
        Ok(())
    }

    /// Records every dynamic unit's current generation pid before a project-wide
    /// stop, so its children can be reclaimed once the services are down.
    fn dynamic_generations(daemon: &Daemon) -> Vec<(String, u32)> {
        let config = daemon.config();
        let handle = daemon.pid_file_handle();
        let Ok(pid_file) = handle.lock() else {
            return Vec::new();
        };
        config
            .services
            .iter()
            .filter(|(_, service)| {
                service
                    .spawn
                    .as_ref()
                    .is_some_and(|spawn| matches!(spawn.mode, Some(SpawnMode::Dynamic)))
            })
            .filter_map(|(name, _)| {
                pid_file
                    .services()
                    .get(name)
                    .map(|pid| (name.clone(), *pid))
            })
            .collect()
    }

    /// Stops every service in one managed project.
    fn stop_project(&mut self, project_id: &str) -> Result<(), SupervisorError> {
        let primary_project = self.daemon.config().project.id.clone();
        if project_id == primary_project {
            self.daemon.cancel_boot();
            self.cron_manager.remove_project_jobs(project_id);
            self.daemon.shutdown_monitor();
            let generations = Self::dynamic_generations(&self.daemon);
            let stop_result = self.daemon.stop_services();
            for (service, root_pid) in generations {
                Self::sweep_dynamic_children(
                    &self.spawn_manager,
                    &self.daemon,
                    project_id,
                    &service,
                    root_pid,
                );
            }
            if let Err(err) = stop_result {
                self.daemon.begin_boot();
                let _ = self.daemon.ensure_monitoring();
                let _ = self.sync_cron_projects();
                return Err(err.into());
            }
            self.primary_active = false;
            self.sync_cron_projects()?;
            return Ok(());
        }

        let Some(project) = self.extra_projects.get(project_id) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' is not managed by this supervisor"),
            )
            .into());
        };
        project.daemon.cancel_boot();
        self.cron_manager.remove_project_jobs(project_id);
        project.daemon.shutdown_monitor();
        let generations = Self::dynamic_generations(&project.daemon);
        let stop_result = project.daemon.stop_services();
        for (service, root_pid) in generations {
            Self::sweep_dynamic_children(
                &self.spawn_manager,
                &project.daemon,
                project_id,
                &service,
                root_pid,
            );
        }
        if let Err(err) = stop_result {
            project.daemon.begin_boot();
            let _ = project.daemon.ensure_monitoring();
            let _ = self.sync_cron_projects();
            return Err(err.into());
        }
        self.extra_projects.remove(project_id);
        crate::logs::clear_project_live_logs(project_id);
        if let Ok(mut projects) = self.boot_projects.write() {
            projects.remove(project_id);
        }
        self.sync_cron_projects()?;
        Ok(())
    }

    /// Stops every service in every project managed by the supervisor.
    fn stop_all_projects(&mut self) -> Result<(), SupervisorError> {
        // Best-effort, for the same reason as `shutdown_runtime`: one project
        // that fails to stop must not leave every project after it — and the
        // primary — still running while the command reports it stopped
        // everything.
        let extra_projects: Vec<String> = self.extra_projects.keys().cloned().collect();
        let mut first_error: Option<SupervisorError> = None;
        for project_id in extra_projects {
            if let Err(err) = self.stop_project(&project_id) {
                error!("Failed to stop project '{project_id}': {err}");
                first_error.get_or_insert(err);
            }
        }

        let primary_project = self.daemon.config().project.id.clone();
        if let Err(err) = self.stop_project(&primary_project) {
            error!("Failed to stop the primary project's services: {err}");
            first_error.get_or_insert(err);
        }
        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Collects a fresh status snapshot using each project's configured snapshot mode.
    fn collect_configured_snapshot(&self) -> Result<StatusSnapshot, SupervisorError> {
        self.collect_aggregate_snapshot(false)
    }

    /// Collects a fresh status snapshot with immediate runtime collection enabled.
    fn collect_live_snapshot_for_request(
        &self,
    ) -> Result<StatusSnapshot, SupervisorError> {
        self.collect_aggregate_snapshot(true)
    }

    /// Handles shutdown runtime.
    fn shutdown_runtime(&mut self) -> Result<(), SupervisorError> {
        if let Some(collector) = self.metrics_collector.take() {
            collector.stop();
        }
        if let Some(refresher) = self.status_refresher.take() {
            refresher.stop();
        }
        self.cron_manager.clear_all_jobs();
        for project in self.extra_projects.values() {
            project.daemon.cancel_boot();
            project.daemon.shutdown_monitor();
        }
        self.daemon.cancel_boot();
        self.daemon.shutdown_monitor();
        // Shutdown is BEST-EFFORT across every project. A `?` here aborted the
        // whole teardown on the first project that failed to stop cleanly —
        // which is exactly what a concurrent restart provokes, since the pid
        // file is being rewritten underneath. Every project after it, AND the
        // primary, were then never stopped: `stop --supervisor` reported
        // "Supervisor shutting down" with rc=0 while leaving orphaned service
        // processes running. Reap everything first, then surface the failure.
        let mut teardown_error: Option<SupervisorError> = None;
        for (project_id, project) in &self.extra_projects {
            if let Err(err) = project.daemon.stop_services() {
                error!("Failed to stop services in project '{project_id}': {err}");
                teardown_error.get_or_insert(err.into());
            }
        }
        if let Err(err) = self.daemon.stop_services() {
            error!("Failed to stop the primary project's services: {err}");
            teardown_error.get_or_insert(err.into());
        }
        ipc::cleanup_runtime_owned(std::process::id() as libc::pid_t)?;
        if let Some(err) = teardown_error {
            return Err(err);
        }
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    }

    /// Get all registered cron jobs (for testing).
    pub fn get_cron_jobs(&self) -> Vec<crate::cron::CronJobState> {
        self.cron_manager.get_all_jobs()
    }

    /// Reload config for testing.
    pub fn reload_config_for_test(
        &mut self,
        path: &std::path::Path,
    ) -> Result<(), SupervisorError> {
        self.reload_config(path)
    }

    /// Shutdown for testing.
    pub fn shutdown_for_test(&mut self) -> Result<(), SupervisorError> {
        self.shutdown_runtime()
    }

    /// Get the last execution status for a cron job (for testing).
    pub fn get_last_cron_execution_status(
        &self,
        job_name: &str,
    ) -> Option<CronExecutionStatus> {
        self.cron_manager.get_last_execution_status(job_name)
    }

    /// Get the cron manager for testing.
    pub fn get_cron_manager_for_test(&self) -> CronManager {
        self.cron_manager.clone()
    }

    /// Waits for a cron job process to complete and returns the final outcome.
    /// Maps an exit status reaped elsewhere onto this run's outcome.
    fn cron_outcome_from_status(
        job_name: &str,
        status: std::process::ExitStatus,
    ) -> CronCompletionOutcome {
        if let Some(signal) = status.signal() {
            warn!(
                "Cron job '{}' was terminated by signal {}",
                job_name, signal
            );
            return CronCompletionOutcome {
                status: CronExecutionStatus::Failed(format!(
                    "Terminated by signal {signal}"
                )),
                exit_code: None,
            };
        }
        match status.code() {
            Some(0) => CronCompletionOutcome {
                status: CronExecutionStatus::Success,
                exit_code: Some(0),
            },
            Some(code) => {
                debug!("Cron job '{}' exited with code {}", job_name, code);
                CronCompletionOutcome {
                    status: CronExecutionStatus::Failed(format!(
                        "Process exited with code {code}"
                    )),
                    exit_code: Some(code),
                }
            }
            None => CronCompletionOutcome {
                status: CronExecutionStatus::Failed(
                    "Process exited without reporting a status".to_string(),
                ),
                exit_code: None,
            },
        }
    }

    fn wait_for_cron_completion(
        pid: u32,
        job_name: &str,
        claim_key: &str,
    ) -> Result<CronCompletionOutcome, SupervisorError> {
        Self::wait_for_cron_completion_with_timeout(
            pid,
            job_name,
            claim_key,
            Duration::from_secs(3600),
            Duration::from_millis(100),
        )
    }

    /// Waits for one cron run to finish. `claim_key` is the unit's state key:
    /// the address a routed exit status carries, unique across projects where a
    /// service name is not.
    fn wait_for_cron_completion_with_timeout(
        pid: u32,
        job_name: &str,
        claim_key: &str,
        max_wait_time: Duration,
        poll_interval: Duration,
    ) -> Result<CronCompletionOutcome, SupervisorError> {
        use nix::{
            sys::wait::{WaitPidFlag, WaitStatus, waitpid},
            unistd::Pid,
        };

        let wait_pid = Pid::from_raw(pid as i32);
        let start = std::time::Instant::now();

        loop {
            // The monitor reaps managed children too, and on Linux a pidfd
            // wakes it the instant one exits. When it gets there first it
            // routes the status here rather than leaving this thread to guess.
            if let Some(status) = crate::reaper::take_for(pid as i32, claim_key) {
                return Ok(Self::cron_outcome_from_status(job_name, status));
            }
            match waitpid(wait_pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {
                    if start.elapsed() > max_wait_time {
                        warn!(
                            "Cron job '{}' exceeded maximum wait time of {} seconds; terminating process tree",
                            job_name,
                            max_wait_time.as_secs()
                        );
                        Daemon::terminate_process_tree(job_name, pid, None)?;
                        return Ok(CronCompletionOutcome {
                            status: CronExecutionStatus::Failed(format!(
                                "Cron job exceeded maximum wait time of {} seconds",
                                max_wait_time.as_secs()
                            )),
                            exit_code: None,
                        });
                    }

                    thread::sleep(poll_interval);
                }
                Ok(WaitStatus::Exited(_, exit_code)) => {
                    debug!("Cron job '{}' exited with code {}", job_name, exit_code);
                    let status = if exit_code == 0 {
                        CronExecutionStatus::Success
                    } else {
                        CronExecutionStatus::Failed(format!(
                            "Process exited with code {exit_code}"
                        ))
                    };
                    return Ok(CronCompletionOutcome {
                        status,
                        exit_code: Some(exit_code),
                    });
                }
                Ok(WaitStatus::Signaled(_, signal, _)) => {
                    warn!(
                        "Cron job '{}' was terminated by signal {:?}",
                        job_name, signal
                    );
                    return Ok(CronCompletionOutcome {
                        status: CronExecutionStatus::Failed(format!(
                            "Terminated by signal {signal}"
                        )),
                        exit_code: None,
                    });
                }
                Ok(WaitStatus::Stopped(..)) | Ok(WaitStatus::Continued(_)) => {
                    thread::sleep(poll_interval);
                }
                #[cfg(any(target_os = "linux", target_os = "android"))]
                Ok(WaitStatus::PtraceEvent(_, _, _))
                | Ok(WaitStatus::PtraceSyscall(_)) => {
                    thread::sleep(poll_interval);
                }
                Err(nix::errno::Errno::ECHILD) => {
                    if let Some(status) = crate::reaper::take_for(pid as i32, claim_key) {
                        return Ok(Self::cron_outcome_from_status(job_name, status));
                    }
                    warn!(
                        "Cron job '{}' was reaped without routing its exit status; recording the run as interrupted",
                        job_name
                    );
                    return Ok(CronCompletionOutcome {
                        status: CronExecutionStatus::Interrupted(
                            CRON_STATUS_LOST_REASON.to_string(),
                        ),
                        exit_code: None,
                    });
                }
                Err(e) => {
                    error!("Error waiting for cron job '{}': {}", job_name, e);
                    return Err(SupervisorError::Process(
                        ProcessManagerError::ServiceStartError {
                            service: job_name.to_string(),
                            source: std::io::Error::from_raw_os_error(e as i32),
                        },
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs};

    use chrono::Utc;
    use tempfile::tempdir_in;

    use super::*;
    use crate::{
        config::{
            LogsConfig, MetricsConfig, ProjectConfig, ServiceConfig, StatusConfig,
            Version,
        },
        runtime,
        status::{
            OverallHealth, UnitHealth, UnitIntent, UnitKind, UnitState, UnitStatus,
        },
    };

    #[test]
    fn op_id_is_the_clients_nonce_not_a_derived_key() {
        assert_eq!(
            Supervisor::op_id(&ControlCommand::Start {
                service: Some("api".into()),
                project: Some("web".into()),
                watch: Some("pid-42-7".into()),
            }),
            Some("pid-42-7".to_string())
        );
        // Two identical commands are distinct operations, so an unstamped one
        // is unwatched rather than sharing the other's journal.
        assert_eq!(
            Supervisor::op_id(&ControlCommand::Start {
                service: Some("api".into()),
                project: Some("web".into()),
                watch: None,
            }),
            None
        );
    }

    #[test]
    fn op_project_reads_the_project_out_of_a_selector() {
        assert_eq!(
            Supervisor::op_project(&ControlCommand::Restart {
                config: None,
                service: Some("web/api".into()),
                project: None,
                watch: None,
            }),
            Some("web")
        );
        // An explicit project wins over the selector's.
        assert_eq!(
            Supervisor::op_project(&ControlCommand::Stop {
                service: Some("web/api".into()),
                project: Some("other".into()),
                watch: None,
            }),
            Some("other")
        );
        // A bare service names no project, so the op is not scoped to one.
        assert_eq!(
            Supervisor::op_project(&ControlCommand::Stop {
                service: Some("api".into()),
                project: None,
                watch: None,
            }),
            None
        );
    }

    #[test]
    fn reconcile_failures_reports_only_real_failures() {
        let failure = crate::daemon::RestartFailure {
            cause: ProcessManagerError::ServiceStartError {
                service: "gamecast_draftkings_ingest".into(),
                source: std::io::Error::new(io::ErrorKind::TimedOut, "timed out"),
            },
            failed_services: Some(vec!["gamecast_draftkings_ingest".into()]),
        };

        assert_eq!(
            Supervisor::reconcile_failures(&failure),
            Some(vec!["gamecast_draftkings_ingest".to_string()])
        );
    }

    #[test]
    fn reconcile_failures_returns_none_for_unattributable_failures() {
        let failure = crate::daemon::RestartFailure {
            cause: ProcessManagerError::ServiceStartError {
                service: "monitor".into(),
                source: std::io::Error::other("monitor thread failed to spawn"),
            },
            failed_services: None,
        };

        assert_eq!(Supervisor::reconcile_failures(&failure), None);
    }

    fn test_service(depends_on: &[&str]) -> ServiceConfig {
        ServiceConfig {
            command: "/bin/true".into(),
            depends_on: if depends_on.is_empty() {
                None
            } else {
                Some(
                    depends_on
                        .iter()
                        .map(|dep| crate::config::DependsOn::from(*dep))
                        .collect(),
                )
            },
            ..ServiceConfig::default()
        }
    }

    #[test]
    fn supervisor_startup_order_honors_dependencies() {
        let mut services = HashMap::new();
        services.insert("worker".into(), test_service(&["beacon"]));
        services.insert("server".into(), test_service(&["worker"]));
        services.insert("beacon".into(), test_service(&[]));

        let config = Config {
            version: Version::V2,
            project: ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: MetricsConfig::default(),
            logs: LogsConfig::default(),
            status: StatusConfig::default(),
        };

        let order = Supervisor::startup_service_order(&config, None).unwrap();

        assert_eq!(order, vec!["beacon", "worker", "server"]);
    }

    #[test]
    fn supervisor_startup_order_applies_service_filter_after_sorting() {
        let mut services = HashMap::new();
        services.insert("worker".into(), test_service(&["beacon"]));
        services.insert("beacon".into(), test_service(&[]));

        let config = Config {
            version: Version::V2,
            project: ProjectConfig::default(),
            services,
            project_dir: None,
            env: None,
            metrics: MetricsConfig::default(),
            logs: LogsConfig::default(),
            status: StatusConfig::default(),
        };

        let order = Supervisor::startup_service_order(&config, Some("worker")).unwrap();

        assert_eq!(order, vec!["worker"]);
    }

    #[test]
    fn cron_completion_timeout_terminates_process_tree() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 10"])
            .spawn()
            .expect("spawn sleeping cron process");
        let pid = child.id();

        let outcome = Supervisor::wait_for_cron_completion_with_timeout(
            pid,
            "slow-cron",
            "v2:demo:slow-cron",
            Duration::from_millis(1),
            Duration::from_millis(1),
        )
        .expect("timeout should terminate process tree and return failed outcome");

        assert!(matches!(
            outcome.status,
            CronExecutionStatus::Failed(ref reason)
                if reason.contains("exceeded maximum wait time")
        ));
        assert_eq!(outcome.exit_code, None);
        match child.try_wait() {
            Ok(Some(_)) => {}
            Err(err) if err.raw_os_error() == Some(libc::ECHILD) => {}
            Ok(None) => {
                let _ = child.kill();
                panic!("timed-out cron process should not remain running");
            }
            Err(err) => panic!("failed to inspect timed-out cron process: {err}"),
        }
    }

    #[test]
    fn cron_completion_reads_a_status_the_monitor_reaped_first() {
        // A pid this thread never parented: `waitpid` can only answer ECHILD,
        // so the outcome has to come from the routed status — exactly the
        // position the cron thread is in when the monitor wins the race.
        let pid = 4_000_001u32;
        crate::reaper::drop_claims("v2:demo:routed-cron");
        crate::reaper::publish(
            pid as i32,
            "v2:demo:routed-cron",
            std::process::ExitStatus::from_raw(1024),
        );

        let outcome = Supervisor::wait_for_cron_completion_with_timeout(
            pid,
            "routed-cron",
            "v2:demo:routed-cron",
            Duration::from_millis(50),
            Duration::from_millis(1),
        )
        .expect("a routed status resolves the run");

        assert!(matches!(
            outcome.status,
            CronExecutionStatus::Failed(ref reason) if reason.contains("code 4")
        ));
        assert_eq!(outcome.exit_code, Some(4));
    }

    #[test]
    fn cron_completion_never_invents_success_for_a_lost_status() {
        let pid = 4_000_002u32;
        crate::reaper::drop_claims("v2:demo:lost-cron");

        let outcome = Supervisor::wait_for_cron_completion_with_timeout(
            pid,
            "lost-cron",
            "v2:demo:lost-cron",
            Duration::from_millis(50),
            Duration::from_millis(1),
        )
        .expect("a lost status still resolves the run");

        assert!(matches!(
            outcome.status,
            CronExecutionStatus::Interrupted(_)
        ));
        assert_eq!(outcome.exit_code, None);
    }

    #[test]
    fn a_signalled_cron_run_is_a_failure() {
        let outcome = Supervisor::cron_outcome_from_status(
            "killed-cron",
            std::process::ExitStatus::from_raw(9),
        );

        assert!(matches!(
            outcome.status,
            CronExecutionStatus::Failed(ref reason) if reason.contains("signal")
        ));
        assert_eq!(outcome.exit_code, None);
    }

    #[test]
    fn daemon_for_project_resolves_only_its_own_project() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let config_path = temp.path().join("systemg.yaml");
        fs::write(
            &config_path,
            r#"
version: "2"
services:
  api:
    command: "/bin/true"
"#,
        )
        .expect("write config");

        let supervisor =
            Supervisor::new(config_path, false, None).expect("create supervisor");

        let project = supervisor.daemon.config().project.id.clone();
        let owner = supervisor
            .daemon_for_project(&project)
            .expect("the primary project resolves to a daemon");
        assert_eq!(
            owner
                .config()
                .services
                .get("api")
                .map(|c| c.command.clone()),
            Some("/bin/true".to_string())
        );
        assert!(
            supervisor.daemon_for_project("no-such-project").is_none(),
            "an unmanaged project must not fall back to the primary daemon"
        );

        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);
    }

    #[test]
    fn status_and_inspect_commands_refresh_configured_snapshot() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let config_path = temp.path().join("systemg.yaml");
        fs::write(
            &config_path,
            r#"
version: "2"
status:
  snapshot_mode: summary
services:
  cached:
    command: "/bin/true"
"#,
        )
        .expect("write config");

        let mut supervisor =
            Supervisor::new(config_path, false, None).expect("create supervisor");
        let cached_unit = UnitStatus {
            name: "cached".into(),
            hash: "cached-hash".into(),
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
            command: Some("/bin/true".into()),
            runtime_command: None,
            spawned_children: Vec::new(),
        };
        supervisor.status_cache.replace(StatusSnapshot {
            schema_version: crate::status::STATUS_SCHEMA_VERSION.into(),
            captured_at: Utc::now(),
            overall_health: OverallHealth::Healthy,
            units: vec![cached_unit],
        });

        match supervisor
            .handle_command(ControlCommand::Status { live: false })
            .expect("status response")
        {
            ControlResponse::Status(snapshot) => {
                assert_eq!(snapshot.units.len(), 1);
                assert_eq!(snapshot.units[0].name, "cached");
                assert_ne!(snapshot.units[0].hash, "cached-hash");
            }
            other => panic!("expected status response, got {other:?}"),
        }

        match supervisor
            .handle_command(ControlCommand::Inspect {
                unit: "cached".into(),
                project: None,
                samples: 10,
                live: false,
            })
            .expect("inspect response")
        {
            ControlResponse::Inspect(payload) => {
                assert_eq!(
                    payload.unit.as_ref().map(|unit| unit.name.as_str()),
                    Some("cached")
                );
                assert_ne!(
                    payload.unit.as_ref().map(|unit| unit.hash.as_str()),
                    Some("cached-hash")
                );
            }
            other => panic!("expected inspect response, got {other:?}"),
        }

        match supervisor
            .handle_command(ControlCommand::Status { live: true })
            .expect("live status response")
        {
            ControlResponse::Status(snapshot) => {
                assert_eq!(snapshot.units.len(), 1);
                assert_eq!(snapshot.units[0].name, "cached");
                assert_ne!(snapshot.units[0].hash, "cached-hash");
            }
            other => panic!("expected status response, got {other:?}"),
        }

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn add_project_config_makes_second_project_visible_in_status() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let alpha_config = temp.path().join("alpha.yaml");
        let beta_config = temp.path().join("beta.yaml");
        let beta_updated_config = temp.path().join("beta-updated.yaml");
        fs::write(
            &alpha_config,
            r#"
version: "2"
project:
  id: alpha
  name: Alpha
services:
  alpha_worker:
    command: "/bin/sleep 31"
"#,
        )
        .expect("write alpha config");
        fs::write(
            &beta_config,
            r#"
version: "2"
project:
  id: beta
  name: Beta
services:
  beta_worker:
    command: "/bin/sleep 32"
  beta_cron:
    command: "/bin/echo beta"
    cron:
      expression: "*/30 * * * *"
"#,
        )
        .expect("write beta config");
        fs::write(
            &beta_updated_config,
            r#"
version: "2"
project:
  id: beta
  name: Beta Updated
services:
  beta_worker:
    command: "/bin/sleep 33"
"#,
        )
        .expect("write updated beta config");

        let mut supervisor = Supervisor::new(alpha_config.clone(), false, None)
            .expect("create supervisor");
        supervisor
            .handle_command(ControlCommand::AddProject {
                config: beta_config.to_string_lossy().to_string(),
                service: None,
                mode: ProjectRunMode::Foreground,
                watch: None,
            })
            .expect("add beta project");

        match supervisor
            .handle_command(ControlCommand::Status { live: true })
            .expect("status response")
        {
            ControlResponse::Status(snapshot) => {
                let projects: std::collections::HashSet<_> = snapshot
                    .units
                    .iter()
                    .filter_map(|unit| {
                        unit.project.as_ref().map(|project| project.id.as_str())
                    })
                    .collect();
                assert!(
                    projects.contains("alpha"),
                    "alpha project missing from status"
                );
                assert!(
                    projects.contains("beta"),
                    "beta project missing from status"
                );
                assert!(
                    snapshot
                        .units
                        .iter()
                        .any(|unit| unit.name == "alpha_worker"),
                    "alpha service missing from status"
                );
                assert!(
                    snapshot.units.iter().any(|unit| unit.name == "beta_worker"),
                    "beta service missing from status"
                );
                let alpha_mode = snapshot
                    .units
                    .iter()
                    .find(|unit| unit.name == "alpha_worker")
                    .and_then(|unit| unit.project.as_ref())
                    .map(|project| project.mode);
                assert_eq!(alpha_mode, Some(ProjectRunMode::Daemon));
                let alpha_config_path = snapshot
                    .units
                    .iter()
                    .find(|unit| unit.name == "alpha_worker")
                    .and_then(|unit| unit.project.as_ref())
                    .and_then(|project| project.config_path.as_deref());
                assert_eq!(
                    alpha_config_path,
                    Some(alpha_config.to_string_lossy().as_ref())
                );
                let beta_mode = snapshot
                    .units
                    .iter()
                    .find(|unit| {
                        unit.name == "beta_worker"
                            && unit.project.as_ref().map(|project| project.id.as_str())
                                == Some("beta")
                    })
                    .and_then(|unit| unit.project.as_ref())
                    .map(|project| project.mode);
                assert_eq!(beta_mode, Some(ProjectRunMode::Foreground));
                let beta_config_path = snapshot
                    .units
                    .iter()
                    .find(|unit| {
                        unit.name == "beta_worker"
                            && unit.project.as_ref().map(|project| project.id.as_str())
                                == Some("beta")
                    })
                    .and_then(|unit| unit.project.as_ref())
                    .and_then(|project| project.config_path.as_deref());
                assert_eq!(
                    beta_config_path,
                    Some(beta_config.to_string_lossy().as_ref())
                );
            }
            other => panic!("expected status response, got {other:?}"),
        }

        let err = supervisor
            .handle_command(ControlCommand::Start {
                service: Some("beta_cron".into()),
                project: Some("beta".into()),
                watch: None,
            })
            .expect_err("direct cron unit start should be rejected");
        assert!(matches!(
            err,
            SupervisorError::Process(ProcessManagerError::Diag(diag))
                if diag.code == crate::diag::SgCode::CronDirectControl
        ));

        let restart_err = supervisor
            .handle_command(ControlCommand::Restart {
                config: None,
                service: Some("beta_cron".into()),
                project: Some("beta".into()),
                watch: None,
            })
            .expect_err("direct cron unit restart should be rejected");
        assert!(matches!(
            restart_err,
            SupervisorError::Process(ProcessManagerError::Diag(diag))
                if diag.code == crate::diag::SgCode::CronDirectControl
        ));

        supervisor
            .handle_command(ControlCommand::Restart {
                config: Some(beta_config.to_string_lossy().to_string()),
                service: Some("beta_worker".into()),
                project: None,
                watch: None,
            })
            .expect("restart beta service from beta config");

        match supervisor
            .handle_command(ControlCommand::Status { live: true })
            .expect("status response after project-scoped restart")
        {
            ControlResponse::Status(snapshot) => {
                assert!(
                    snapshot.units.iter().any(|unit| {
                        unit.name == "alpha_worker"
                            && unit.project.as_ref().map(|project| project.id.as_str())
                                == Some("alpha")
                    }),
                    "alpha project should remain visible after restarting beta service"
                );
                assert!(
                    snapshot.units.iter().any(|unit| {
                        unit.name == "beta_worker"
                            && unit.project.as_ref().map(|project| project.id.as_str())
                                == Some("beta")
                    }),
                    "beta project should remain visible after restarting beta service"
                );
            }
            other => panic!("expected status response, got {other:?}"),
        }

        supervisor
            .handle_command(ControlCommand::Restart {
                config: Some(beta_updated_config.to_string_lossy().to_string()),
                service: None,
                project: Some("beta".into()),
                watch: None,
            })
            .expect("restart beta project from updated config");

        let beta_runtime = supervisor
            .extra_projects
            .get("beta")
            .expect("beta runtime after project restart");
        assert_eq!(beta_runtime.daemon.config().project.name, "Beta Updated");
        assert_eq!(
            beta_runtime
                .daemon
                .config()
                .services
                .get("beta_worker")
                .map(|service| service.command.as_str()),
            Some("/bin/sleep 33")
        );
        assert_eq!(
            beta_runtime.config_path,
            beta_updated_config
                .canonicalize()
                .unwrap_or_else(|_| beta_updated_config.clone())
        );

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn project_service_names(snapshot: &StatusSnapshot, project_id: &str) -> Vec<String> {
        snapshot
            .units
            .iter()
            .filter(|unit| {
                unit.project.as_ref().map(|project| project.id.as_str())
                    == Some(project_id)
            })
            .map(|unit| unit.name.clone())
            .collect()
    }

    #[test]
    fn restart_primary_project_without_config_reloads_stored_manifest() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let config_path = temp.path().join("primary.yaml");
        fs::write(
            &config_path,
            r#"
version: "2"
project:
  id: primary
services:
  alpha:
    command: "/bin/sleep 45"
  beta:
    command: "/bin/sleep 45"
"#,
        )
        .expect("write config");

        let mut supervisor =
            Supervisor::new(config_path.clone(), false, None).expect("create supervisor");

        fs::write(
            &config_path,
            r#"
version: "2"
project:
  id: primary
services:
  alpha:
    command: "/bin/sleep 60"
  gamma:
    command: "/bin/sleep 45"
"#,
        )
        .expect("rewrite config");

        supervisor
            .handle_command(ControlCommand::Restart {
                config: None,
                service: None,
                project: Some("primary".into()),
                watch: None,
            })
            .expect("restart primary project without config");

        match supervisor
            .handle_command(ControlCommand::Status { live: true })
            .expect("status after restart")
        {
            ControlResponse::Status(snapshot) => {
                let names = project_service_names(&snapshot, "primary");
                assert!(
                    names.contains(&"gamma".to_string()),
                    "added service missing"
                );
                assert!(
                    !names.contains(&"beta".to_string()),
                    "removed service lingered"
                );
                assert!(names.contains(&"alpha".to_string()), "kept service missing");
            }
            other => panic!("expected status response, got {other:?}"),
        }

        assert_eq!(
            supervisor
                .daemon
                .config()
                .services
                .get("alpha")
                .map(|service| service.command.as_str()),
            Some("/bin/sleep 60")
        );

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    /// Verifies a failed added unit leaves unchanged primary processes intact.
    fn primary_reconcile_failure_preserves_unchanged_processes() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let config_path = temp.path().join("primary.yaml");
        fs::write(
            &config_path,
            r#"
version: "2"
project:
  id: primary
services:
  web:
    command: "/bin/sleep 45"
  api:
    command: "/bin/sleep 45"
"#,
        )
        .expect("write config");

        let mut supervisor =
            Supervisor::new(config_path.clone(), false, None).expect("create supervisor");
        supervisor
            .daemon
            .start_services()
            .expect("start primary services");
        let before = supervisor
            .daemon
            .pid_file_handle()
            .lock()
            .expect("pid file lock")
            .services()
            .clone();

        fs::write(
            &config_path,
            r#"
version: "2"
project:
  id: primary
services:
  web:
    command: "/bin/sleep 45"
  api:
    command: "/bin/sleep 45"
  bad:
    command: "/bin/sh -c 'exit 1'"
"#,
        )
        .expect("rewrite config");

        let error = supervisor
            .handle_command(ControlCommand::Restart {
                config: Some(config_path.to_string_lossy().to_string()),
                service: None,
                project: Some("primary".into()),
                watch: None,
            })
            .expect_err("failing added service should make reconcile incomplete");
        assert!(
            error.to_string().contains("SG0302"),
            "unexpected reconcile error: {error}"
        );

        let after = supervisor
            .daemon
            .pid_file_handle()
            .lock()
            .expect("pid file lock")
            .services()
            .clone();
        for service in ["web", "api"] {
            let pid = before.get(service).copied().expect("original service pid");
            assert_eq!(after.get(service), Some(&pid));
            assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, 0);
        }
        assert!(supervisor.daemon.config().services.contains_key("bad"));

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn restart_extra_project_without_config_reloads_stored_manifest() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let alpha_config = temp.path().join("alpha.yaml");
        fs::write(
            &alpha_config,
            r#"
version: "2"
project:
  id: alpha
services:
  alpha_worker:
    command: "/bin/sleep 45"
"#,
        )
        .expect("write alpha config");

        let beta_config = temp.path().join("beta.yaml");
        fs::write(
            &beta_config,
            r#"
version: "2"
project:
  id: beta
services:
  beta_worker:
    command: "/bin/sleep 45"
  beta_legacy:
    command: "/bin/sleep 45"
"#,
        )
        .expect("write beta config");

        let mut supervisor =
            Supervisor::new(alpha_config, false, None).expect("create supervisor");
        supervisor
            .handle_command(ControlCommand::AddProject {
                config: beta_config.to_string_lossy().to_string(),
                service: None,
                mode: ProjectRunMode::Daemon,
                watch: None,
            })
            .expect("add beta project");

        fs::write(
            &beta_config,
            r#"
version: "2"
project:
  id: beta
services:
  beta_worker:
    command: "/bin/sleep 60"
  beta_added:
    command: "/bin/sleep 45"
"#,
        )
        .expect("rewrite beta config");

        supervisor
            .handle_command(ControlCommand::Restart {
                config: None,
                service: None,
                project: Some("beta".into()),
                watch: None,
            })
            .expect("restart beta project without config");

        match supervisor
            .handle_command(ControlCommand::Status { live: true })
            .expect("status after restart")
        {
            ControlResponse::Status(snapshot) => {
                let names = project_service_names(&snapshot, "beta");
                assert!(
                    names.contains(&"beta_added".to_string()),
                    "added service missing"
                );
                assert!(
                    !names.contains(&"beta_legacy".to_string()),
                    "removed service lingered"
                );
                assert!(
                    names.contains(&"beta_worker".to_string()),
                    "kept service missing"
                );
            }
            other => panic!("expected status response, got {other:?}"),
        }

        let beta_runtime = supervisor
            .extra_projects
            .get("beta")
            .expect("beta runtime after restart");
        assert_eq!(
            beta_runtime
                .daemon
                .config()
                .services
                .get("beta_worker")
                .map(|service| service.command.as_str()),
            Some("/bin/sleep 60")
        );

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    /// Verifies redundant primary registration preserves every service process.
    fn repro_redundant_add_project_bounces_primary() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let config_path = temp.path().join("demo.yaml");
        fs::write(
            &config_path,
            r#"
version: "2"
project:
  id: demo
services:
  one:
    command: "/bin/sleep 45"
  two:
    command: "/bin/sleep 45"
"#,
        )
        .expect("write config");

        let mut supervisor =
            Supervisor::new(config_path.clone(), false, None).expect("create supervisor");

        supervisor
            .daemon
            .start_services()
            .expect("boot primary services");

        let pids_before: BTreeMap<String, u32> = {
            let guard = supervisor.daemon.pid_file_handle();
            let locked = guard.lock().unwrap();
            locked
                .services()
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect()
        };

        supervisor
            .handle_command(ControlCommand::AddProject {
                config: config_path.to_string_lossy().to_string(),
                service: None,
                mode: ProjectRunMode::Daemon,
                watch: None,
            })
            .expect("redundant add of same primary config");
        let pids_after: BTreeMap<String, u32> = {
            let guard = supervisor.daemon.pid_file_handle();
            let locked = guard.lock().unwrap();
            locked
                .services()
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect()
        };

        assert_eq!(
            pids_before, pids_after,
            "redundant AddProject bounced the primary services"
        );

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn add_project_for_primary_reloads_manifest_after_stop() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let config_path = temp.path().join("primary.yaml");
        fs::write(
            &config_path,
            r#"
version: "2"
project:
  id: primary
services:
  alpha:
    command: "/bin/sleep 45"
"#,
        )
        .expect("write config");

        let mut supervisor =
            Supervisor::new(config_path.clone(), false, None).expect("create supervisor");

        supervisor
            .handle_command(ControlCommand::Stop {
                service: None,
                project: Some("primary".into()),
                watch: None,
            })
            .expect("stop primary project");

        fs::write(
            &config_path,
            r#"
version: "2"
project:
  id: primary
services:
  alpha:
    command: "/bin/sleep 60"
  delta:
    command: "/bin/sleep 45"
"#,
        )
        .expect("rewrite config");

        supervisor
            .handle_command(ControlCommand::AddProject {
                config: config_path.to_string_lossy().to_string(),
                service: None,
                mode: ProjectRunMode::Daemon,
                watch: None,
            })
            .expect("re-add primary project");

        match supervisor
            .handle_command(ControlCommand::Status { live: true })
            .expect("status after re-add")
        {
            ControlResponse::Status(snapshot) => {
                let names = project_service_names(&snapshot, "primary");
                assert!(
                    names.contains(&"delta".to_string()),
                    "added service missing"
                );
                assert!(names.contains(&"alpha".to_string()), "kept service missing");
            }
            other => panic!("expected status response, got {other:?}"),
        }

        assert_eq!(
            supervisor
                .daemon
                .config()
                .services
                .get("alpha")
                .map(|service| service.command.as_str()),
            Some("/bin/sleep 60")
        );

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn add_project_config_registers_extra_project_cron_jobs() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let alpha_config = temp.path().join("alpha.yaml");
        let beta_config = temp.path().join("beta.yaml");
        fs::write(
            &alpha_config,
            r#"
version: "2"
project:
  id: alpha
  name: Alpha
services:
  alpha_worker:
    command: "/bin/true"
"#,
        )
        .expect("write alpha config");
        fs::write(
            &beta_config,
            r#"
version: "2"
project:
  id: beta
  name: Beta
services:
  beta_cron:
    command: "/bin/true"
    cron:
      expression: "0 * * * *"
      timezone: "UTC"
"#,
        )
        .expect("write beta config");

        let mut supervisor =
            Supervisor::new(alpha_config, false, None).expect("create supervisor");
        supervisor
            .handle_command(ControlCommand::AddProject {
                config: beta_config.to_string_lossy().to_string(),
                service: None,
                mode: ProjectRunMode::Daemon,
                watch: None,
            })
            .expect("add beta project");

        let jobs = supervisor.get_cron_jobs();
        assert!(
            jobs.iter().any(|job| job.service_name == "beta_cron"),
            "extra project cron job should be registered"
        );

        let beta_hash = supervisor
            .extra_projects
            .get("beta")
            .map(|project| project.daemon.config().state_key("beta_cron"))
            .expect("beta cron hash");
        assert!(
            jobs.iter().any(|job| job.service_hash == beta_hash),
            "extra project cron job should be registered by service hash"
        );

        let cron_projects = supervisor
            .cron_projects
            .read()
            .expect("cron projects lock")
            .clone();
        assert!(
            cron_projects.iter().any(|project| {
                project.project_id == "beta"
                    && project.config.services.contains_key("beta_cron")
                    && project.config.state_key("beta_cron") == beta_hash
            }),
            "extra project cron job should be routable to its project"
        );

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn aggregate_status_preserves_cron_state_for_all_projects() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let alpha_config = temp.path().join("alpha.yaml");
        let beta_config = temp.path().join("beta.yaml");
        fs::write(
            &alpha_config,
            r#"
version: "2"
project:
  id: alpha
  name: Alpha
services:
  alpha_cron:
    command: "/bin/true"
    cron:
      expression: "0 * * * *"
      timezone: "UTC"
"#,
        )
        .expect("write alpha config");
        fs::write(
            &beta_config,
            r#"
version: "2"
project:
  id: beta
  name: Beta
services:
  beta_cron:
    command: "/bin/true"
    cron:
      expression: "0 * * * *"
      timezone: "UTC"
"#,
        )
        .expect("write beta config");

        let mut supervisor =
            Supervisor::new(alpha_config, false, None).expect("create supervisor");
        supervisor
            .handle_command(ControlCommand::AddProject {
                config: beta_config.to_string_lossy().to_string(),
                service: None,
                mode: ProjectRunMode::Daemon,
                watch: None,
            })
            .expect("add beta project");

        let alpha_hash = supervisor.daemon.config().state_key("alpha_cron");
        let beta_hash = supervisor
            .extra_projects
            .get("beta")
            .map(|project| project.daemon.config().state_key("beta_cron"))
            .expect("beta cron hash");

        let alpha_store = supervisor.daemon.store();
        let beta_store = supervisor
            .extra_projects
            .get("beta")
            .expect("beta project")
            .daemon
            .store();
        let alpha_cron = crate::cron::CronStateFile::load(alpha_store.clone())
            .expect("load alpha cron");
        let beta_cron =
            crate::cron::CronStateFile::load(beta_store.clone()).expect("load beta cron");
        assert!(
            alpha_cron.jobs().contains_key(&alpha_hash),
            "alpha cron should be persisted before aggregate status"
        );
        assert!(
            beta_cron.jobs().contains_key(&beta_hash),
            "beta cron should be persisted before aggregate status"
        );

        match supervisor
            .handle_command(ControlCommand::Status { live: true })
            .expect("status response")
        {
            ControlResponse::Status(snapshot) => {
                assert!(
                    snapshot.units.iter().any(|unit| {
                        unit.name == "alpha_cron"
                            && unit.cron.is_some()
                            && unit.project.as_ref().map(|project| project.id.as_str())
                                == Some("alpha")
                    }),
                    "alpha cron should retain cron status in aggregate snapshot"
                );
                assert!(
                    snapshot.units.iter().any(|unit| {
                        unit.name == "beta_cron"
                            && unit.cron.is_some()
                            && unit.project.as_ref().map(|project| project.id.as_str())
                                == Some("beta")
                    }),
                    "beta cron should retain cron status in aggregate snapshot"
                );
            }
            other => panic!("expected status response, got {other:?}"),
        }

        let alpha_cron = crate::cron::CronStateFile::load(alpha_store)
            .expect("load alpha cron after aggregate status");
        let beta_cron = crate::cron::CronStateFile::load(beta_store)
            .expect("load beta cron after aggregate status");
        assert!(
            alpha_cron.jobs().contains_key(&alpha_hash),
            "aggregate status should not prune primary project cron state"
        );
        assert!(
            beta_cron.jobs().contains_key(&beta_hash),
            "aggregate status should not prune extra project cron state"
        );

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn stop_extra_project_removes_status_and_cron_routing() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let alpha_config = temp.path().join("alpha.yaml");
        let beta_config = temp.path().join("beta.yaml");
        fs::write(
            &alpha_config,
            r#"
version: "2"
project:
  id: alpha
  name: Alpha
services:
  alpha_worker:
    command: "/bin/true"
"#,
        )
        .expect("write alpha config");
        fs::write(
            &beta_config,
            r#"
version: "2"
project:
  id: beta
  name: Beta
services:
  beta_worker:
    command: "/bin/sleep 31"
  beta_cron:
    command: "/bin/true"
    cron:
      expression: "0 * * * *"
      timezone: "UTC"
"#,
        )
        .expect("write beta config");

        let mut supervisor =
            Supervisor::new(alpha_config, false, None).expect("create supervisor");
        supervisor
            .handle_command(ControlCommand::AddProject {
                config: beta_config.to_string_lossy().to_string(),
                service: None,
                mode: ProjectRunMode::Daemon,
                watch: None,
            })
            .expect("add beta project");
        assert!(
            supervisor.extra_projects.contains_key("beta"),
            "beta should be registered before stop"
        );
        assert!(
            supervisor
                .get_cron_jobs()
                .iter()
                .any(|job| job.service_name == "beta_cron"),
            "beta cron should be registered before stop"
        );

        let response = supervisor
            .handle_command(ControlCommand::Stop {
                service: None,
                project: Some("beta".into()),
                watch: None,
            })
            .expect("stop beta project");
        match response {
            ControlResponse::Message(message) => {
                assert_eq!(message, "Project 'beta' stopped");
            }
            other => panic!("expected stop message response, got {other:?}"),
        }
        assert!(
            !supervisor.extra_projects.contains_key("beta"),
            "beta should be removed after stop"
        );
        assert!(
            !supervisor
                .get_cron_jobs()
                .iter()
                .any(|job| job.service_name == "beta_cron"),
            "beta cron should be pruned after project stop"
        );
        let cron_projects = supervisor
            .cron_projects
            .read()
            .expect("cron projects lock")
            .clone();
        assert!(
            !cron_projects
                .iter()
                .any(|project| project.project_id == "beta"),
            "beta should be removed from cron routing"
        );

        match supervisor
            .handle_command(ControlCommand::Status { live: true })
            .expect("status after beta stop")
        {
            ControlResponse::Status(snapshot) => {
                assert!(
                    snapshot.units.iter().all(|unit| {
                        unit.project.as_ref().map(|project| project.id.as_str())
                            != Some("beta")
                    }),
                    "stopped extra project should not remain visible in status"
                );
            }
            other => panic!("expected status response, got {other:?}"),
        }

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn stop_and_readd_extra_project_preserves_cron_history() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let alpha_config = temp.path().join("alpha.yaml");
        let beta_config = temp.path().join("beta.yaml");
        fs::write(
            &alpha_config,
            r#"
version: "2"
project:
  id: alpha
  name: Alpha
services:
  alpha_worker:
    command: "/bin/true"
"#,
        )
        .expect("write alpha config");
        fs::write(
            &beta_config,
            r#"
version: "2"
project:
  id: beta
  name: Beta
services:
  beta_cron:
    command: "/bin/true"
    cron:
      expression: "*/1 * * * * *"
      timezone: "UTC"
"#,
        )
        .expect("write beta config");

        let mut supervisor =
            Supervisor::new(alpha_config, false, None).expect("create supervisor");
        supervisor
            .handle_command(ControlCommand::AddProject {
                config: beta_config.to_string_lossy().to_string(),
                service: None,
                mode: ProjectRunMode::Daemon,
                watch: None,
            })
            .expect("add beta project");

        let beta_hash = supervisor
            .extra_projects
            .get("beta")
            .map(|project| project.daemon.config().state_key("beta_cron"))
            .expect("beta cron hash");

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let due_jobs = supervisor.cron_manager.get_due_job_refs();
            if due_jobs.iter().any(|job| job.service_hash == beta_hash) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for beta cron to become due"
            );
            thread::sleep(Duration::from_millis(50));
        }

        supervisor.cron_manager.mark_job_completed_by_hash(
            &beta_hash,
            CronExecutionStatus::Success,
            Some(0),
            vec![],
        );

        supervisor.status_cache.replace(StatusSnapshot {
            schema_version: crate::status::STATUS_SCHEMA_VERSION.into(),
            captured_at: Utc::now(),
            overall_health: OverallHealth::Healthy,
            units: Vec::new(),
        });

        match supervisor
            .handle_command(ControlCommand::Status { live: false })
            .expect("status response")
        {
            ControlResponse::Status(snapshot) => {
                let beta_unit = snapshot
                    .units
                    .iter()
                    .find(|unit| unit.hash == beta_hash)
                    .expect("beta cron in non-live status");
                assert_eq!(
                    beta_unit
                        .cron
                        .as_ref()
                        .expect("beta cron status")
                        .recent_runs
                        .len(),
                    1,
                    "non-live status should read current cron history"
                );
            }
            other => panic!("expected status response, got {other:?}"),
        }

        supervisor.status_cache.replace(StatusSnapshot {
            schema_version: crate::status::STATUS_SCHEMA_VERSION.into(),
            captured_at: Utc::now(),
            overall_health: OverallHealth::Healthy,
            units: Vec::new(),
        });

        match supervisor
            .handle_command(ControlCommand::Inspect {
                unit: "beta_cron".into(),
                project: Some("beta".into()),
                samples: 10,
                live: false,
            })
            .expect("inspect response")
        {
            ControlResponse::Inspect(payload) => {
                assert_eq!(
                    payload
                        .unit
                        .as_ref()
                        .and_then(|unit| unit.cron.as_ref())
                        .map(|cron| cron.recent_runs.len()),
                    Some(1),
                    "non-live inspect should read current cron history"
                );
            }
            other => panic!("expected inspect response, got {other:?}"),
        }

        let beta_store = supervisor
            .extra_projects
            .get("beta")
            .expect("beta project")
            .daemon
            .store();
        let cron_state = crate::cron::CronStateFile::load(beta_store.clone())
            .expect("load cron state before stop");
        assert_eq!(
            cron_state
                .jobs()
                .get(&beta_hash)
                .expect("beta cron state before stop")
                .execution_history
                .len(),
            1,
            "beta cron history should be recorded before stop"
        );

        supervisor
            .handle_command(ControlCommand::Stop {
                service: None,
                project: Some("beta".into()),
                watch: None,
            })
            .expect("stop beta project");

        assert!(
            !supervisor
                .get_cron_jobs()
                .iter()
                .any(|job| job.service_name == "beta_cron"),
            "stopped beta cron should leave active scheduler routing"
        );
        let cron_state = crate::cron::CronStateFile::load(beta_store)
            .expect("load cron state after stop");
        assert_eq!(
            cron_state
                .jobs()
                .get(&beta_hash)
                .expect("beta cron state after stop")
                .execution_history
                .len(),
            1,
            "stopping an extra project must not delete persisted cron history"
        );

        supervisor
            .handle_command(ControlCommand::AddProject {
                config: beta_config.to_string_lossy().to_string(),
                service: None,
                mode: ProjectRunMode::Daemon,
                watch: None,
            })
            .expect("re-add beta project");

        let beta_job = supervisor
            .get_cron_jobs()
            .into_iter()
            .find(|job| job.service_hash == beta_hash)
            .expect("re-added beta cron job");
        assert_eq!(
            beta_job.execution_history.len(),
            1,
            "re-added beta cron should restore existing history for the same hash"
        );

        supervisor
            .shutdown_runtime()
            .expect("shutdown test supervisor runtime");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn offline_unit(name: &str, project: &str) -> UnitStatus {
        UnitStatus {
            name: name.into(),
            hash: format!("{name}-hash"),
            project: Some(crate::status::ProjectStatus {
                id: project.into(),
                name: project.into(),
                mode: Default::default(),
                config_path: None,
                boot: None,
                loose: false,
            }),
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
            command: Some("/bin/true".into()),
            runtime_command: None,
            spawned_children: Vec::new(),
        }
    }

    /// Drives `handle_logs_command` for a project-wide request over a socket pair
    /// and returns the raw bytes streamed to the client.
    fn run_project_logs(
        snapshot: StatusSnapshot,
        structured: bool,
        filter: crate::logs::LogFilter,
    ) -> String {
        use std::io::Read as _;

        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let request = SupervisorLogRequest {
            snapshot,
            service: None,
            project: Some("arb".into()),
            lines: 50,
            kind: None,
            follow: false,
            filter,
            structured,
            stream: &server,
        };
        Supervisor::handle_logs_command(request).expect("logs command");
        drop(server);

        let mut client = client;
        let mut out = Vec::new();
        client.read_to_end(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn project_logs_apply_grep_and_emit_service_markers() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let log_dir = runtime::log_dir().join("arb");
        fs::create_dir_all(&log_dir).expect("make log dir");
        fs::write(
            log_dir.join("arb_rs__server.log"),
            "2026-07-08T09:00:00Z stdout openai_call ok\n\
2026-07-08T09:00:01Z stdout gemini_call ignored\n",
        )
        .unwrap();
        fs::write(
            log_dir.join("arb_py__curator.log"),
            "2026-07-08T09:00:02Z stdout rolling insights ignored\n\
2026-07-08T09:00:03Z stdout openai_embeddings ok\n",
        )
        .unwrap();

        let snapshot = StatusSnapshot {
            schema_version: crate::status::STATUS_SCHEMA_VERSION.into(),
            captured_at: Utc::now(),
            overall_health: OverallHealth::Healthy,
            units: vec![
                offline_unit("arb_rs__server", "arb"),
                offline_unit("arb_py__curator", "arb"),
            ],
        };

        let filter = crate::logs::LogFilter::from_parts(
            None,
            None,
            Some("openai_"),
            false,
            Utc::now(),
        )
        .unwrap();

        let out = run_project_logs(snapshot, true, filter);

        assert!(out.contains("openai_call ok"), "{out}");
        assert!(out.contains("openai_embeddings ok"), "{out}");
        assert!(!out.contains("gemini_call"), "{out}");
        assert!(!out.contains("rolling insights"), "{out}");

        let server_marker =
            String::from_utf8(crate::logs::service_marker_line("arb_rs__server"))
                .unwrap();
        let curator_marker =
            String::from_utf8(crate::logs::service_marker_line("arb_py__curator"))
                .unwrap();
        assert!(out.contains(server_marker.trim_end()), "{out}");
        assert!(out.contains(curator_marker.trim_end()), "{out}");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn project_logs_omit_service_markers_when_not_structured() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let log_dir = runtime::log_dir().join("arb");
        fs::create_dir_all(&log_dir).expect("make log dir");
        fs::write(
            log_dir.join("arb_rs__server.log"),
            "2026-07-08T09:00:00Z stdout plain line\n",
        )
        .unwrap();

        let snapshot = StatusSnapshot {
            schema_version: crate::status::STATUS_SCHEMA_VERSION.into(),
            captured_at: Utc::now(),
            overall_health: OverallHealth::Healthy,
            units: vec![offline_unit("arb_rs__server", "arb")],
        };

        let out = run_project_logs(snapshot, false, crate::logs::LogFilter::default());

        assert!(out.contains("plain line"), "{out}");
        assert!(!out.contains(crate::logs::SERVICE_MARKER_PREFIX), "{out}");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn repro_project_follow_beta_leak() {
        let _guard = crate::test_utils::env_lock();
        let base = std::env::current_dir().unwrap().join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);

        let alpha_dir = runtime::log_dir().join("alpha");
        let beta_dir = runtime::log_dir().join("beta");
        fs::create_dir_all(&alpha_dir).unwrap();
        fs::create_dir_all(&beta_dir).unwrap();
        fs::write(
            alpha_dir.join("alphasvc.log"),
            "2026-07-08T09:00:00Z stdout ALPHA_MARKER_LINE\n",
        )
        .unwrap();
        fs::write(
            beta_dir.join("betasvc.log"),
            "2026-07-08T09:00:00Z stdout BETA_MARKER_LINE\n",
        )
        .unwrap();

        let snapshot = StatusSnapshot {
            schema_version: crate::status::STATUS_SCHEMA_VERSION.into(),
            captured_at: Utc::now(),
            overall_health: OverallHealth::Healthy,
            units: vec![
                offline_unit("alphasvc", "alpha"),
                offline_unit("betasvc", "beta"),
            ],
        };

        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let request = SupervisorLogRequest {
            snapshot,
            service: None,
            project: Some("alpha".into()),
            lines: 50,
            kind: None,
            follow: false,
            filter: crate::logs::LogFilter::default(),
            structured: false,
            stream: &server,
        };
        Supervisor::handle_logs_command(request).expect("logs command");
        drop(server);
        use std::io::Read as _;
        let mut client = client;
        let mut out = Vec::new();
        client.read_to_end(&mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        eprintln!("=== OUT ===\n{out}\n=== END ===");
        assert!(out.contains("ALPHA_MARKER_LINE"), "alpha present: {out}");
        assert!(!out.contains("BETA_MARKER_LINE"), "BETA LEAKED: {out}");

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn handoff_home() -> (tempfile::TempDir, Option<String>) {
        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create base dir");
        let temp = tempdir_in(&base).expect("create tempdir");
        let home = temp.path().join("home");
        fs::create_dir_all(&home).expect("create home");
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &home);
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);
        (temp, original_home)
    }

    fn restore_home(original_home: Option<String>) {
        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        runtime::init(runtime::RuntimeMode::User);
        runtime::set_drop_privileges(false);
    }

    fn loose_manifest(dir: &std::path::Path, service: &str) -> PathBuf {
        let path = dir.join("tunnel-8d7b.yaml");
        fs::write(
            &path,
            format!(
                "version: \"2\"\nservices:\n  {service}:\n    command: \"/bin/sleep 30\"\n"
            ),
        )
        .expect("write manifest");
        path
    }

    #[test]
    fn handoff_translates_the_legacy_loose_id_to_the_derived_one() {
        let _guard = crate::test_utils::env_lock();
        let (temp, original_home) = handoff_home();

        let config_path = loose_manifest(temp.path(), "tunnel");
        let record = crate::upgrade::HandoffProject {
            project_id: crate::state_store::LOOSE_PROJECT_ID.to_string(),
            config_path: config_path.clone(),
            config_hash: ipc::manifest_content_hash(&config_path).expect("hash"),
            mode: ProjectRunMode::Daemon,
            active: true,
            daemon: crate::upgrade::HandoffDaemonState {
                processes: Vec::new(),
                manual_stops: Vec::new(),
                restart_suppressed: Vec::new(),
                restart_counts: std::collections::BTreeMap::new(),
                stopped_for_dependency: std::collections::BTreeMap::new(),
            },
        };

        let loaded = Supervisor::load_handoff_project(&record, None).expect("translate");
        assert!(loaded.config.project.loose);
        assert_ne!(
            loaded.config.project.id,
            crate::state_store::LOOSE_PROJECT_ID
        );
        assert_eq!(
            loaded.legacy_id.as_deref(),
            Some(crate::state_store::LOOSE_PROJECT_ID)
        );

        restore_home(original_home);
    }

    #[test]
    fn handoff_still_refuses_a_genuinely_absent_project() {
        let _guard = crate::test_utils::env_lock();
        let (temp, original_home) = handoff_home();

        let config_path = loose_manifest(temp.path(), "tunnel");
        let record = crate::upgrade::HandoffProject {
            project_id: "some-named-project".to_string(),
            config_path: config_path.clone(),
            config_hash: ipc::manifest_content_hash(&config_path).expect("hash"),
            mode: ProjectRunMode::Daemon,
            active: true,
            daemon: crate::upgrade::HandoffDaemonState {
                processes: Vec::new(),
                manual_stops: Vec::new(),
                restart_suppressed: Vec::new(),
                restart_counts: std::collections::BTreeMap::new(),
                stopped_for_dependency: std::collections::BTreeMap::new(),
            },
        };

        // Only the literal legacy loose id is translated; any other mismatch is
        // still the config drift it always was.
        assert!(Supervisor::load_handoff_project(&record, None).is_err());

        restore_home(original_home);
    }

    #[test]
    fn a_pre_migration_handoff_resumes_a_live_loose_service() {
        let _guard = crate::test_utils::env_lock();
        let (temp, original_home) = handoff_home();

        let config_path = loose_manifest(temp.path(), "tunnel");
        let config_hash = ipc::manifest_content_hash(&config_path).expect("hash");

        // A real process standing in for the service the 0.58 resident was
        // supervising: adoption verifies pid, pgid and kernel start time.
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        let pgid = crate::daemon::Daemon::process_group_for_pid(pid).expect("child pgid");
        let started = crate::daemon::process_start_time(pid).expect("child start time");

        // The legacy on-disk layout the old supervisor left behind.
        let legacy_dir = runtime::state_dir()
            .join(crate::state_store::PROJECTS_DIR)
            .join(crate::state_store::LOOSE_PROJECT_ID);
        fs::create_dir_all(&legacy_dir).expect("legacy dir");
        fs::write(
            legacy_dir.join(crate::constants::PID_FILE_NAME),
            format!(
                "<PidFile>\n  <services>\n    <name>tunnel</name>\n    <pid>{pid}</pid>\n  \
                 </services>\n  <service_groups>\n    <name>tunnel</name>\n    \
                 <pgid>{pgid}</pgid>\n  </service_groups>\n  <service_starts>\n    \
                 <name>tunnel</name>\n    <started>{started}</started>\n  \
                 </service_starts>\n</PidFile>\n"
            ),
        )
        .expect("legacy pid.xml");
        fs::write(
            legacy_dir.join(crate::constants::STATE_FILE_NAME),
            format!(
                "<ServiceStateFile>\n  <services>\n    <name>v2:none:tunnel</name>\n    \
                 <state>\n      <status>running</status>\n      <pid>{pid}</pid>\n    \
                 </state>\n  </services>\n</ServiceStateFile>\n"
            ),
        )
        .expect("legacy state.xml");

        let current = crate::upgrade::LiveUpgradeInfo::current();
        let state = SupervisorHandoff {
            schema: crate::upgrade::HANDOFF_SCHEMA_VERSION,
            protocol: crate::upgrade::LIVE_REEXEC_PROTOCOL,
            source_binary: std::env::current_exe().expect("exe"),
            source_version: current.version.clone(),
            target_version: current.version.clone(),
            rollback_reason: None,
            lock_fd: -1,
            listener_fd: -1,
            service_filter: None,
            pipe_stderr: false,
            primary: crate::upgrade::HandoffProject {
                project_id: crate::state_store::LOOSE_PROJECT_ID.to_string(),
                config_path: config_path.clone(),
                config_hash,
                mode: ProjectRunMode::Daemon,
                active: true,
                daemon: crate::upgrade::HandoffDaemonState {
                    processes: vec![crate::upgrade::HandoffProcess {
                        service: "tunnel".to_string(),
                        pid,
                        pgid,
                        sid: None,
                        started,
                    }],
                    manual_stops: Vec::new(),
                    restart_suppressed: Vec::new(),
                    restart_counts: std::collections::BTreeMap::new(),
                    stopped_for_dependency: std::collections::BTreeMap::new(),
                },
            },
            projects: std::collections::BTreeMap::new(),
            log_pipes: Vec::new(),
            manifests: BTreeMap::new(),
        };
        let handoff_path = state.persist().expect("persist handoff");

        let supervisor =
            Supervisor::from_handoff(handoff_path).expect("resume across migration");

        // The resumed supervisor speaks the NEW identity end to end.
        let new_id = supervisor.daemon.config().project.id.clone();
        assert_ne!(new_id, crate::state_store::LOOSE_PROJECT_ID);
        assert!(supervisor.daemon.config().project.loose);

        // Its pids were seeded into the derived project's store.
        let seeded = fs::read_to_string(
            runtime::state_dir()
                .join(crate::state_store::PROJECTS_DIR)
                .join(&new_id)
                .join(crate::constants::PID_FILE_NAME),
        )
        .expect("seeded pid.xml");
        assert!(seeded.contains(&format!("<pid>{pid}</pid>")));

        // And the registry learned about it, so a later cold boot restores it.
        let registry = crate::loose_registry::LooseRegistry::load().expect("registry");
        assert!(registry.by_project(&new_id).is_some());

        let _ = child.kill();
        let _ = child.wait();
        drop(supervisor);
        restore_home(original_home);
    }
}
