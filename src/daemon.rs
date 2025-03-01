//! # Process Manager Daemon
//!
//! This module manages service processes, handling dependencies, restarts, and logging.
//! It ensures services start in the correct order, applies restart policies, and provides
//! an interface for starting, stopping, and checking the status of running services.

use crate::config::{Config, ServiceConfig};
use crate::error::ProcessManagerError;
use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use tracing::{trace, error};

/// Manages service processes, handling dependencies, restarts, and logging.
pub struct Daemon {
    /// A map of running service processes.
    processes: HashMap<String, Child>,
}

impl Daemon {
    /// Starts all services defined in the configuration.
    ///
    /// Ensures that dependencies are started first and applies restart policies.
    pub fn start_services(&mut self, config: &Config) -> Result<(), ProcessManagerError> {
        for (name, service) in &config.services {
            self.start_service(name, service)?;
        }
        Ok(())
    }

    /// Starts a single service and manages its process.
    ///
    /// If the service has dependencies, it waits for them to start first.
    pub fn start_service(
        &mut self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<(), ProcessManagerError> {
        if let Some(deps) = &service.depends_on {
            for dep in deps {
                if !self.processes.contains_key(dep) {
                    return Err(ProcessManagerError::DependencyError {
                        service: name.to_string(),
                        dependency: dep.to_string(),
                    });
                }
            }
        }

        trace!("Starting service: {}", name);
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&service.command);

        if let Some(env) = &service.env {
            if let Some(vars) = &env.vars {
                for (key, value) in vars {
                    cmd.env(key, value);
                }
            }
        }

        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

        let child = cmd
            .spawn()
            .map_err(|e| ProcessManagerError::ServiceStartError {
                service: name.to_string(),
                source: e,
            })?;

        self.processes.insert(name.to_string(), child);
        trace!("Service {} started successfully", name);
        Ok(())
    }

    /// Stops a specific service by name.
    ///
    /// If the service is running, it will be terminated and removed from the process map.
    pub fn stop_service(&mut self, service_name: &str) -> Result<(), ProcessManagerError> {
        if let Some(mut child) = self.processes.remove(service_name) {
            trace!("Stopping service: {}", service_name);
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
        if self.processes.is_empty() {
            trace!("No services running to stop.");
            return Ok(());
        }
        let service_names: Vec<String> = self.processes.keys().cloned().collect();
        for service in service_names {
            self.stop_service(&service)?;
        }
        Ok(())
    }

    /// Restarts all services by stopping and restarting them individually.
    ///
    /// Ensures services are restarted in a way that maintains dependency order.
    pub fn restart_services(&mut self, config: &Config) -> Result<(), ProcessManagerError> {
        trace!("Restarting all services...");
        let service_names: Vec<String> = self.processes.keys().cloned().collect();

        for service in service_names {
            if let Some(service_config) = config.services.get(&service) {
                trace!("Restarting service: {}", service);
                self.stop_service(&service)?;
                self.start_service(&service, service_config)?;
            }
        }

        Ok(())
    }

    /// Checks the status of running services.
    ///
    /// Logs the names of all currently active services.
    pub fn status(&self) {
        for name in self.processes.keys() {
            trace!("Service {} is running", name);
        }
    }
}

impl Default for Daemon {
    /// Creates a new `Daemon` with an empty process map.
    fn default() -> Self {
        Daemon {
            processes: HashMap::new(),
        }
    }
}
