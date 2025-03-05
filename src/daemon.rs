use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use libc::{getppid, setpgid};
use tracing::{debug, error, info, warn};

use crate::config::{Config, EnvConfig, ServiceConfig};
use crate::error::{PidFileError, ProcessManagerError};

/// Represents the PID file structure
#[derive(Debug, Serialize, Deserialize, Default)]
struct PidFile {
    /// Map of service names to their respective PIDs.
    services: HashMap<String, u32>,
}

impl PidFile {
    /// Returns the PID file path
    fn path() -> PathBuf {
        let home = std::env::var("HOME").expect("HOME not set");
        PathBuf::from(format!("{}/.local/share/systemg/pid.json", home))
    }

    /// Loads the PID file from disk
    pub fn load() -> Result<Self, PidFileError> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(&path)?;
        let pid_data = serde_json::from_str::<Self>(&contents)?;
        Ok(pid_data)
    }

    /// Saves the current state to the PID file
    pub fn save(&self) -> Result<(), PidFileError> {
        let path = Self::path();
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Inserts a new service PID and saves
    pub fn insert(&mut self, service: &str, pid: u32) -> Result<(), PidFileError> {
        self.services.insert(service.to_string(), pid);
        self.save()
    }

    /// Removes a service and saves
    pub fn remove(&mut self, service: &str) -> Result<(), PidFileError> {
        if self.services.remove(service).is_some() {
            self.save()
        } else {
            Err(PidFileError::ServiceNotFound)
        }
    }

    /// Retrieves a service PID
    pub fn get(&self, service: &str) -> Option<u32> {
        self.services.get(service).copied()
    }
}

/// Manages services, ensuring they start, stop, and restart as needed.
pub struct Daemon {
    /// Shared map of running service processes.
    processes: Arc<Mutex<HashMap<String, Child>>>,
    /// Reference to the service configuration.
    config: Arc<Config>,
    /// The PID file for tracking service PIDs.
    pid: Arc<Mutex<PidFile>>,
}

impl Daemon {
    /// Initializes a new `Daemon` with an empty process map and a shared config reference.
    pub fn new(config: Config) -> Self {
        info!("Initializing daemon...");

        let pid = PidFile::load().unwrap_or_default();
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
            pid: Arc::new(Mutex::new(pid)),
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
    ) -> Result<u32, ProcessManagerError> {
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
                let pid = child.id();
                debug!("Service '{service_name}' started with PID: {pid}");
                processes
                    .lock()
                    .expect("Poisoned lock")
                    .insert(service_name.to_string(), child);
                info!("Service '{service_name}' started successfully.");
                Ok(pid)
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

            match Daemon::launch_attached_service(
                &service_name,
                &command,
                env,
                processes.clone(),
            ) {
                Ok(pid) => {
                    let mut pidf = PidFile::load().unwrap_or_default();
                    pidf.insert(&service_name, pid).unwrap();
                }
                Err(e) => error!("Failed to start service '{service_name}': {e}"),
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
        let mut pidf = self.pid.lock()?;

        if let Some(pid) = pidf.get(service_name) {
            let pid = nix::unistd::Pid::from_raw(pid as i32);

            if nix::sys::signal::kill(pid, None).is_err() {
                warn!("Service '{service_name}' is already stopped.");
            } else {
                debug!("Stopping service '{service_name}' (PID {pid})");
                nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).map_err(
                    |e| ProcessManagerError::ServiceStopError {
                        service: service_name.to_string(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        ),
                    },
                )?;
            }

            self.processes.lock()?.remove(service_name);
            pidf.remove(service_name)?;
        } else {
            warn!("Service '{service_name}' not found in PID file.");
        }

        Ok(())
    }

    /// Stops all running services.
    ///
    /// Iterates over all active processes and terminates them.
    pub fn stop_services(&mut self) -> Result<(), ProcessManagerError> {
        let services: Vec<String> = {
            let pidf = self.pid.lock()?;
            pidf.services.keys().cloned().collect()
        };

        for service in services {
            if let Err(e) = self.stop_service(&service) {
                error!("Failed to stop service '{service}': {e}");
            }
        }

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

                match Daemon::launch_attached_service(
                    &service_name,
                    &command,
                    env,
                    processes.clone(),
                ) {
                    Ok(pid) => {
                        let mut pidf = PidFile::load().unwrap_or_default();
                        pidf.insert(&service_name, pid).unwrap();
                    }
                    Err(e) => error!("Failed to start service '{service_name}': {e}"),
                }
            }
        });

        let _ = handle
            .join()
            .map_err(|e| error!("Failed to join service thread: {e:?}"));
    }
}
