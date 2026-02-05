#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    fs, io,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, SystemTime},
};

use thiserror::Error;
use tracing::{debug, error, info, warn};

use crate::{
    config::{SkipConfig, SpawnMode, TerminationPolicy, load_config},
    cron::{CronExecutionStatus, CronManager},
    daemon::{
        Daemon, PersistedSpawnChild, ServiceLifecycleStatus, ServiceReadyState,
        ServiceStateFile,
    },
    error::ProcessManagerError,
    ipc::{self, ControlCommand, ControlResponse, InspectPayload},
    logs::spawn_dynamic_child_log_writer,
    metrics::{self, MetricsCollector, MetricsHandle},
    spawn::{DynamicSpawnManager, SpawnedChild, SpawnedExit},
    status::{StatusCache, StatusRefresher, StatusSnapshot, collect_runtime_snapshot},
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
}

/// Parameters for spawning a child process.
struct SpawnParams {
    parent_pid: u32,
    name: String,
    command: Vec<String>,
    ttl: Option<u64>,
}

#[derive(Debug, Clone)]
struct CronCompletionOutcome {
    status: CronExecutionStatus,
    exit_code: Option<i32>,
}

impl Supervisor {
    /// Creates supervisor with config.
    pub fn new(
        config_path: PathBuf,
        detach_children: bool,
        service_filter: Option<String>,
    ) -> Result<Self, SupervisorError> {
        let config = load_config(Some(config_path.to_string_lossy().as_ref()))?;
        let cron_manager = CronManager::new();
        cron_manager.sync_from_config(&config)?;

        let daemon = Daemon::from_config(config.clone(), detach_children)?;
        let config_arc = daemon.config();
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
        })
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
        let socket_path = ipc::socket_path()?;
        if socket_path.exists() {
            fs::remove_file(&socket_path)?;
        }

        let listener = UnixListener::bind(&socket_path)?;
        ipc::write_supervisor_pid(unsafe { libc::getpid() })?;

        // Only start non-cron services
        let config = load_config(Some(self.config_path.to_string_lossy().as_ref()))?;
        for (service_name, service_config) in &config.services {
            // Skip if service_filter is set and doesn't match
            if let Some(ref filter) = self.service_filter
                && service_name != filter
            {
                continue;
            }

            if service_config.cron.is_none() {
                if let Some(skip_config) = &service_config.skip {
                    match skip_config {
                        SkipConfig::Flag(true) => {
                            info!("Skipping service '{service_name}' due to skip flag");
                            if let Err(err) =
                                self.daemon.mark_service_skipped(service_name)
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
                                .evaluate_skip_condition(service_name, skip_command)
                            {
                                Ok(true) => {
                                    info!(
                                        "Skipping service '{service_name}' due to skip condition"
                                    );
                                    if let Err(err) =
                                        self.daemon.mark_service_skipped(service_name)
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

                // Only start services that are not cron jobs
                self.daemon.start_service(service_name, service_config)?;

                if let Some(ref spawn) = service_config.spawn
                    && let Some(SpawnMode::Dynamic) = spawn.mode
                    && let Ok(pid_file) = self.daemon.pid_file_handle().lock()
                    && let Some(&pid) = pid_file.services().get(service_name)
                {
                    self.spawn_manager
                        .register_service_pid(service_name.clone(), pid);
                }
            }
        }

        // Ensure the background monitor thread is watching any long-lived
        // processes we just started so that exits are reaped and lifecycle
        // state stays accurate.
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
        ) {
            Ok(snapshot) => self.status_cache.replace(snapshot),
            Err(err) => error!("failed to build initial status snapshot: {err}"),
        }

        let cache_clone = self.status_cache.clone();
        let config_for_refresh = Arc::clone(&config_handle);
        let pid_for_refresh = Arc::clone(&pid_handle);
        let state_for_refresh = Arc::clone(&state_handle);
        let metrics_for_refresh = self.metrics_store.clone();
        let spawn_manager_for_refresh = self.spawn_manager.clone();
        self.status_refresher = Some(StatusRefresher::spawn(
            cache_clone,
            Duration::from_secs(1),
            move || {
                collect_runtime_snapshot(
                    Arc::clone(&config_for_refresh),
                    &pid_for_refresh,
                    &state_for_refresh,
                    Some(&metrics_for_refresh),
                    Some(&spawn_manager_for_refresh),
                )
            },
        ));

        let metrics_handle = self.metrics_store.clone();
        self.metrics_collector = Some(MetricsCollector::spawn(
            metrics_handle,
            Arc::clone(&config_handle),
            pid_handle,
            state_handle,
        ));

        // Spawn cron checker thread
        let cron_manager = self.cron_manager.clone();
        let config_path = self.config_path.clone();
        let detach_children = self.detach_children;
        let metrics_store = self.metrics_store.clone();

        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(1));

                let due_jobs = cron_manager.get_due_jobs();
                if !due_jobs.is_empty() {
                    let config_result =
                        load_config(Some(config_path.to_string_lossy().as_ref()));
                    if let Ok(cfg) = config_result {
                        for job_name in due_jobs {
                            if let Some(service_config) =
                                cfg.services.get(&job_name).cloned()
                            {
                                info!("Running cron job '{}'", job_name);

                                // Spawn a thread to run and monitor the cron job
                                let cron_manager_clone = cron_manager.clone();
                                let job_name_clone = job_name.clone();
                                let cfg_clone = cfg.clone();
                                let metrics_store_clone = metrics_store.clone();
                                let service_hash = service_config.compute_hash();

                                thread::spawn(move || {
                                    use crate::daemon::PidFile;

                                    // Run the cron job and wait for completion
                                    match Daemon::from_config(
                                        cfg_clone.clone(),
                                        detach_children,
                                    ) {
                                        Ok(daemon) => {
                                            match daemon.start_service(
                                                &job_name_clone,
                                                &service_config,
                                            ) {
                                                Ok(
                                                    ServiceReadyState::CompletedSuccess,
                                                ) => {
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

                                                    cron_manager_clone
                                                        .mark_job_completed(
                                                            &job_name_clone,
                                                            CronExecutionStatus::Success,
                                                            Some(0),
                                                            metrics,
                                                        );

                                                    // Persist exit state to ServiceStateFile for status display
                                                    if let Some(service_hash) = daemon.get_service_hash(&job_name_clone)
                                                        && let Ok(mut state_file) =
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
                                                    // Give the process a moment to register in the PID file
                                                    thread::sleep(Duration::from_millis(
                                                        50,
                                                    ));

                                                    // Reload the PID file to get the process ID
                                                    match PidFile::reload() {
                                                        Ok(pid_file) => {
                                                            if let Some(pid) = pid_file
                                                                .get(&job_name_clone)
                                                            {
                                                                // Wait for the process to complete
                                                                let result = Self::wait_for_cron_completion(
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

                                                                        let metrics = if let Ok(guard) = metrics_store_clone.try_read() {
                                                                            guard.snapshot_unit(&service_hash).unwrap_or_default()
                                                                        } else {
                                                                            vec![]
                                                                        };

                                                                        cron_manager_clone.mark_job_completed(
                                                                            &job_name_clone,
                                                                            status.clone(),
                                                                            exit_code,
                                                                            metrics,
                                                                        );

                                                                        // Persist exit state to ServiceStateFile for status display
                                                                        if let Some(service_hash) = daemon.get_service_hash(&job_name_clone)
                                                                            && let Ok(mut state_file) = ServiceStateFile::load()
                                                                        {
                                                                            let lifecycle_status = match status {
                                                                                CronExecutionStatus::Success => ServiceLifecycleStatus::ExitedSuccessfully,
                                                                                CronExecutionStatus::Failed(_) | CronExecutionStatus::OverlapError => ServiceLifecycleStatus::ExitedWithError,
                                                                            };
                                                                            if let Err(err) = state_file.set(
                                                                                &service_hash,
                                                                                lifecycle_status,
                                                                                None,
                                                                                exit_code,
                                                                                None,
                                                                            ) {
                                                                                warn!("Failed to persist cron job '{}' exit state: {}", job_name_clone, err);
                                                                            }
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        error!(
                                                                            "Error waiting for cron job '{}': {}",
                                                                            job_name_clone, e
                                                                        );
                                                                        let metrics = if let Ok(guard) = metrics_store_clone.try_read() {
                                                                            guard.snapshot_unit(&service_hash).unwrap_or_default()
                                                                        } else {
                                                                            vec![]
                                                                        };

                                                                        cron_manager_clone.mark_job_completed(
                                                                            &job_name_clone,
                                                                            CronExecutionStatus::Failed(
                                                                                e.to_string(),
                                                                            ),
                                                                            None,
                                                                            metrics,
                                                                        );

                                                                        // Persist error state to ServiceStateFile
                                                                        if let Some(service_hash) = daemon.get_service_hash(&job_name_clone)
                                                                            && let Ok(mut state_file) = ServiceStateFile::load()
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

                                                                // Clean up the PID file entry without changing lifecycle status
                                                                // (state has already been persisted above)
                                                                if let Ok(mut pid_file) = PidFile::load()
                                                                    && let Err(err) = pid_file.remove(&job_name_clone) {
                                                                    debug!("Failed to remove cron job '{}' from PID file: {}", job_name_clone, err);
                                                                }
                                                            } else {
                                                                // PID not found - check if the job already completed successfully
                                                                // This can happen when a cron job completes very quickly
                                                                let already_completed = if let Some(service_hash) = daemon.get_service_hash(&job_name_clone)
                                                                    && let Ok(state_file) = ServiceStateFile::load()
                                                                    && let Some(entry) = state_file.get(&service_hash)
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
                                                                    // Job completed successfully before we could track it
                                                                    let metrics = if let Ok(guard) = metrics_store_clone.try_read() {
                                                                        guard.snapshot_unit(&service_hash).unwrap_or_default()
                                                                    } else {
                                                                        vec![]
                                                                    };

                                                                    cron_manager_clone.mark_job_completed(
                                                                        &job_name_clone,
                                                                        CronExecutionStatus::Success,
                                                                        Some(0),
                                                                        metrics,
                                                                    );
                                                                } else {
                                                                    error!(
                                                                        "Failed to find PID for cron job '{}' in PID file and job has not completed",
                                                                        job_name_clone
                                                                    );
                                                                    // No metrics available since process didn't start
                                                                    cron_manager_clone.mark_job_completed(
                                                                        &job_name_clone,
                                                                        CronExecutionStatus::Failed(
                                                                            "Failed to get PID from PID file"
                                                                                .to_string(),
                                                                        ),
                                                                        None,
                                                                        vec![],
                                                                    );

                                                                    // Persist error state to ServiceStateFile
                                                                    if let Some(service_hash) = daemon.get_service_hash(&job_name_clone)
                                                                        && let Ok(mut state_file) =
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
                                                            // No metrics available since process didn't start
                                                            cron_manager_clone.mark_job_completed(
                                                                &job_name_clone,
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
                                                        "Failed to start cron job '{}': {}",
                                                        job_name_clone, e
                                                    );
                                                    // No metrics available since process didn't start properly
                                                    cron_manager_clone
                                                        .mark_job_completed(
                                                            &job_name_clone,
                                                            CronExecutionStatus::Failed(
                                                                e.to_string(),
                                                            ),
                                                            None,
                                                            vec![],
                                                        );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!(
                                                "Failed to create daemon for cron job '{}': {}",
                                                job_name_clone, e
                                            );
                                            // No metrics available since daemon creation failed
                                            cron_manager_clone.mark_job_completed(
                                                &job_name_clone,
                                                CronExecutionStatus::Failed(
                                                    e.to_string(),
                                                ),
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
            }
        });

        info!("systemg supervisor listening on {:?}", socket_path);

        let mut shutdown_requested = false;
        listener.set_nonblocking(false)?;

        while !shutdown_requested {
            match listener.accept() {
                Ok((mut stream, _addr)) => match ipc::read_command(&mut stream) {
                    Ok(command) => {
                        let should_shutdown = matches!(command, ControlCommand::Shutdown);
                        debug!("Supervisor received command: {:?}", command);
                        match self.handle_command(command) {
                            Ok(response) => {
                                if let Err(err) =
                                    ipc::write_response(&mut stream, &response)
                                {
                                    error!("Failed to write supervisor response: {err}");
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
                },
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

    fn handle_command(
        &mut self,
        command: ControlCommand,
    ) -> Result<ControlResponse, SupervisorError> {
        match command {
            ControlCommand::Start { service } => {
                if let Some(service_name) = service {
                    // Start a single service
                    let config = self.daemon.config();
                    if let Some(service_config) = config.services.get(&service_name) {
                        self.daemon.start_service(&service_name, service_config)?;

                        if let Some(ref spawn) = service_config.spawn
                            && let Some(SpawnMode::Dynamic) = spawn.mode
                            && let Ok(pid_file) = self.daemon.pid_file_handle().lock()
                            && let Some(&pid) = pid_file.services().get(&service_name)
                        {
                            self.spawn_manager
                                .register_service_pid(service_name.clone(), pid);
                        }

                        self.refresh_status_cache();
                        Ok(ControlResponse::Message(format!(
                            "Service '{service_name}' started"
                        )))
                    } else {
                        Ok(ControlResponse::Error(format!(
                            "Service '{service_name}' not found in configuration"
                        )))
                    }
                } else {
                    // Start all services
                    self.daemon.start_services_blocking()?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message("All services started".into()))
                }
            }
            ControlCommand::Stop { service } => {
                if let Some(service) = service {
                    self.daemon.stop_service(&service)?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message(format!(
                        "Service '{service}' stopped"
                    )))
                } else {
                    self.daemon.stop_services()?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message("All services stopped".into()))
                }
            }
            ControlCommand::Restart { config, service } => {
                if let Some(service) = service {
                    if let Some(path) = config {
                        self.reload_config(Path::new(&path))?;
                    }
                    self.restart_single_service(&service)?;
                    self.refresh_status_cache();
                    Ok(ControlResponse::Message(format!(
                        "Service '{service}' restarted"
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
            ControlCommand::Inspect { unit, samples } => {
                let limit = samples as usize;
                let snapshot = self.status_cache.snapshot();
                let matching_unit = snapshot
                    .units
                    .iter()
                    .find(|status| status.name == unit || status.hash == unit)
                    .cloned();

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
            ControlCommand::Spawn {
                parent_pid,
                name,
                command,
                ttl,
            } => {
                let params = SpawnParams {
                    parent_pid,
                    name,
                    command,
                    ttl,
                };
                match self.handle_spawn(params) {
                    Ok(pid) => Ok(ControlResponse::Spawned { pid }),
                    Err(err) => Ok(ControlResponse::Error(err.to_string())),
                }
            }
            ControlCommand::Shutdown => {
                self.daemon.stop_services()?;
                self.refresh_status_cache();
                Ok(ControlResponse::Message("Supervisor shutting down".into()))
            }
            ControlCommand::Status => {
                self.refresh_status_cache();
                Ok(ControlResponse::Status(self.status_cache.snapshot()))
            }
        }
    }

    fn handle_spawn(&mut self, params: SpawnParams) -> Result<u32, SupervisorError> {
        let spawn_auth = self
            .spawn_manager
            .authorize_spawn(params.parent_pid, &params.name)?;
        let depth = spawn_auth.depth;

        let mut cmd = std::process::Command::new(&params.command[0]);
        if params.command.len() > 1 {
            cmd.args(&params.command[1..]);
        }

        cmd.env("SPAWN_DEPTH", depth.to_string());
        cmd.env("SPAWN_PARENT_PID", params.parent_pid.to_string());
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

        self.cron_manager.sync_from_config(&config)?;

        self.daemon.start_services()?;
        self.daemon.ensure_monitoring()?;

        self.metrics_store = metrics::shared_store(metrics_settings)?;
        let metrics_handle = self.metrics_store.clone();

        let config_handle = self.daemon.config();
        let pid_handle = self.daemon.pid_file_handle();
        let state_handle = self.daemon.service_state_handle();

        if let Ok(snapshot) = collect_runtime_snapshot(
            Arc::clone(&config_handle),
            &pid_handle,
            &state_handle,
            Some(&self.metrics_store),
            Some(&self.spawn_manager),
        ) {
            self.status_cache.replace(snapshot);
        }

        let cache_clone = self.status_cache.clone();
        let refresh_config = Arc::clone(&config_handle);
        let refresh_pid = Arc::clone(&pid_handle);
        let refresh_state = Arc::clone(&state_handle);
        let refresh_metrics = self.metrics_store.clone();
        let refresh_spawn_manager = self.spawn_manager.clone();
        self.status_refresher = Some(StatusRefresher::spawn(
            cache_clone,
            Duration::from_secs(1),
            move || {
                collect_runtime_snapshot(
                    Arc::clone(&refresh_config),
                    &refresh_pid,
                    &refresh_state,
                    Some(&refresh_metrics),
                    Some(&refresh_spawn_manager),
                )
            },
        ));

        self.metrics_collector = Some(MetricsCollector::spawn(
            metrics_handle,
            config_handle,
            pid_handle,
            state_handle,
        ));
        Ok(())
    }

    fn restart_single_service(&self, name: &str) -> Result<(), SupervisorError> {
        let config = load_config(Some(self.config_path.to_string_lossy().as_ref()))?;
        let Some(service_config) = config.services.get(name) else {
            return Err(ProcessManagerError::DependencyError {
                service: name.into(),
                dependency: "service not defined".into(),
            }
            .into());
        };

        self.daemon.restart_service(name, service_config)?;
        Ok(())
    }

    fn refresh_status_cache(&mut self) {
        let config = self.daemon.config();
        let pid_handle = self.daemon.pid_file_handle();
        let state_handle = self.daemon.service_state_handle();

        match collect_runtime_snapshot(
            config,
            &pid_handle,
            &state_handle,
            Some(&self.metrics_store),
            Some(&self.spawn_manager),
        ) {
            Ok(snapshot) => self.status_cache.replace(snapshot),
            Err(err) => error!("failed to refresh status snapshot: {err}"),
        }
    }

    fn shutdown_runtime(&mut self) -> Result<(), SupervisorError> {
        if let Some(collector) = self.metrics_collector.take() {
            collector.stop();
        }
        if let Some(refresher) = self.status_refresher.take() {
            refresher.stop();
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
        use nix::{
            sys::wait::{WaitPidFlag, WaitStatus, waitpid},
            unistd::Pid,
        };

        let pid = Pid::from_raw(pid as i32);
        const MAX_WAIT_TIME: Duration = Duration::from_secs(3600); // 1 hour max
        const POLL_INTERVAL: Duration = Duration::from_millis(100);
        let start = std::time::Instant::now();

        loop {
            match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {
                    if start.elapsed() > MAX_WAIT_TIME {
                        warn!(
                            "Cron job '{}' exceeded maximum wait time of 1 hour",
                            job_name
                        );
                        return Err(SupervisorError::Process(
                            ProcessManagerError::ServiceStartError {
                                service: job_name.to_string(),
                                source: std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    "Cron job exceeded maximum wait time",
                                ),
                            },
                        ));
                    }

                    thread::sleep(POLL_INTERVAL);
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
                    thread::sleep(POLL_INTERVAL);
                }
                #[cfg(any(target_os = "linux", target_os = "android"))]
                Ok(WaitStatus::PtraceEvent(_, _, _))
                | Ok(WaitStatus::PtraceSyscall(_)) => {
                    thread::sleep(POLL_INTERVAL);
                }
                Err(nix::errno::Errno::ECHILD) => {
                    // Already reaped elsewhere; assume success.
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
