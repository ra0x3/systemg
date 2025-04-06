use crate::logs::spawn_log_writer;
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
use tracing::{debug, error, info, warn};

use crate::config::{Config, EnvConfig, ServiceConfig};
use crate::error::{PidFileError, ProcessManagerError};

/// Represents the PID file structure
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PidFile {
    /// Map of service names to their respective PIDs.
    services: HashMap<String, u32>,
}

impl PidFile {
    /// Returns the PID file path
    fn path() -> PathBuf {
        let home = std::env::var("HOME").expect("HOME not set");
        PathBuf::from(format!("{}/.local/share/systemg/pid.json", home))
    }

    /// Returns the services map.
    pub fn services(&self) -> &HashMap<String, u32> {
        &self.services
    }

    /// Loads the PID file from disk
    pub fn load() -> Result<Self, PidFileError> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(path)?;
        let pid_data = serde_json::from_str::<Self>(&contents)?;
        Ok(pid_data)
    }

    /// Return the PID for a specific service
    pub fn pid_for(&self, service: &str) -> Option<u32> {
        self.services.get(service).copied()
    }

    /// Reloads the PID file from disk
    pub fn reload() -> Result<Self, PidFileError> {
        let path = Self::path();
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
    pid_file: Arc<Mutex<PidFile>>,
}

impl Daemon {
    /// Initializes a new `Daemon` with an empty process map and a shared config reference.
    pub fn new(config: Config, pid_file: Arc<Mutex<PidFile>>) -> Self {
        debug!("Initializing daemon...");

        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
            pid_file,
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
        debug!("Launching service: '{service_name}' with command: `{command}`");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);

        debug!("Executing command: {cmd:?}");

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

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
                if libc::setpgid(0, 0) < 0 {
                    let err = std::io::Error::last_os_error();
                    eprintln!("systemg pre_exec: setpgid failed: {:?}", err);
                    return Err(err);
                }

