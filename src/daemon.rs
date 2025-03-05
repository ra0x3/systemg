use std::{
    collections::HashMap,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use libc::{getppid, setpgid};
use tracing::{debug, error, info, warn};

use crate::config::{Config, EnvConfig, ServiceConfig};
use crate::error::ProcessManagerError;

/// Manages services, ensuring they start, stop, and restart as needed.
pub struct Daemon {
    /// Shared map of running service processes.
    processes: Arc<Mutex<HashMap<String, Child>>>,
    /// Reference to the service configuration.
    config: Arc<Config>,
}

impl Daemon {
    /// Initializes a new `Daemon` with an empty process map and a shared config reference.
    pub fn new(config: Config) -> Self {
        info!("Initializing daemon...");
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
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
    /// * `command` - The command string to execute.
    /// * `env` - Optional environment variables.
    /// * `processes` - Shared process tracking map.
    ///
    /// # Returns
    /// A [`Child`] process handle if successful.
    fn launch_attached_service(
        service_name: &str,
        command: &str,
        env: Option<EnvConfig>,
        processes: Arc<Mutex<HashMap<String, Child>>>,
    ) -> Result<(), ProcessManagerError> {
        debug!("Launching service: {service_name}");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);

        debug!("Executing command: {cmd:?}");

        // Inherit stdout and stderr for logging
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

        // Set environment variables if provided
        if let Some(env) = env {
            if let Some(vars) = &env.vars {
                debug!("Setting environment variables: {vars:?}");
                for (key, value) in vars {
                    cmd.env(key, value);
                }
            }
        }

        unsafe {
            cmd.pre_exec(|| {
                let ppid = getppid();
                if setpgid(0, ppid) < 0 {
                    error!("Failed to set process group (setpgid)");
                    return Err(std::io::Error::last_os_error());
                }

                #[cfg(target_os = "linux")]
                {
                    use libc::{PR_SET_PDEATHSIG, SIGTERM, prctl};
                    if prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0) < 0 {
                        error!("Failed to set PR_SET_PDEATHSIG");
                        return Err(std::io::Error::last_os_error());
                    }
                }

                Ok(())
            });
        }

        match cmd.spawn() {
            Ok(child) => {
                debug!("Service '{service_name}' started with PID: {}", child.id());
                processes
                    .lock()
                    .expect("Poisoned lock")
                    .insert(service_name.to_string(), child);
                info!("Service '{service_name}' started successfully.");
                Ok(())
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

    /// Starts all services and begins monitoring them in the background.
    pub fn start_services(&self) -> Result<(), ProcessManagerError> {
        info!("Starting all services...");
        let config = Arc::clone(&self.config);

        for (name, service) in &config.services {
            self.start_service(name, service)?;
        }
        info!("All services started successfully.");

        // Start monitoring processes in a background thread
        self.monitor_services();

        Ok(())
    }

    /// Restarts all services by stopping and then starting them again.
    ///
    /// Maintains dependency order and applies restart policies where necessary.
    ///
    /// # Errors
    /// Returns `ProcessManagerError` if a service fails to restart.
    pub fn restart_services(&mut self) -> Result<(), ProcessManagerError> {
        info!("Restarting all services...");
        self.stop_services()?;

        let config = Arc::clone(&self.config);
        for (name, service) in &config.services {
            debug!("Restarting service: {}", name);
            self.start_service(name, service)?;
        }

        info!("All services restarted successfully.");
        Ok(())
    }

    /// Starts a single service and stores it in the process map.
    pub fn start_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<(), ProcessManagerError> {
        info!("Starting service: {name}");

        let processes = Arc::clone(&self.processes);
        let command = service.command.clone();
        let env = service.env.clone();
        let service_name = name.to_string();

        let handle = thread::spawn(move || {
            debug!("Starting service thread for '{service_name}'");

            if let Err(e) = Daemon::launch_attached_service(
                &service_name,
                &command,
                env,
                processes.clone(),
            ) {
                error!("Failed to start service '{service_name}': {e}");
            }
        });

        // Wait for the thread to complete and propagate any errors
        handle.join().map_err(|e| {
            error!("Failed to join service thread for '{name}': {e:?}");
            ProcessManagerError::ServiceStartError {
                service: name.to_string(),
                source: std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    format!("{e:?}"),
                ),
            }
        })?;

        debug!("Service thread for '{name}' completed");

        Ok(())
    }

    /// Stops a specific service by name.
    ///
    /// If the service is running, it will be terminated and removed from the process map.
    pub fn stop_service(
        &mut self,
        service_name: &str,
    ) -> Result<(), ProcessManagerError> {
        if let Some(mut child) = self.processes.lock()?.remove(service_name) {
            info!("Stopping service: {service_name}");
            child
                .kill()
                .map_err(|e| ProcessManagerError::ServiceStopError {
                    service: service_name.to_string(),
                    source: e,
                })?;
        }
        Ok(())
    }

    /// Stops all running services.
    ///
    /// Iterates over all active processes and terminates them.
    pub fn stop_services(&mut self) -> Result<(), ProcessManagerError> {
        if self.processes.lock()?.is_empty() {
            debug!("No services running to stop.");
            return Ok(());
        }
        info!("Stopping all running services...");
        let service_names: Vec<String> = self.processes.lock()?.keys().cloned().collect();
        for service in service_names {
            self.stop_service(&service)?;
        }
        info!("All services have been stopped.");
        Ok(())
    }

    /// Monitors all running services and restarts them if they exit unexpectedly.
    fn monitor_services(&self) {
        info!("Starting service monitoring thread...");
        let processes = Arc::clone(&self.processes);
        let config = Arc::clone(&self.config);

        let handle = thread::spawn(move || {
            loop {
                let mut restarted_services = Vec::new();
                {
                    let mut locked_processes = processes.lock().unwrap();

                    debug!("Checking service statuses: {locked_processes:?}");

                    for (name, child) in locked_processes.iter_mut() {
                        match child.try_wait() {
                            Ok(Some(status)) if !status.success() => {
                                error!("Service '{name}' exited with error: {status:?}");
                                restarted_services.push(name.clone());
                            }
                            Ok(Some(_)) => {
                                debug!("Service '{name}' exited normally.");
                            }
                            Ok(None) => {
                                // Process still running, do nothing
                                debug!("Service '{name}' is still running.");
                            }
                            Err(e) => {
                                error!("Failed to check status of '{name}': {e}");
                            }
                        }
                    }

                    // Remove failed services from process list
                    for name in &restarted_services {
                        locked_processes.remove(name);
                    }
                }

                // Restart services after releasing the lock
                for name in restarted_services {
                    if let Some(service) = config.services.get(&name) {
                        error!("Restarting service '{}'", name);
                        // let mut locked_processes = processes.lock().unwrap();
                        Self::handle_restart(&name, service, processes.clone());
                    }
                }

                thread::sleep(Duration::from_secs(5));
            }
        });

        let _ = handle
            .join()
            .map_err(|e| error!("Failed to join service thread: {e:?}"));
    }

    /// Handles restarting a service if its restart policy allows.
    fn handle_restart(
        name: &str,
        service: &ServiceConfig,
        processes: Arc<Mutex<HashMap<String, Child>>>,
    ) {
        let service_name = name.to_string();
        let restart_policy = service
            .restart_policy
            .clone()
            .unwrap_or_else(|| "never".to_string());
        let backoff = service
            .backoff
            .as_deref()
            .unwrap_or("5s")
            .trim_end_matches('s')
            .parse::<u64>()
            .unwrap_or(5);
        let command = service.command.clone();
        let env = service.env.clone();

        let handle = thread::spawn(move || {
            debug!("Service '{service_name}' restart policy: {restart_policy}");

            if restart_policy == "always" || restart_policy == "on_failure" {
                warn!("Restarting '{service_name}' after {backoff} seconds...");
                thread::sleep(Duration::from_secs(backoff));

                if let Err(e) = Daemon::launch_attached_service(
                    &service_name,
                    &command,
                    env,
                    processes.clone(),
                ) {
                    error!("Failed to restart '{service_name}': {e}");
                }
            }
        });

        let _ = handle
            .join()
            .map_err(|e| error!("Failed to join service thread: {e:?}"));
    }
}
