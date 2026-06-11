#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    io,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    thread,
    time::{Duration, SystemTime},
};

use nix::unistd::{Uid, User};
use thiserror::Error;
use tracing::{debug, error, info, warn};

use crate::{
    config::{
        Config, SkipConfig, SpawnMode, StatusSnapshotMode, TerminationPolicy, load_config,
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
    metrics::{self, MetricsCollector, MetricsHandle},
    runtime,
    spawn::{DynamicSpawnManager, SpawnedChild, SpawnedChildKind, SpawnedExit},
    status::{
        ProjectRunMode, StatusCache, StatusError, StatusRefresher, StatusSnapshot,
        collect_runtime_snapshot, collect_runtime_snapshot_with_cron_hashes,
        compute_overall_health, cron_hashes_for_config,
    },
};

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

/// Daemon supervisor that handles CLI commands.
pub struct Supervisor {
    config_path: PathBuf,
    daemon: Daemon,
    detach_children: bool,
    cron_manager: CronManager,
    service_filter: Option<String>,
    status_cache: StatusCache,
    status_refresher: Option<StatusRefresher>,
    metrics_store: MetricsHandle,
    metrics_collector: Option<MetricsCollector>,
    spawn_manager: DynamicSpawnManager,
    pipe_stderr: bool,
    primary_project_mode: ProjectRunMode,
    extra_projects: BTreeMap<String, ProjectRuntime>,
    cron_projects: Arc<RwLock<Vec<CronProjectRuntime>>>,
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
}

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
    /// Shared PID file handle used by the log manager.
    pid_file: std::sync::Arc<std::sync::Mutex<crate::daemon::PidFile>>,
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
    /// Supervisor-owned Unix stream connected to the CLI client.
    stream: &'a std::os::unix::net::UnixStream,
}

#[derive(Debug, Clone)]
/// Represents cron completion outcome.
struct CronCompletionOutcome {
    status: CronExecutionStatus,
    exit_code: Option<i32>,
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

/// Splits a qualified selector of the form `project_id/service_name`.
fn split_project_selector(selector: &str) -> Option<(&str, &str)> {
    let (project, service) = selector.split_once('/')?;
    if project.is_empty() || service.is_empty() {
        None
    } else {
        Some((project, service))
    }
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
        match config.status.snapshot_mode {
            StatusSnapshotMode::Off => StatusSnapshotMode::Summary,
            mode => mode,
        }
    }

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

    /// Registers dynamic spawn limits from a project config.
    fn register_spawn_limits_for_config(
        spawn_manager: &DynamicSpawnManager,
        config: &Config,
    ) -> Result<(), SupervisorError> {
        for (service_name, service_config) in &config.services {
            if let Some(spawn_config) = &service_config.spawn
                && let Some(limits) = &spawn_config.limits
            {
                spawn_manager.register_service(service_name.clone(), limits)?;
            }
        }

        Ok(())
    }