                #[cfg(target_os = "linux")]
                {
                    use libc::{PR_SET_PDEATHSIG, SIGTERM, prctl};
                    if prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0) < 0 {
                        let err = std::io::Error::last_os_error();
                        eprintln!(
                            "systemg pre_exec: prctl PR_SET_PDEATHSIG failed: {:?}",
                            err
                        );
                        return Err(err);
                    }
                }

                Ok(())
            });
        }

        match cmd.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                debug!("Service '{service_name}' started with PID: {pid}");

                let stdout = child.stdout.take();
                let stderr = child.stderr.take();

                if let Some(out) = stdout {
                    spawn_log_writer(service_name, out, "stdout");
                }
                if let Some(err) = stderr {
                    spawn_log_writer(service_name, err, "stderr");
                }

                processes.lock()?.insert(service_name.to_string(), child);
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

        std::thread::sleep(std::time::Duration::from_millis(200));

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
    ///
    /// This implementation is intended for **Unix/macOS platforms only**.
    ///
    /// On these systems, services can be safely launched inside threads using `Command::spawn`,
    /// and the thread may exit immediately after spawning the child process. The child will continue
    /// running independently without issue.
    ///
    /// This function spawns the service in a dedicated thread, immediately joins that thread to ensure
    /// errors are surfaced synchronously. The child process is inserted into the shared process map,
    /// and its PID is recorded in the PID file.
    #[cfg(not(target_os = "linux"))]
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
        let pid_file = Arc::clone(&self.pid_file);

        let handle = thread::spawn(move || {
            debug!("Starting service thread for '{service_name}'");

            match Daemon::launch_attached_service(
                &service_name,
                &command,
                env,
                processes.clone(),
            ) {
                Ok(pid) => {
                    pid_file.lock()?.insert(&service_name, pid).unwrap();
                    Ok(pid)
                }
                Err(e) => {
                    error!("Failed to start service '{service_name}': {e}");
                    Err(e)
                }
            }
        });

        // Wait for the thread to complete and propagate any errors
        let _ = handle.join().map_err(|e| {
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

    /// Starts a single service and stores it in the process map.
    ///
    /// This implementation is intended for **Linux platforms only**.
    ///
    /// On Linux, services are launched in their own process groups and configured with
    /// `PR_SET_PDEATHSIG` to receive a `SIGTERM` if the parent thread exits. Because of this,
    /// the thread that spawns the service must remain alive for the lifetime of the child
    /// process to prevent premature termination.
    ///
    /// This function launches the service in a detached thread and inserts the child process
    /// into the shared process map and PID file. After spawning, the thread enters a blocking
    /// loop to ensure it stays alive, preserving the relationship required by `PR_SET_PDEATHSIG`.
    ///
    /// The service itself is monitored and managed separately, so the thread’s only responsibility
    /// is to maintain parent liveness for the child process.
    #[cfg(target_os = "linux")]
    pub fn start_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<(), ProcessManagerError> {
        use std::thread;

        info!("Starting service: {name}");

        let processes = Arc::clone(&self.processes);
        let command = service.command.clone();
        let env = service.env.clone();
        let service_name = name.to_string();
        let pid_file = Arc::clone(&self.pid_file);

        // Spawn the thread, but DO NOT join it.
        thread::spawn(move || {
            debug!("Starting service thread for '{service_name}'");

            match Daemon::launch_attached_service(
                &service_name,
                &command,
                env,
                processes.clone(),
            ) {
                Ok(pid) => {
                    pid_file.lock().unwrap().insert(&service_name, pid).unwrap();

                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(60));
                    }
                }
                Err(e) => {
                    error!("Failed to start service '{service_name}': {e}");
                }
            }
        });

        debug!("Service thread for '{name}' launched and detached (Linux)");

        Ok(())
    }

    /// Stops a specific service by name.
    ///
    /// If the service is running, it will be terminated and removed from the process map.
    ///
    /// On **Unix/macOS**, services are typically launched as individual processes.
    /// Sending a `SIGTERM` to the recorded PID is sufficient to stop the service cleanly.
    ///
    /// On **Linux**, services are launched in their own process groups (via `setpgid`)
    /// to support `PR_SET_PDEATHSIG` for parent-child death linkage. This means that
    /// stopping a service requires sending a signal to the *entire process group*,
    /// not just the root PID. This is done by sending a signal to the **negative** PID
    /// (`-pid`) using Unix conventions.
    ///
    /// This function correctly handles both platforms, ensuring the full service tree
    /// is terminated regardless of how it was spawned.
    pub fn stop_service(
        &mut self,
        service_name: &str,
    ) -> Result<(), ProcessManagerError> {
        let pid = self.pid_file.lock()?.get(service_name);

        if let Some(process_id) = pid {
            let pid = nix::unistd::Pid::from_raw(process_id as i32);

            if nix::sys::signal::kill(pid, None).is_err() {
                warn!("Service '{service_name}' is already stopped.");
            } else {
                debug!("Stopping service '{service_name}' (PID {pid})");

                let pgid = nix::unistd::Pid::from_raw(-pid.as_raw()); // Negative = process group
                nix::sys::signal::kill(pgid, nix::sys::signal::Signal::SIGTERM)?;

                self.processes.lock()?.remove(service_name);
                self.pid_file.lock()?.remove(service_name)?;
            }

            debug!("Service '{service_name}' stopped successfully.");
        } else {
            warn!("Service '{service_name}' not found in PID file.");
        }

        Ok(())
    }

    /// Stops all running services.
    ///
    /// Iterates over all active processes and terminates them.
    pub fn stop_services(&mut self) -> Result<(), ProcessManagerError> {
        let services: Vec<String> =
            self.pid_file.lock()?.services.keys().cloned().collect();

        for service in services {
            if let Err(e) = self.stop_service(&service) {
                error!("Failed to stop service '{service}': {e}");
            }
        }

        Ok(())
    }

    /// Monitors all running services and restarts them if they exit unexpectedly.
    fn monitor_services(&self) {
        debug!("Starting service monitoring thread...");
        let processes = Arc::clone(&self.processes);
        let config = Arc::clone(&self.config);

        let handle = thread::spawn(move || {
            loop {
                let mut exited_services = Vec::new();
                let mut restarted_services = Vec::new();
                let mut active_services = 0;
                let mut pid_file = PidFile::load().unwrap();

                {
                    let mut locked_processes = processes.lock().unwrap();
                    for (name, child) in locked_processes.iter_mut() {
                        match child.try_wait() {
                            Ok(Some(status)) => {
                                let exited_normally = status.success();
                                if exited_normally {
                                    info!("Service '{name}' exited normally.");
                                } else {
                                    warn!(
                                        "Service '{name}' was terminated with {status:?}."
                                    );
                                }
                                exited_services.push((name.clone(), exited_normally));
                            }
                            Ok(None) => {
                                debug!("Service '{name}' is still running.");
                                active_services += 1;
                            }
                            Err(e) => error!("Failed to check status of '{name}': {e}"),
                        }
                    }
                }

                debug!("PID file: {pid_file:?}");

                for (name, exited_normally) in exited_services {
                    if pid_file.get(&name).is_none() {
                        info!("Service '{name}' was manually stopped. Skipping restart.");
                    } else if !exited_normally {
                        warn!("Service '{name}' crashed. Restarting...");
                        restarted_services.push(name.clone());
                    } else {
                        debug!(
                            "Service '{name}' exited cleanly. Removing from PID file."
                        );
                        pid_file.remove(&name).unwrap();
                    }

                    processes.lock().unwrap().remove(&name);
                }

                if active_services == 0 {
                    info!("All services have exited. systemg is shutting down.");
                    std::process::exit(0);
                }

                for name in restarted_services {
                    if let Some(service) = config.services.get(&name) {
                        Self::handle_restart(&name, service, Arc::clone(&processes));
                    }
                }

                thread::sleep(Duration::from_secs(2));
            }
        });

        handle.join().unwrap();
    }

    /// Handles restarting a service if its restart policy allows.
    fn handle_restart(
        name: &str,
        service: &ServiceConfig,
        processes: Arc<Mutex<HashMap<String, Child>>>,
    ) {
        let name = name.to_string();
        let command = service.command.clone();
        let env = service.env.clone();

        let backoff = service
            .backoff
            .as_deref()
            .unwrap_or("5s")
            .trim_end_matches('s')
            .parse::<u64>()
            .unwrap_or(5);

        let _ = thread::spawn(move || {
            warn!("Restarting '{name}' after {backoff} seconds...");
            thread::sleep(Duration::from_secs(backoff));

            if let Err(e) =
                Daemon::launch_attached_service(&name, &command, env, processes)
            {
                error!("Failed to restart '{name}': {e}");
            }
        })
        .join();
    }
}
