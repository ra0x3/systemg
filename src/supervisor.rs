use std::{
    fs, io,
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    time::Duration,
};

use tracing::{debug, error, info, warn};

use crate::{
    config::load_config,
    daemon::Daemon,
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
}

impl Supervisor {
    /// Creates a new supervisor from a config path.
    pub fn new(
        config_path: PathBuf,
        detach_children: bool,
    ) -> Result<Self, SupervisorError> {
        let config = load_config(Some(config_path.to_string_lossy().as_ref()))?;
        let daemon = Daemon::from_config(config, detach_children)?;
        Ok(Self {
            config_path,
            daemon,
            detach_children,
        })
    }

    /// Runs the supervisor event loop.
    pub fn run(&mut self) -> Result<(), SupervisorError> {
        ipc::cleanup_runtime()?;
        let socket_path = ipc::socket_path()?;
        if socket_path.exists() {
            fs::remove_file(&socket_path)?;
        }

        let listener = UnixListener::bind(&socket_path)?;
        ipc::write_supervisor_pid(unsafe { libc::getpid() })?;
        self.daemon.start_services_nonblocking()?;

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

        self.daemon.stop_service(name)?;
        self.daemon.start_service(name, service_config)?;
        Ok(())
    }

    fn shutdown_runtime(&mut self) -> Result<(), SupervisorError> {
        self.daemon.stop_services()?;
        self.daemon.shutdown_monitor();
        ipc::cleanup_runtime()?;
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    }
}
