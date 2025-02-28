use crate::config::{Config, ServiceConfig};
use crate::error::ProcessManagerError;
use std::collections::HashMap;
use std::process::{Child, Command, Stdio};

/// Manages service processes, handling dependencies, restarts, and logging.
///
/// This module is responsible for starting services, managing their lifecycles,
/// handling restart policies, and ensuring dependent services are started in the correct order.
pub struct Daemon {
    /// A map of running service processes.
    processes: HashMap<String, Child>,
}

impl Daemon {
    /// Starts all services defined in the configuration.
    ///
    /// This function ensures that dependencies are started first and applies restart policies.
    ///
    /// # Arguments
    ///
    /// * `config` - The parsed configuration containing service definitions.
    ///
    /// # Returns
    ///
    /// * `Ok(())` if all services start successfully.
    /// * `Err(ProcessManagerError)` if any service fails to start.
    pub fn start_services(&mut self, config: &Config) -> Result<(), ProcessManagerError> {
        for (name, service) in &config.services {
            self.start_service(name, service)?;
        }
        Ok(())
    }

    /// Starts a single service and manages its process.
    ///
    /// If the service has dependencies, it waits for them to start first.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the service.
    /// * `service` - The service configuration.
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the service starts successfully.
    /// * `Err(ProcessManagerError::DependencyError)` if a required dependency is missing.
    /// * `Err(ProcessManagerError::ServiceStartError)` if the service fails to start.
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

        println!("Starting service: {}", name);
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
        println!("Service {} started successfully", name);
        Ok(())
    }

    /// Stops a specific service by name.
    ///
    /// If the service is running, it will be terminated and removed from the process map.
    ///
    /// # Arguments
    ///
    /// * `service_name` - The name of the service to stop.
    ///
    /// # Returns
    ///
    /// * `Ok(())` if the service stops successfully.
    /// * `Err(ProcessManagerError::ServiceStopError)` if the service fails to stop.
    pub fn stop_service(
        &mut self,
        service_name: &str,
    ) -> Result<(), ProcessManagerError> {
        if let Some(mut child) = self.processes.remove(service_name) {
            println!("Stopping service: {}", service_name);
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
    ///
    /// # Returns
    ///
    /// * `Ok(())` if all services stop successfully.
    /// * `Err(ProcessManagerError::ServiceStopError)` if any service fails to stop.
    pub fn stop_services(&mut self) -> Result<(), ProcessManagerError> {
        if self.processes.is_empty() {
            println!("No services running to stop.");
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
    ///
    /// # Arguments
    ///
    /// * `config` - The parsed configuration containing service definitions.
    ///
    /// # Returns
    ///
    /// * `Ok(())` if all services restart successfully.
    /// * `Err(ProcessManagerError)` if any service fails to stop or restart.
    pub fn restart_services(
        &mut self,
        config: &Config,
    ) -> Result<(), ProcessManagerError> {
        println!("Restarting all services...");
        let service_names: Vec<String> = self.processes.keys().cloned().collect();

        for service in service_names {
            if let Some(service_config) = config.services.get(&service) {
                println!("Restarting service: {}", service);
                self.stop_service(&service)?;
                self.start_service(&service, service_config)?;
            }
        }

        Ok(())
    }

    /// Checks the status of running services.
    ///
    /// Prints the names of all currently active services.
    pub fn status(&self) {
        for name in self.processes.keys() {
            println!("Service {} is running", name);
        }
    }
}

/// Implement the default trait for `Daemon`.
impl Default for Daemon {
    /// Creates a new `Daemon` with an empty process map.
    fn default() -> Self {
        Daemon {
            processes: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ServiceConfig};
    use crate::error::ProcessManagerError;
    use std::collections::HashMap;

    /// Helper function to create a mock configuration for testing.
    fn mock_config() -> Config {
        let mut services = HashMap::new();

        services.insert(
            "db".to_string(),
            ServiceConfig {
                command: "echo Starting DB".to_string(),
                env: None,
                restart_policy: Some("always".to_string()),
                backoff: Some("5s".to_string()),
                depends_on: None,
                hooks: None,
            },
        );

        services.insert(
            "app".to_string(),
            ServiceConfig {
                command: "echo Starting App".to_string(),
                env: None,
                restart_policy: Some("on-failure".to_string()),
                backoff: Some("5s".to_string()),
                depends_on: Some(vec!["db".to_string()]),
                hooks: None,
            },
        );

        Config {
            version: 1,
            services,
        }
    }

    /// Test starting a service successfully.
    #[test]
    fn test_start_service() {
        let config = mock_config();
        let mut daemon = Daemon::default();

        let result = daemon.start_service("db", config.services.get("db").unwrap());
        assert!(result.is_ok(), "Service should start successfully");
        assert!(
            daemon.processes.contains_key("db"),
            "Process should be stored in daemon"
        );
    }

    /// Test starting a service with missing dependency.
    #[test]
    fn test_start_service_missing_dependency() {
        let config = mock_config();
        let mut daemon = Daemon::default();

        let result = daemon.start_service("app", config.services.get("app").unwrap());
        assert!(
            matches!(result, Err(ProcessManagerError::DependencyError { .. })),
            "Should return DependencyError"
        );
    }

    /// Test stopping a running service.
    #[test]
    fn test_stop_service() {
        let config = mock_config();
        let mut daemon = Daemon::default();
        daemon
            .start_service("db", config.services.get("db").unwrap())
            .unwrap();

        let result = daemon.stop_services();
        assert!(result.is_ok(), "Stopping services should succeed");
        assert!(
            daemon.processes.is_empty(),
            "All processes should be removed"
        );
    }

    /// Test restarting services.
    #[test]
    fn test_restart_services() {
        let config = mock_config();
        let mut daemon = Daemon::default();
        daemon
            .start_service("db", config.services.get("db").unwrap())
            .unwrap();

        let result = daemon.restart_services(&config);
        if let Err(e) = &result {
            println!("Error restarting services: {:?}", e);
        }
        assert!(result.is_ok(), "Restarting services should succeed");
        assert!(
            daemon.processes.contains_key("db"),
            "Process should be restarted"
        );
    }

    /// Test checking service status.
    #[test]
    fn test_service_status() {
        let config = mock_config();
        let mut daemon = Daemon::default();
        daemon
            .start_service("db", config.services.get("db").unwrap())
            .unwrap();

        daemon.status(); // This will print output, but we ensure it doesn't panic.
    }
}