    /// Starts services for a project daemon without blocking the supervisor control loop.
    fn start_project_services(
        daemon: &Daemon,
        config: &Config,
        service_filter: Option<&str>,
        spawn_manager: &DynamicSpawnManager,
    ) -> Result<(), SupervisorError> {
        let service_order = Self::startup_service_order(config, service_filter)?;
        for service_name in service_order {
            let Some(service_config) = config.services.get(&service_name) else {
                continue;
            };

            if service_config.cron.is_some() {
                continue;
            }

            if let Some(skip_config) = &service_config.skip {
                match skip_config {
                    SkipConfig::Flag(true) => {
                        info!("Skipping service '{service_name}' due to skip flag");
                        daemon.mark_service_skipped(&service_name)?;
                        continue;
                    }
                    SkipConfig::Flag(false) => {}
                    SkipConfig::Command(skip_command) => {
                        match daemon.evaluate_skip_condition(&service_name, skip_command)
                        {
                            Ok(true) => {
                                info!(
                                    "Skipping service '{service_name}' due to skip condition"
                                );
                                daemon.mark_service_skipped(&service_name)?;
                                continue;
                            }
                            Ok(false) => {}
                            Err(err) => {
                                warn!(
                                    "Failed to evaluate skip condition for '{service_name}': {err}"
                                );
                            }
                        }
                    }
                }
            }

            daemon.start_service(&service_name, service_config)?;

            if let Some(ref spawn) = service_config.spawn
                && let Some(SpawnMode::Dynamic) = spawn.mode
                && let Ok(pid_file) = daemon.pid_file_handle().lock()
                && let Some(&pid) = pid_file.services().get(&service_name)
            {
                spawn_manager.register_service_pid(service_name.clone(), pid);
            }
        }

        daemon.ensure_monitoring()?;
        Ok(())
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

    /// Returns cron hashes for all projects currently managed by the supervisor.
    fn managed_cron_hashes(&self) -> HashSet<String> {
        let mut hashes = cron_hashes_for_config(self.daemon.config().as_ref());
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
        let mut snapshots = vec![Self::collect_daemon_snapshot(
            &self.daemon,
            &self.metrics_store,
            &self.spawn_manager,
            primary_mode,
            self.primary_project_mode,
            &self.config_path,
            Some(&valid_cron_hashes),
        )?];

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
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "project flag '{flag}' does not match service selector project '{selector_project}'"
                ),
            )
            .into());
        }

        let requested_project = project.or(selector_project);
        if let (Some(requested), Some(config_project)) =
            (requested_project, config_project)
            && requested != config_project
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "project '{requested}' does not match config project '{config_project}'"
                ),
            )
            .into());
        }

        if let Some(target_project) = requested_project.or(config_project) {
            return Ok(target_project.to_string());
        }

        let matching_projects = self.projects_containing_service(service_name);
        match matching_projects.as_slice() {
            [project_id] => Ok(project_id.clone()),
            [] => Ok(self.daemon.config().project.id.clone()),
            projects => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "service '{service_name}' exists in multiple projects ({}); pass --project to choose one",
                    projects.join(", ")
                ),
            )
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

        if service_config.cron.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "cron unit '{service_name}' cannot be started directly; start or reload project '{target_project}' to schedule it"
                ),
            )
            .into());
        }

        daemon.start_service(service_name, &service_config)?;

        if let Some(ref spawn) = service_config.spawn
            && let Some(SpawnMode::Dynamic) = spawn.mode
            && let Ok(pid_file) = daemon.pid_file_handle().lock()
            && let Some(&pid) = pid_file.services().get(service_name)
        {
            self.spawn_manager
                .register_service_pid(service_name.to_string(), pid);
        }

        Ok((target_project, service_name.to_string()))
    }

    /// Starts all non-cron services in one managed project.
    fn start_project_target(&self, project_id: &str) -> Result<(), SupervisorError> {
        let primary_project = self.daemon.config().project.id.clone();
        if project_id == primary_project {
            self.daemon.start_services_blocking()?;
            return Ok(());
        }

        let Some(project_runtime) = self.extra_projects.get(project_id) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' is not managed by this supervisor"),
            )
            .into());
        };

        Self::start_project_services(
            &project_runtime.daemon,
            project_runtime.daemon.config().as_ref(),
            None,
            &self.spawn_manager,
        )?;
        Ok(())
    }

    /// Restarts all non-cron services in one managed project.
    fn restart_project_target(
        &mut self,
        project_id: &str,
        config_path: Option<&Path>,
    ) -> Result<(), SupervisorError> {
        if let Some(config_path) = config_path {
            let resolved = if config_path.is_absolute() {
                config_path.to_path_buf()
            } else {
                std::env::current_dir()?.join(config_path)
            };
            let resolved = resolved.canonicalize().unwrap_or(resolved);
            runtime::ensure_trusted_config(&resolved)?;
            let config = load_config(Some(resolved.to_string_lossy().as_ref()))?;
            if config.project.id != project_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "project '{project_id}' does not match config project '{}'",
                        config.project.id
                    ),
                )
                .into());
            }

            let primary_project = self.daemon.config().project.id.clone();
            if project_id == primary_project {
                self.reload_config(&resolved)?;
                return Ok(());
            }

            self.replace_extra_project_runtime(config, resolved)?;
            return Ok(());
        }

        let primary_project = self.daemon.config().project.id.clone();
        if project_id == primary_project {
            self.daemon.restart_services()?;
            return Ok(());
        }

        let Some(project_runtime) = self.extra_projects.get(project_id) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' is not managed by this supervisor"),
            )
            .into());
        };

        project_runtime.daemon.restart_services()?;
        Ok(())
    }

    /// Replaces an extra project with a freshly loaded runtime and starts it.
    fn replace_extra_project_runtime(
        &mut self,
        config: Config,
        config_path: PathBuf,
    ) -> Result<(), SupervisorError> {
        let project_id = config.project.id.clone();
        let Some(existing) = self.extra_projects.remove(&project_id) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' is not managed by this supervisor"),
            )
            .into());
        };
        let mode = existing.mode;
        existing.daemon.stop_services()?;
        existing.daemon.shutdown_monitor();

        Self::register_spawn_limits_for_config(&self.spawn_manager, &config)?;
        let mut daemon = Daemon::new(
            config,
            self.daemon.pid_file_handle(),
            self.daemon.service_state_handle(),
            self.detach_children,
        );
        daemon.set_pipe_stderr(self.pipe_stderr);
        self.extra_projects.insert(
            project_id.clone(),
            ProjectRuntime {
                daemon,
                mode,
                config_path,
            },
        );

        let project = self.extra_projects.get(&project_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' was not registered"),
            )
        })?;
        Self::start_project_services(
            &project.daemon,
            project.daemon.config().as_ref(),
            None,
            &self.spawn_manager,
        )?;
        self.sync_cron_projects()?;
        Ok(())
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
        let config_path = if config_path.is_absolute() {
            config_path
        } else {
            std::env::current_dir()?.join(config_path)
        };
        let config_path = config_path.canonicalize().unwrap_or(config_path);
        let config = load_config(Some(config_path.to_string_lossy().as_ref()))?;
        let cron_manager = CronManager::new();
        cron_manager.sync_from_config(&config)?;

        let daemon = Daemon::from_config(config.clone(), detach_children)?;
        let config_arc = daemon.config();
        let cron_projects = Arc::new(RwLock::new(vec![CronProjectRuntime {
            project_id: config_arc.project.id.clone(),
            daemon: daemon.clone(),
            config: Arc::clone(&config_arc),
        }]));
        let metrics_settings = config_arc
            .metrics
            .to_settings(config_arc.project_dir.as_deref().map(Path::new));
        let metrics_store = metrics::shared_store(metrics_settings)?;
        let status_cache = StatusCache::new(StatusSnapshot::empty());

        let spawn_manager = DynamicSpawnManager::new();
        for (service_name, service_config) in &config.services {
            if let Some(ref spawn) = service_config.spawn
                && let Some(SpawnMode::Dynamic) = spawn.mode
                && let Some(ref limits) = spawn.limits
            {
                spawn_manager.register_service(service_name.clone(), limits)?;
            }
        }

        Ok(Self {
            config_path,
            daemon,
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
            extra_projects: BTreeMap::new(),
            cron_projects,
        })
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
        let mut projects = vec![CronProjectRuntime {
            project_id: self.daemon.config().project.id.clone(),
            daemon: self.daemon.clone(),
            config: self.daemon.config(),
        }];

        projects.extend(self.extra_projects.iter().map(|(project_id, project)| {
            CronProjectRuntime {
                project_id: project_id.clone(),
                daemon: project.daemon.clone(),
                config: project.daemon.config(),
            }
        }));

        projects
    }

    /// Synchronizes cron registration and scheduler routing for all managed projects.
    fn sync_cron_projects(&self) -> Result<(), SupervisorError> {
        let projects = self.cron_project_runtimes();
        self.cron_manager
            .sync_from_configs(projects.iter().map(|project| project.config.as_ref()))?;

        match self.cron_projects.write() {
            Ok(mut guard) => *guard = projects,
            Err(err) => warn!("Failed to update cron project routing: {}", err),
        }

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

    /// Runs the supervisor event loop.
    fn run_internal(&mut self) -> Result<(), SupervisorError> {
        ipc::cleanup_runtime()?;
        ipc::write_config_hint(&self.config_path)?;
        let listener = ipc::bind_control_socket()?;
        ipc::write_supervisor_pid(unsafe { libc::getpid() })?;
        let config = load_config(Some(self.config_path.to_string_lossy().as_ref()))?;
        let service_order =
            Self::startup_service_order(&config, self.service_filter.as_deref())?;
        for service_name in service_order {
            let Some(service_config) = config.services.get(&service_name) else {
                continue;
            };

            if service_config.cron.is_none() {
                if let Some(skip_config) = &service_config.skip {
                    match skip_config {
                        SkipConfig::Flag(true) => {
                            info!("Skipping service '{service_name}' due to skip flag");
                            if let Err(err) =
                                self.daemon.mark_service_skipped(&service_name)
                            {
                                warn!(
                                    "Failed to record skipped state for '{service_name}': {err}"
                                );
                            }
                            continue;
                        }
                        SkipConfig::Flag(false) => {
                            debug!(
                                "Skip flag for '{service_name}' disabled; starting service"
                            );
                        }
                        SkipConfig::Command(skip_command) => {
                            match self
                                .daemon
                                .evaluate_skip_condition(&service_name, skip_command)
                            {
                                Ok(true) => {
                                    info!(
                                        "Skipping service '{service_name}' due to skip condition"
                                    );
                                    if let Err(err) =
                                        self.daemon.mark_service_skipped(&service_name)
                                    {
                                        warn!(
                                            "Failed to record skipped state for '{service_name}': {err}"
                                        );
                                    }
                                    continue;
                                }
                                Ok(false) => {
                                    debug!(
                                        "Skip condition for '{service_name}' evaluated to false, starting service"
                                    );
                                }
                                Err(err) => {
                                    warn!(
                                        "Failed to evaluate skip condition for '{service_name}': {err}"
                                    );
                                }
                            }
                        }
                    }
                }
                self.daemon.start_service(&service_name, service_config)?;

                if let Some(ref spawn) = service_config.spawn
                    && let Some(SpawnMode::Dynamic) = spawn.mode
                    && let Ok(pid_file) = self.daemon.pid_file_handle().lock()
                    && let Some(&pid) = pid_file.services().get(&service_name)
                {
                    self.spawn_manager
                        .register_service_pid(service_name.clone(), pid);
                }
            }
        }
        self.daemon.ensure_monitoring()?;

        let config_handle = self.daemon.config();
        let pid_handle = self.daemon.pid_file_handle();
        let state_handle = self.daemon.service_state_handle();

        match collect_runtime_snapshot(
            Arc::clone(&config_handle),
            &pid_handle,
            &state_handle,
            Some(&self.metrics_store),
            Some(&self.spawn_manager),
            Self::status_snapshot_mode(config_handle.as_ref()),
        ) {
            Ok(mut snapshot) => {
                Self::apply_project_metadata(
                    &mut snapshot,
                    self.primary_project_mode,
                    &self.config_path,
                );
                self.status_cache.replace(snapshot);
            }
            Err(err) => error!("failed to build initial status snapshot: {err}"),
        }

        let cache_clone = self.status_cache.clone();
        let config_for_refresh = Arc::clone(&config_handle);
        let pid_for_refresh = Arc::clone(&pid_handle);
        let state_for_refresh = Arc::clone(&state_handle);
        let metrics_for_refresh = self.metrics_store.clone();
        let spawn_manager_for_refresh = self.spawn_manager.clone();
        let refresh_interval = Self::status_snapshot_interval(config_handle.as_ref());
        let refresh_mode = Self::status_snapshot_mode(config_handle.as_ref());
        let refresh_project_mode = self.primary_project_mode;
        let refresh_config_path = self.config_path.clone();
        if !matches!(refresh_mode, StatusSnapshotMode::Off) {
            self.status_refresher = Some(StatusRefresher::spawn(
                cache_clone,
                refresh_interval,
                move || {
                    let mut snapshot = collect_runtime_snapshot(
                        Arc::clone(&config_for_refresh),
                        &pid_for_refresh,
                        &state_for_refresh,
                        Some(&metrics_for_refresh),
                        Some(&spawn_manager_for_refresh),
                        refresh_mode,
                    )?;
                    Supervisor::apply_project_metadata(
                        &mut snapshot,
                        refresh_project_mode,
                        &refresh_config_path,
                    );
                    Ok(snapshot)
                },
            ));
        }

        let metrics_handle = self.metrics_store.clone();
        self.metrics_collector = Some(MetricsCollector::spawn(
            metrics_handle,
            Arc::clone(&config_handle),
            pid_handle,
            state_handle,
        ));
        let cron_manager = self.cron_manager.clone();
        let cron_projects = Arc::clone(&self.cron_projects);
        let metrics_store = self.metrics_store.clone();

        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(1));

                let due_jobs = cron_manager.get_due_job_refs();
                if !due_jobs.is_empty() {
                    let projects = match cron_projects.read() {
                        Ok(projects) => projects.clone(),
                        Err(err) => {
                            error!("Failed to read cron project routing: {}", err);
                            Vec::new()
                        }
                    };

                    for due_job in due_jobs {
                        let project = projects.iter().find(|project| {
                            project
                                .config
                                .services
                                .get(&due_job.service_name)
                                .is_some_and(|service_config| {
                                    service_config.compute_hash() == due_job.service_hash
                                })
                        });

                        let Some(project) = project else {
                            error!(
                                "Failed to resolve cron job '{}' ({}) to a managed project",
                                due_job.service_name, due_job.service_hash
                            );
                            cron_manager.mark_job_completed_by_hash(
                                &due_job.service_hash,
                                CronExecutionStatus::Failed(
                                    "Cron job project is not managed".to_string(),
                                ),
                                None,
                                vec![],
                            );
                            continue;
                        };

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

                            thread::spawn(move || {
                                use crate::daemon::PidFile;
                                match daemon
                                    .start_service(&job_name_clone, &service_config)
                                {
                                    Ok(ServiceReadyState::CompletedSuccess) => {
                                        cron_manager_clone.annotate_job_execution(
                                            &job_name_clone,
                                            None,
                                            user.clone(),
                                            command.clone(),
                                        );
                                        info!(
                                            "Cron job '{}' completed successfully",
                                            job_name_clone
                                        );

                                        let metrics = if let Ok(guard) =
                                            metrics_store_clone.try_read()
                                        {
                                            guard
                                                .snapshot_unit(&service_hash)
                                                .unwrap_or_default()
                                        } else {
                                            vec![]
                                        };

                                        cron_manager_clone.mark_job_completed_by_hash(
                                            &service_hash,
                                            CronExecutionStatus::Success,
                                            Some(0),
                                            metrics,
                                        );
                                        if let Ok(mut state_file) =
                                                        ServiceStateFile::load()
                                                        && let Err(err) = state_file.set(
                                                            &service_hash,
                                                            ServiceLifecycleStatus::ExitedSuccessfully,
                                                            None,
                                                            Some(0),
                                                            None,
                                                        )
                                                    {
                                                        warn!("Failed to persist cron job '{}' exit state: {}", job_name_clone, err);
                                                    }
                                    }
                                    Ok(ServiceReadyState::Running) => {
                                        thread::sleep(Duration::from_millis(50));
                                        match PidFile::reload() {
                                            Ok(pid_file) => {
                                                if let Some(pid) =
                                                    pid_file.get(&job_name_clone)
                                                {
                                                    cron_manager_clone
                                                        .annotate_job_execution_by_hash(
                                                            &service_hash,
                                                            Some(pid),
                                                            user.clone(),
                                                            command.clone(),
                                                        );
                                                    let result =
                                                        Self::wait_for_cron_completion(
                                                            pid,
                                                            &job_name_clone,
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
                                                                            CronExecutionStatus::OverlapError => warn!(
                                                                                "Cron job '{}' reported overlap state unexpectedly",
                                                                                job_name_clone
                                                                            ),
                                                                        }

                                                            let metrics =
                                                                if let Ok(guard) =
                                                                    metrics_store_clone
                                                                        .try_read()
                                                                {
                                                                    guard
                                                                    .snapshot_unit(
                                                                        &service_hash,
                                                                    )
                                                                    .unwrap_or_default()
                                                                } else {
                                                                    vec![]
                                                                };

                                                            cron_manager_clone.mark_job_completed_by_hash(
                                                                            &service_hash,
                                                                            status.clone(),
                                                                            exit_code,
                                                                            metrics,
                                                                        );
                                                            if let Ok(mut state_file) =
                                                                ServiceStateFile::load()
                                                            {
                                                                let lifecycle_status = match status {
                                                                                CronExecutionStatus::Success => ServiceLifecycleStatus::ExitedSuccessfully,
                                                                                CronExecutionStatus::Failed(_) | CronExecutionStatus::OverlapError => ServiceLifecycleStatus::ExitedWithError,
                                                                            };
                                                                if let Err(err) =
                                                                    state_file.set(
                                                                        &service_hash,
                                                                        lifecycle_status,
                                                                        None,
                                                                        exit_code,
                                                                        None,
                                                                    )
                                                                {
                                                                    warn!(
                                                                        "Failed to persist cron job '{}' exit state: {}",
                                                                        job_name_clone,
                                                                        err
                                                                    );
                                                                }
                                                            }
                                                        }
                                                        Err(e) => {
                                                            error!(
                                                                "Error waiting for cron job '{}': {}",
                                                                job_name_clone, e
                                                            );
                                                            let metrics =
                                                                if let Ok(guard) =
                                                                    metrics_store_clone
                                                                        .try_read()
                                                                {
                                                                    guard
                                                                    .snapshot_unit(
                                                                        &service_hash,
                                                                    )
                                                                    .unwrap_or_default()
                                                                } else {
                                                                    vec![]
                                                                };

                                                            cron_manager_clone.mark_job_completed_by_hash(
                                                                            &service_hash,
                                                                            CronExecutionStatus::Failed(
                                                                                e.to_string(),
                                                                            ),
                                                                            None,
                                                                            metrics,
                                                                        );
                                                            if let Ok(mut state_file) = ServiceStateFile::load()
                                                                            && let Err(err) = state_file.set(
                                                                                &service_hash,
                                                                                ServiceLifecycleStatus::ExitedWithError,
                                                                                None,
                                                                                None,
                                                                                None,
                                                                            )
                                                                        {
                                                                            warn!("Failed to persist cron job '{}' error state: {}", job_name_clone, err);
                                                                        }
                                                        }
                                                    }
                                                    if let Ok(mut pid_file) =
                                                        PidFile::load()
                                                        && let Err(err) = pid_file
                                                            .remove(&job_name_clone)
                                                    {
                                                        debug!(
                                                            "Failed to remove cron job '{}' from PID file: {}",
                                                            job_name_clone, err
                                                        );
                                                    }
                                                } else {
                                                    let already_completed = if let Ok(
                                                        state_file,
                                                    ) =
                                                        ServiceStateFile::load()
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
                                                                        .annotate_job_execution_by_hash(
                                                                            &service_hash,
                                                                            None,
                                                                            user.clone(),
                                                                            command.clone(),
                                                                        );
                                                        let metrics = if let Ok(guard) =
                                                            metrics_store_clone.try_read()
                                                        {
                                                            guard
                                                                .snapshot_unit(
                                                                    &service_hash,
                                                                )
                                                                .unwrap_or_default()
                                                        } else {
                                                            vec![]
                                                        };

                                                        cron_manager_clone
                                                            .mark_job_completed_by_hash(
                                                            &service_hash,
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
                                                        cron_manager_clone.mark_job_completed_by_hash(
                                                                        &service_hash,
                                                                        CronExecutionStatus::Failed(
                                                                            "Failed to get PID from PID file"
                                                                                .to_string(),
                                                                        ),
                                                                        None,
                                                                        vec![],
                                                                    );
                                                        if let Ok(mut state_file) =
                                                                        ServiceStateFile::load()
                                                                        && let Err(err) = state_file.set(
                                                                            &service_hash,
                                                                            ServiceLifecycleStatus::ExitedWithError,
                                                                            None,
                                                                            None,
                                                                            None,
                                                                        )
                                                                    {
                                                                        warn!("Failed to persist cron job '{}' error state: {}", job_name_clone, err);
                                                                    }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                error!(
                                                    "Failed to reload PID file for cron job '{}': {}",
                                                    job_name_clone, e
                                                );
                                                cron_manager_clone
                                                    .annotate_job_execution_by_hash(
                                                        &service_hash,
                                                        None,
                                                        user.clone(),
                                                        command.clone(),
                                                    );
                                                cron_manager_clone.mark_job_completed_by_hash(
                                                                &service_hash,
                                                                CronExecutionStatus::Failed(
                                                                    format!(
                                                                        "Failed to reload PID file: {}",
                                                                        e
                                                                    ),
                                                                ),
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
                                            .annotate_job_execution_by_hash(
                                                &service_hash,
                                                None,
                                                user.clone(),
                                                command.clone(),
                                            );
                                        cron_manager_clone.mark_job_completed_by_hash(
                                            &service_hash,
                                            CronExecutionStatus::Failed(e.to_string()),
                                            None,
                                            vec![],
                                        );
                                    }
                                }
                            });
                        }
                    }
                }
            }
        });

        if let Ok(socket_path) = ipc::socket_path() {
            info!("systemg supervisor listening on {:?}", socket_path);
        }

        let mut shutdown_requested = false;
        listener.set_nonblocking(false)?;

        while !shutdown_requested {
            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    if let Err(err) = ipc::authenticate_peer(&stream) {
                        warn!("Rejected unauthorized control connection: {err}");
                        let _ = ipc::write_response(
                            &mut stream,
                            &ControlResponse::Error(err.to_string()),
                        );
                        continue;
                    }
                    match ipc::read_command(&mut stream) {
                        Ok(command) => {
                            let should_shutdown =
                                matches!(command, ControlCommand::Shutdown);
                            debug!("Supervisor received command: {:?}", command);
                            if let ControlCommand::Logs {
                                service,
                                project,
                                lines,
                                kind,
                                follow,
                            } = command
                            {
                                let snapshot = match self.collect_configured_snapshot() {
                                    Ok(snapshot) => {
                                        self.status_cache.replace(snapshot.clone());
                                        snapshot
                                    }
                                    Err(err) => {
                                        error!("Supervisor logs command failed: {err}");
                                        let _ = writeln!(stream, "{err}");
                                        continue;
                                    }
                                };
                                let pid_file = self.daemon.pid_file_handle();
                                let mut log_stream = match stream.try_clone() {
                                    Ok(stream) => stream,
                                    Err(err) => {
                                        error!(
                                            "Failed to clone supervisor log stream: {err}"
                                        );
                                        continue;
                                    }
                                };
                                thread::spawn(move || {
                                    let request = SupervisorLogRequest {
                                        snapshot,
                                        pid_file,
                                        service,
                                        project,
                                        lines,
                                        kind: kind.as_deref(),
                                        follow,
                                        stream: &log_stream,
                                    };
                                    if let Err(err) =
                                        Supervisor::handle_logs_command(request)
                                    {
                                        error!("Supervisor logs command failed: {err}");
                                        let _ = writeln!(log_stream, "{err}");
                                    }
                                });
                                continue;
                            }
                            match self.handle_command(command) {
                                Ok(response) => {
                                    if let Err(err) =
                                        ipc::write_response(&mut stream, &response)
                                    {
                                        error!(
                                            "Failed to write supervisor response: {err}"
                                        );
                                    }
                                    if should_shutdown {
                                        shutdown_requested = true;
                                    }
                                }
                                Err(err) => {
                                    error!("Supervisor command failed: {err}");
                                    let _ = ipc::write_response(
                                        &mut stream,
                                        &ControlResponse::Error(err.to_string()),
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            warn!("Invalid supervisor command: {err}");
                            let _ = ipc::write_response(
                                &mut stream,
                                &ControlResponse::Error(err.to_string()),
                            );
                        }
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    error!("Supervisor listener error: {err}");
                    shutdown_requested = true;
                }
            }
        }

        self.shutdown_runtime()?;
        Ok(())
    }

    /// Streams logs through the supervisor-owned control socket.
    fn handle_logs_command(
        request: SupervisorLogRequest<'_>,
    ) -> Result<(), SupervisorError> {
        let manager = LogManager::new(request.pid_file);
        let requested_kind = request.kind;

        if let Some(service_name) = request.service {
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
                return manager
                    .stream_log_to_socket(
                        &unit.name,
                        pid,
                        request.lines,
                        requested_kind,
                        request.follow,
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

            let combined_exists = get_service_log_path(&service_name).exists();
            let stdout_exists = resolve_log_path(&service_name, "stdout").exists();
            let stderr_exists = resolve_log_path(&service_name, "stderr").exists();
            if combined_exists || stdout_exists || stderr_exists {
                return manager
                    .stream_log_to_socket(
                        &service_name,
                        None,
                        request.lines,
                        requested_kind,
                        request.follow,
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
        let render_project_groups = project_groups.len() > 1
            || project_groups
                .iter()
                .any(|(label, _)| label.as_str() != "Ungrouped");

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

                if pid.is_some() {
                    running_units.push((unit.name.clone(), pid));
                } else {
                    offline_units.push((unit.name.clone(), pid));
                }
            }

            running_units.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            running_units.dedup_by(|left, right| left.0 == right.0);
            offline_units.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            offline_units.dedup_by(|left, right| left.0 == right.0);

            for (section, units) in [
                (LogSection::Running, running_units),
                (LogSection::Offline, offline_units),
            ] {
                if units.is_empty() {
                    continue;
                }

                write_log_section_header(request.stream.try_clone()?, section)?;

                for (service_name, pid) in units {
                    manager.stream_log_to_socket(
                        &service_name,
                        pid,
                        request.lines,
                        requested_kind,
                        false,
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
            ControlCommand::Start { service, project } => {
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
                    self.daemon.start_services_blocking()?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message("All services started".into()))
                }
            }
            ControlCommand::AddProject {
                config,
                service,
                mode,
            } => {
                let project_id =
                    self.add_project_config(Path::new(&config), service, mode)?;
                Ok(ControlResponse::Message(format!(
                    "Project '{project_id}' loaded"
                )))
            }
            ControlCommand::StopProject { project } => {
                self.stop_project(&project)?;
                self.refresh_status_cache();
                Ok(ControlResponse::Message(format!(
                    "Project '{project}' stopped"
                )))
            }
            ControlCommand::Stop { service, project } => {
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
                    if let Some(config_path) = config.as_deref() {
                        runtime::ensure_trusted_config(Path::new(config_path))?;
                        let config = load_config(Some(config_path))?;
                        if config.project.id != project_id {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                format!(
                                    "project '{project_id}' does not match config project '{}'",
                                    config.project.id
                                ),
                            )
                            .into());
                        }
                    }
                    self.restart_project_target(
                        project_id,
                        config.as_deref().map(Path::new),
                    )?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message(format!(
                        "Project '{project_id}' restarted"
                    )))
                } else {
                    let target_path = config
                        .as_ref()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| self.config_path.clone());
                    self.reload_config(&target_path)?;
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
                for project in self.extra_projects.values() {
                    project.daemon.stop_services()?;
                    project.daemon.shutdown_monitor();
                }
                self.daemon.stop_services()?;
                self.refresh_status_cache();
                Ok(ControlResponse::Message("Supervisor shutting down".into()))
            }
            ControlCommand::Status { live } => {
                let snapshot = if live {
                    self.collect_live_snapshot_for_request()?
                } else {
                    self.collect_configured_snapshot()?
                };
                self.status_cache.replace(snapshot.clone());
                Ok(ControlResponse::Status(snapshot))
            }
        }
    }

    /// Handles handle spawn.
    fn handle_spawn(&mut self, params: SpawnParams) -> Result<u32, SupervisorError> {
        let Some(program) = params.command.first() else {
            return Err(SupervisorError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "spawn command must not be empty",
            )));
        };

        let spawn_auth = self
            .spawn_manager
            .authorize_spawn(params.parent_pid, &params.name)?;
        let depth = spawn_auth.depth;

        let mut cmd = std::process::Command::new(program);
        if params.command.len() > 1 {
            cmd.args(&params.command[1..]);
        }

        cmd.env("SPAWN_DEPTH", depth.to_string());
        cmd.env("SPAWN_PARENT_PID", params.parent_pid.to_string());

        if let Some(log_level) = params.log_level {
            cmd.env("RUST_LOG", log_level);
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;
        let child_pid = child.id();
        let command_string = params.command.join(" ");
        let child_name = params.name.clone();
        let started_at = SystemTime::now();

        let spawned_child = SpawnedChild {
            name: child_name.clone(),
            pid: child_pid,
            parent_pid: params.parent_pid,
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
            params.parent_pid,
            spawned_child,
            spawn_auth.root_service.clone(),
        )?;
        let effective_root = root_service.or(spawn_auth.root_service);

        let echo_to_console = !self.detach_children;
        if let Some(stdout) = child.stdout.take() {
            spawn_dynamic_child_log_writer(
                effective_root.as_deref(),
                &child_name,
                child_pid,
                stdout,
                "stdout",
                echo_to_console,
            );
        }

        if let Some(stderr) = child.stderr.take() {
            spawn_dynamic_child_log_writer(
                effective_root.as_deref(),
                &child_name,
                child_pid,
                stderr,
                "stderr",
                echo_to_console,
            );
        }

        let pid_file_handle = self.daemon.pid_file_handle();
        if let Ok(mut pid_file) = pid_file_handle.lock() {
            let service_hash = effective_root
                .as_deref()
                .and_then(|name| self.daemon.get_service_hash(name));
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

        let spawn_manager_for_exit = self.spawn_manager.clone();
        let pid_file_for_exit = Arc::clone(&pid_file_handle);
        let child_name_for_exit = child_name.clone();
        thread::spawn(move || match child.wait() {
            Ok(status) => {
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

                    for descendant in removed.iter().filter(|c| c.parent_pid == child_pid)
                    {
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
                error!("Failed to wait for spawned child {child_pid}: {err}");
            }
        });

        info!(
            "Spawned child '{}' (PID: {}) from parent {}",
            child_name, child_pid, params.parent_pid
        );

        Ok(child_pid)
    }

    /// Reloads config.
    fn reload_config(&mut self, path: &Path) -> Result<(), SupervisorError> {
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.config_path
                .parent()
                .unwrap_or_else(|| Path::new("/"))
                .join(path)
        };

        info!("Reloading configuration from {:?}", resolved);
        runtime::ensure_trusted_config(&resolved)?;
        let config = load_config(Some(resolved.to_string_lossy().as_ref()))?;

        if let Some(collector) = self.metrics_collector.take() {
            collector.stop();
        }
        if let Some(refresher) = self.status_refresher.take() {
            refresher.stop();
        }

        self.daemon.stop_services()?;
        self.daemon.shutdown_monitor();

        let metrics_settings = config
            .metrics
            .to_settings(config.project_dir.as_deref().map(Path::new));
        self.daemon = Daemon::from_config(config.clone(), self.detach_children)?;
        self.config_path = resolved;

        self.sync_cron_projects()?;

        self.daemon.start_services()?;
        self.daemon.ensure_monitoring()?;

        self.metrics_store = metrics::shared_store(metrics_settings)?;
        let metrics_handle = self.metrics_store.clone();

        let config_handle = self.daemon.config();
        let pid_handle = self.daemon.pid_file_handle();
        let state_handle = self.daemon.service_state_handle();

        if let Ok(mut snapshot) = collect_runtime_snapshot(
            Arc::clone(&config_handle),
            &pid_handle,
            &state_handle,
            Some(&self.metrics_store),
            Some(&self.spawn_manager),
            Self::status_snapshot_mode(config_handle.as_ref()),
        ) {
            Self::apply_project_metadata(
                &mut snapshot,
                self.primary_project_mode,
                &self.config_path,
            );
            self.status_cache.replace(snapshot);
        }

        let cache_clone = self.status_cache.clone();
        let refresh_config = Arc::clone(&config_handle);
        let refresh_pid = Arc::clone(&pid_handle);
        let refresh_state = Arc::clone(&state_handle);
        let refresh_metrics = self.metrics_store.clone();
        let refresh_spawn_manager = self.spawn_manager.clone();
        let refresh_interval = Self::status_snapshot_interval(config_handle.as_ref());
        let refresh_mode = Self::status_snapshot_mode(config_handle.as_ref());
        let refresh_project_mode = self.primary_project_mode;
        let refresh_config_path = self.config_path.clone();
        if !matches!(refresh_mode, StatusSnapshotMode::Off) {
            self.status_refresher = Some(StatusRefresher::spawn(
                cache_clone,
                refresh_interval,
                move || {
                    let mut snapshot = collect_runtime_snapshot(
                        Arc::clone(&refresh_config),
                        &refresh_pid,
                        &refresh_state,
                        Some(&refresh_metrics),
                        Some(&refresh_spawn_manager),
                        refresh_mode,
                    )?;
                    Supervisor::apply_project_metadata(
                        &mut snapshot,
                        refresh_project_mode,
                        &refresh_config_path,
                    );
                    Ok(snapshot)
                },
            ));
        }

        self.metrics_collector = Some(MetricsCollector::spawn(
            metrics_handle,
            config_handle,
            pid_handle,
            state_handle,
        ));
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
        runtime::ensure_trusted_config(&resolved)?;
        let config = load_config(Some(resolved.to_string_lossy().as_ref()))?;
        let project_id = config.project.id.clone();
        let primary_project = self.daemon.config().project.id.clone();

        if project_id == primary_project {
            self.primary_project_mode = mode;
            Self::start_project_services(
                &self.daemon,
                self.daemon.config().as_ref(),
                service_filter.as_deref(),
                &self.spawn_manager,
            )?;
            self.sync_cron_projects()?;
            self.refresh_status_cache();
            return Ok(project_id);
        }

        if !self.extra_projects.contains_key(&project_id) {
            Self::register_spawn_limits_for_config(&self.spawn_manager, &config)?;
            let mut daemon = Daemon::new(
                config.clone(),
                self.daemon.pid_file_handle(),
                self.daemon.service_state_handle(),
                self.detach_children,
            );
            daemon.set_pipe_stderr(self.pipe_stderr);
            self.extra_projects.insert(
                project_id.clone(),
                ProjectRuntime {
                    daemon,
                    mode,
                    config_path: resolved.clone(),
                },
            );
        } else if let Some(project) = self.extra_projects.get_mut(&project_id) {
            project.mode = mode;
            project.config_path = resolved.clone();
        }

        let project = self.extra_projects.get(&project_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' was not registered"),
            )
        })?;
        Self::start_project_services(
            &project.daemon,
            project.daemon.config().as_ref(),
            service_filter.as_deref(),
            &self.spawn_manager,
        )?;
        self.sync_cron_projects()?;

        if let Some(refresher) = self.status_refresher.take() {
            refresher.stop();
        }
        self.refresh_status_cache();
        Ok(project_id)
    }

    /// Restarts one service in the selected project without reloading unrelated projects.
    fn restart_single_service_target(
        &self,
        selector: &str,
        project: Option<&str>,
        config_path: Option<&Path>,
    ) -> Result<(), SupervisorError> {
        let (selector_project, service_name) = split_project_selector(selector)
            .map(|(project_id, service_name)| (Some(project_id), service_name))
            .unwrap_or((None, selector));

        let override_config = if let Some(path) = config_path {
            runtime::ensure_trusted_config(path)?;
            Some(load_config(Some(path.to_string_lossy().as_ref()))?)
        } else {
            None
        };
        let config_project = override_config
            .as_ref()
            .map(|config| config.project.id.as_str());
        let target_project = self.resolve_service_target_project(
            service_name,
            project,
            selector_project,
            config_project,
        )?;
        let primary_project = self.daemon.config().project.id.clone();

        if target_project == primary_project {
            let config_handle = self.daemon.config();
            let service_config = override_config
                .as_ref()
                .and_then(|config| config.services.get(service_name))
                .or_else(|| config_handle.services.get(service_name))
                .ok_or_else(|| ProcessManagerError::DependencyError {
                    service: service_name.into(),
                    dependency: "service not defined".into(),
                })?;
            self.daemon.restart_service(service_name, service_config)?;
            return Ok(());
        }

        let Some(project_runtime) = self.extra_projects.get(&target_project) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{target_project}' is not managed by this supervisor"),
            )
            .into());
        };
        let config_handle = project_runtime.daemon.config();
        let service_config = override_config
            .as_ref()
            .and_then(|config| config.services.get(service_name))
            .or_else(|| config_handle.services.get(service_name))
            .ok_or_else(|| ProcessManagerError::DependencyError {
                service: service_name.into(),
                dependency: "service not defined".into(),
            })?;
        project_runtime
            .daemon
            .restart_service(service_name, service_config)?;
        Ok(())
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

        let target_project = self.resolve_service_target_project(
            service_name,
            project,
            selector_project,
            None,
        )?;
        let primary_project = self.daemon.config().project.id.clone();

        if target_project == primary_project {
            self.daemon.stop_service(service_name)?;
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

        project_runtime.daemon.stop_service(service_name)?;
        Ok((target_project, service_name.to_string()))
    }

    /// Handles refresh status cache.
    fn refresh_status_cache(&mut self) {
        match self.collect_aggregate_snapshot(false) {
            Ok(snapshot) => self.status_cache.replace(snapshot),
            Err(err) => error!("failed to refresh status snapshot: {err}"),
        }
    }

    /// Stops every service in one managed project.
    fn stop_project(&mut self, project_id: &str) -> Result<(), SupervisorError> {
        let primary_project = self.daemon.config().project.id.clone();
        if project_id == primary_project {
            let services: Vec<String> =
                self.daemon.config().services.keys().cloned().collect();
            for service in services {
                self.daemon.stop_service(&service)?;
            }
            return Ok(());
        }

        let Some(project) = self.extra_projects.get(project_id) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("project '{project_id}' is not managed by this supervisor"),
            )
            .into());
        };
        let services: Vec<String> =
            project.daemon.config().services.keys().cloned().collect();
        for service in services {
            project.daemon.stop_service(&service)?;
        }
        project.daemon.shutdown_monitor();
        self.extra_projects.remove(project_id);
        self.sync_cron_projects()?;
        Ok(())
    }

    /// Stops every service in every project managed by the supervisor.
    fn stop_all_projects(&mut self) -> Result<(), SupervisorError> {
        let extra_projects: Vec<String> = self.extra_projects.keys().cloned().collect();
        for project_id in extra_projects {
            self.stop_project(&project_id)?;
        }

        self.daemon.stop_services()?;
        Ok(())
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
        for project in self.extra_projects.values() {
            project.daemon.stop_services()?;
            project.daemon.shutdown_monitor();
        }
        self.daemon.stop_services()?;
        self.daemon.shutdown_monitor();
        ipc::cleanup_runtime()?;
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
    fn wait_for_cron_completion(
        pid: u32,
        job_name: &str,
    ) -> Result<CronCompletionOutcome, SupervisorError> {
        Self::wait_for_cron_completion_with_timeout(
            pid,
            job_name,
            Duration::from_secs(3600),
            Duration::from_millis(100),
        )
    }

    fn wait_for_cron_completion_with_timeout(
        pid: u32,
        job_name: &str,
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
                    debug!(
                        "Cron job '{}' already reaped before wait, assuming success",
                        job_name
                    );
                    return Ok(CronCompletionOutcome {
                        status: CronExecutionStatus::Success,
                        exit_code: Some(0),
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

    fn test_service(depends_on: &[&str]) -> ServiceConfig {
        ServiceConfig {
            command: "/bin/true".into(),
            depends_on: if depends_on.is_empty() {
                None
            } else {
                Some(depends_on.iter().map(|dep| dep.to_string()).collect())
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
            version: Version::V1,
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
            version: Version::V1,
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
version: "1"
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
version: "1"
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
version: "1"
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
version: "1"
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
            })
            .expect_err("direct cron unit start should be rejected");
        assert!(
            err.to_string()
                .contains("cron unit 'beta_cron' cannot be started directly"),
            "unexpected cron start error: {err}"
        );

        supervisor
            .handle_command(ControlCommand::Restart {
                config: Some(beta_config.to_string_lossy().to_string()),
                service: Some("beta_worker".into()),
                project: None,
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
version: "1"
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
version: "1"
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
            .and_then(|project| project.daemon.get_service_hash("beta_cron"))
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
                    && project
                        .config
                        .services
                        .get("beta_cron")
                        .is_some_and(|service| service.compute_hash() == beta_hash)
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
version: "1"
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
version: "1"
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
            })
            .expect("add beta project");

        let alpha_hash = supervisor
            .daemon
            .get_service_hash("alpha_cron")
            .expect("alpha cron hash");
        let beta_hash = supervisor
            .extra_projects
            .get("beta")
            .and_then(|project| project.daemon.get_service_hash("beta_cron"))
            .expect("beta cron hash");

        let cron_state = crate::cron::CronStateFile::load().expect("load cron state");
        assert!(
            cron_state.jobs().contains_key(&alpha_hash),
            "alpha cron should be persisted before aggregate status"
        );
        assert!(
            cron_state.jobs().contains_key(&beta_hash),
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

        let cron_state = crate::cron::CronStateFile::load()
            .expect("load cron state after aggregate status");
        assert!(
            cron_state.jobs().contains_key(&alpha_hash),
            "aggregate status should not prune primary project cron state"
        );
        assert!(
            cron_state.jobs().contains_key(&beta_hash),
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
version: "1"
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
version: "1"
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
version: "1"
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
version: "1"
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
            })
            .expect("add beta project");

        let beta_hash = supervisor
            .extra_projects
            .get("beta")
            .and_then(|project| project.daemon.get_service_hash("beta_cron"))
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

        let cron_state =
            crate::cron::CronStateFile::load().expect("load cron state before stop");
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
            })
            .expect("stop beta project");

        assert!(
            !supervisor
                .get_cron_jobs()
                .iter()
                .any(|job| job.service_name == "beta_cron"),
            "stopped beta cron should leave active scheduler routing"
        );
        let cron_state =
            crate::cron::CronStateFile::load().expect("load cron state after stop");
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
}
