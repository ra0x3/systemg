use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, error, info};

use crate::config::{Config, ServiceConfig};
use crate::error::ProcessManagerError;

/// Manages services, ensuring they start, stop, and restart as needed.
pub struct Daemon {
    /// Shared map of running service processes.
    processes: Arc<Mutex<HashMap<String, Child>>>,
    /// Reference to the service configuration.
    config: Arc<Config>,
    /// Handles for service monitoring threads.
    service_threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Daemon {
    /// Initializes a new `Daemon` with an empty process map and a shared config reference.
    pub fn new(config: Config) -> Self {
        info!("Initializing daemon...");
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
            service_threads: Mutex::new(Vec::new()),
        }
    }

    /// Starts all services and begins monitoring them in the background.
    pub fn start_services(&self) -> Result<(), ProcessManagerError> {
        info!("Starting all services...");
        let config = Arc::clone(&self.config);

        for (name, service) in &config.services {
            debug!("Starting service: {}", name);
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

        info!("Waiting for all services to stop...");
        for handle in self.service_threads.lock()?.drain(..) {
            handle.join().expect("Failed to join service thread");
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
        info!("Starting service: {}", name);

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&service.command);
        debug!("Executing command: {}", service.command);

        if let Some(env) = &service.env {
            if let Some(vars) = &env.vars {
                debug!("Setting environment variables: {:?}", vars);
                for (key, value) in vars {
                    cmd.env(key, value);
                }
            }
        }

        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

        let child = cmd.spawn().map_err(|e| {
            error!("Failed to start service '{}': {}", name, e);
            ProcessManagerError::ServiceStartError {
                service: name.to_string(),
                source: e,
            }
        })?;

        self.processes.lock()?.insert(name.to_string(), child);
        info!("Service '{}' started successfully.", name);

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
            info!("Stopping service: {}", service_name);
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
            info!("No services running to stop.");
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
        let processes = Arc::clone(&self.processes);
        let config = Arc::clone(&self.config);

        let handle = thread::spawn(move || {
            loop {
                let mut service_failures = Vec::new();

                {
                    let mut locked_processes =
                        processes.lock().expect("Failed to lock processes");

                    for (name, child) in locked_processes.iter_mut() {
                        match child.try_wait() {
                            Ok(Some(status)) if !status.success() => {
                                error!(
                                    "Service '{}' exited with error: {:?}",
                                    name, status
                                );
                                if let Some(service) = config.services.get(name) {
                                    service_failures
                                        .push((name.clone(), service.clone()));
                                }
                            }
                            Ok(Some(_)) => {
                                info!("Service '{}' exited normally.", name);
                            }
                            Ok(None) => {
                                // Process still running, do nothing
                                debug!("Service '{}' is still running.", name);
                            }
                            Err(e) => {
                                error!("Failed to check status of '{}': {}", name, e);
                            }
                        }
                    }
                }

                // Handle restarts outside the locked scope
                for (name, service) in service_failures {
                    let mut locked_processes =
                        processes.lock().expect("Failed to lock processes");
                    Self::handle_restart(&name, &service, &mut locked_processes);
                }

                thread::sleep(Duration::from_secs(5));
            }
        });

        self.service_threads.lock().unwrap().push(handle);
    }

    /// Handles restarting a service if its restart policy allows.
    fn handle_restart(
        name: &str,
        service: &ServiceConfig,
        processes: &mut HashMap<String, Child>,
    ) {
        let restart_policy = service.restart_policy.as_deref().unwrap_or("never");

        if restart_policy == "always" || restart_policy == "on-failure" {
            let backoff_time = service
                .backoff
                .as_deref()
                .unwrap_or("5s")
                .trim_end_matches('s')
                .parse::<u64>()
                .unwrap_or(5);

            error!("Restarting '{}' after {} seconds...", name, backoff_time);
            thread::sleep(Duration::from_secs(backoff_time));

            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(&service.command);
            cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

            match cmd.spawn() {
                Ok(child) => {
                    processes.insert(name.to_string(), child);
                    info!("Service '{}' restarted successfully.", name);
                }
                Err(e) => {
                    error!("Failed to restart service '{}': {}", name, e);
                }
            }
        }
    }
}
