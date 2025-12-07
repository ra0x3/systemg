use std::{
    fs, io,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use tracing::{debug, error, info, warn};

use crate::{
    config::{SkipConfig, load_config},
    cron::{CronExecutionStatus, CronManager},
    daemon::{Daemon, ServiceReadyState},
    error::ProcessManagerError,
    ipc::{self, ControlCommand, ControlResponse},
};

use thiserror::Error;

/// Errors emitted by the resident supervisor runtime.
#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    Process(#[from] ProcessManagerError),
    #[error(transparent)]
    Control(#[from] ipc::ControlError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Long-lived supervisor that keeps `systemg` alive in daemon mode and reacts to CLI commands.
pub struct Supervisor {
    config_path: PathBuf,
    daemon: Daemon,
    detach_children: bool,
    cron_manager: CronManager,
    service_filter: Option<String>,
}

#[derive(Debug, Clone)]
struct CronCompletionOutcome {
    status: CronExecutionStatus,
    exit_code: Option<i32>,
}

impl Supervisor {
    /// Creates a new supervisor from a config path.
    pub fn new(
        config_path: PathBuf,
        detach_children: bool,
        service_filter: Option<String>,
    ) -> Result<Self, SupervisorError> {
        let config = load_config(Some(config_path.to_string_lossy().as_ref()))?;
        let cron_manager = CronManager::new();

        // Register cron jobs
        for (service_name, service_config) in &config.services {
            if let Some(cron_config) = &service_config.cron {
                cron_manager.register_job(service_name, cron_config)?;
                info!("Registered cron job for service '{}'", service_name);
            }
        }

        let daemon = Daemon::from_config(config, detach_children)?;
        Ok(Self {
            config_path,
            daemon,
            detach_children,
            cron_manager,
            service_filter,
        })
    }

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
                // Check skip flag before starting service
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
            }
        }

        // Ensure the background monitor thread is watching any long-lived
        // processes we just started so that exits are reaped and lifecycle
        // state stays accurate.
        self.daemon.ensure_monitoring()?;

        // Spawn cron checker thread
        let cron_manager = self.cron_manager.clone();
        let config_path = self.config_path.clone();
        let detach_children = self.detach_children;

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
                                                    cron_manager_clone
                                                        .mark_job_completed(
                                                            &job_name_clone,
                                                            CronExecutionStatus::Success,
                                                            Some(0),
                                                        );
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

                                                                        cron_manager_clone.mark_job_completed(
                                                                            &job_name_clone,
                                                                            status,
                                                                            exit_code,
                                                                        );
                                                                    }
                                                                    Err(e) => {
                                                                        error!(
                                                                            "Error waiting for cron job '{}': {}",
                                                                            job_name_clone, e
                                                                        );
                                                                        cron_manager_clone.mark_job_completed(
                                                                            &job_name_clone,
                                                                            CronExecutionStatus::Failed(
                                                                                e.to_string(),
                                                                            ),
                                                                            None,
                                                                        );
                                                                    }
                                                                }

                                                                // Clean up the PID file entry
                                                                let _ = daemon
                                                                    .stop_service(
                                                                        &job_name_clone,
                                                                    );
                                                            } else {
                                                                error!(
                                                                    "Failed to find PID for cron job '{}' in PID file",
                                                                    job_name_clone
                                                                );
                                                                cron_manager_clone.mark_job_completed(
                                                                    &job_name_clone,
                                                                    CronExecutionStatus::Failed(
                                                                        "Failed to get PID from PID file"
                                                                            .to_string(),
                                                                    ),
                                                                    None,
                                                                );
                                                            }
                                                        }
                                                        Err(e) => {
                                                            error!(
                                                                "Failed to reload PID file for cron job '{}': {}",
                                                                job_name_clone, e
                                                            );
                                                            cron_manager_clone.mark_job_completed(
                                                                &job_name_clone,
                                                                CronExecutionStatus::Failed(
                                                                    format!(
                                                                        "Failed to reload PID file: {}",
                                                                        e
                                                                    ),
                                                                ),
                                                                None,
                                                            );
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    error!(
                                                        "Failed to start cron job '{}': {}",
                                                        job_name_clone, e
                                                    );
                                                    cron_manager_clone
                                                        .mark_job_completed(
                                                            &job_name_clone,
                                                            CronExecutionStatus::Failed(
                                                                e.to_string(),
                                                            ),
                                                            None,
                                                        );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!(
                                                "Failed to create daemon for cron job '{}': {}",
                                                job_name_clone, e
                                            );
                                            cron_manager_clone.mark_job_completed(
                                                &job_name_clone,
                                                CronExecutionStatus::Failed(
                                                    e.to_string(),
                                                ),
                                                None,
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
                            Ok(response @ ControlResponse::Ok)
                            | Ok(response @ ControlResponse::Message(_)) => {
                                let _ = ipc::write_response(&mut stream, &response);
                                if should_shutdown {
                                    shutdown_requested = true;
                                }
                            }
                            Ok(ControlResponse::Error(msg)) => {
                                let _ = ipc::write_response(
                                    &mut stream,
                                    &ControlResponse::Error(msg.clone()),
                                );
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
            ControlCommand::Stop { service } => {
                if let Some(service) = service {
                    self.daemon.stop_service(&service)?;
                    Ok(ControlResponse::Message(format!(
                        "Service '{service}' stopped"
                    )))
                } else {
                    self.daemon.stop_services()?;
                    Ok(ControlResponse::Message("All services stopped".into()))
                }
            }
            ControlCommand::Restart { config, service } => {
                if let Some(service) = service {
                    if let Some(path) = config {
                        self.reload_config(Path::new(&path))?;
                    }
                    self.restart_single_service(&service)?;
                    Ok(ControlResponse::Message(format!(
                        "Service '{service}' restarted"
                    )))
                } else {
                    let target_path = config
                        .as_ref()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| self.config_path.clone());
                    self.reload_config(&target_path)?;
                    Ok(ControlResponse::Message("All services restarted".into()))
                }
            }
            ControlCommand::Shutdown => {
                self.daemon.stop_services()?;
                Ok(ControlResponse::Message("Supervisor shutting down".into()))
            }
        }
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
        self.daemon.stop_services()?;
        self.daemon.shutdown_monitor();
        self.daemon = Daemon::from_config(config, self.detach_children)?;
        self.config_path = resolved;
        self.daemon.start_services_nonblocking()?;
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

    fn shutdown_runtime(&mut self) -> Result<(), SupervisorError> {
        self.daemon.stop_services()?;
        self.daemon.shutdown_monitor();
        ipc::cleanup_runtime()?;
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    }

    /// Waits for a cron job process to complete and returns the final outcome.
    fn wait_for_cron_completion(
        pid: u32,
        job_name: &str,
    ) -> Result<CronCompletionOutcome, SupervisorError> {
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
        use nix::unistd::Pid;

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
