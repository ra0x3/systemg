//! Module for managing and monitoring system services.
use crate::logs::spawn_log_writer;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tracing::{debug, error, info, warn};

use crate::config::{Config, EnvConfig, HookType, Hooks, ServiceConfig};
use crate::error::{PidFileError, ProcessManagerError};

/// Represents the PID file structure
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
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

/// Run a hook command with the provided environment variables.
fn run_hook(
    hook_cmd: &str,
    env: &Option<EnvConfig>,
    hook_type: HookType,
    service_name: &str,
) {
    debug!(
        "Running {} hook for '{}': `{}`",
        hook_type.as_ref(),
        service_name,
        hook_cmd
    );

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(hook_cmd);

    if let Some(env_config) = env
        && let Some(vars) = &env_config.vars
    {
        for (k, v) in vars {
            cmd.env(k, v);
        }
    }

    match cmd.spawn() {
        Ok(mut child) => {
            let _ = child.wait();
            debug!(
                "{} hook for '{}' completed.",
                hook_type.as_ref(),
                service_name
            );
        }
        Err(e) => {
            error!(
                "Failed to run {} hook for '{}': {}",
                hook_type.as_ref(),
                service_name,
                e
            );
        }
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
    /// Whether child services should be detached from systemg (legacy behavior).
    detach_children: bool,
    /// Base directory for resolving relative service commands and assets.
    project_root: PathBuf,
    /// Flag indicating whether the monitoring loop should remain active.
    running: Arc<AtomicBool>,
    /// Handle to the background monitoring thread once spawned.
    monitor_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

impl Daemon {
    /// Initializes a new `Daemon` with an empty process map and a shared config reference.
    pub fn new(
        config: Config,
        pid_file: Arc<Mutex<PidFile>>,
        detach_children: bool,
    ) -> Self {
        debug!("Initializing daemon...");

        let project_root = config
            .project_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));

        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
            pid_file,
            detach_children,
            running: Arc::new(AtomicBool::new(false)),
            monitor_handle: Arc::new(Mutex::new(None)),
            project_root,
        }
    }

    /// Convenience constructor that loads the PID file automatically.
    pub fn from_config(
        config: Config,
        detach_children: bool,
    ) -> Result<Self, ProcessManagerError> {
        let pid_file = Arc::new(Mutex::new(PidFile::load()?));
        Ok(Self::new(config, pid_file, detach_children))
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
        working_dir: PathBuf,
        processes: Arc<Mutex<HashMap<String, Child>>>,
        hooks: Option<Hooks>,
        detach_children: bool,
    ) -> Result<u32, ProcessManagerError> {
        debug!("Launching service: '{service_name}' with command: `{command}`");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd.current_dir(&working_dir);

        debug!("Executing command: {cmd:?}");

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        if let Some(env) = env.clone()
            && let Some(vars) = &env.vars
        {
            debug!("Setting environment variables: {vars:?}");
            for (key, value) in vars {
                cmd.env(key, value);
            }
        }

        unsafe {
            cmd.pre_exec(move || {
                if detach_children {
                    // Legacy mode: allow services to continue if systemg exits abruptly.
                    if libc::setsid() < 0 {
                        let err = std::io::Error::last_os_error();
                        eprintln!("systemg pre_exec: setsid failed: {:?}", err);
                        return Err(err);
                    }
                } else {
                    // Place each service in its own process group so we can signal the entire
                    // tree without touching the supervisor's group.
                    if libc::setpgid(0, 0) < 0 {
                        let err = std::io::Error::last_os_error();
                        eprintln!("systemg pre_exec: setpgid(0, 0) failed: {:?}", err);
                        return Err(err);
                    }
                }

                // Ensure service gets killed on parent death (Linux only)
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

                if let Some(hooks) = hooks
                    && let Some(hook_cmd) = &hooks.on_start
                {
                    run_hook(hook_cmd, &env, HookType::OnStart, service_name);
                }

                processes.lock()?.insert(service_name.to_string(), child);
                Ok(pid)
            }
            Err(e) => {
                error!("Failed to start service '{service_name}': {e}");
                if let Some(hooks) = hooks
                    && let Some(hook_cmd) = &hooks.on_start
                {
                    run_hook(hook_cmd, &env, HookType::OnStart, service_name);
                }
                Err(ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source: e,
                })
            }
        }
    }

    /// Starts all services and blocks until they exit.
    pub fn start_services_blocking(&self) -> Result<(), ProcessManagerError> {
        self.start_all_services()?;
        self.spawn_monitor_thread()?;
        self.wait_for_monitor();
        Ok(())
    }

    /// Starts all services and returns immediately, keeping the monitor loop alive in the
    /// background. Intended for the long-lived supervisor process.
    pub fn start_services_nonblocking(&self) -> Result<(), ProcessManagerError> {
        self.start_all_services()?;
        self.spawn_monitor_thread()
    }

    /// Executes the start workflow without waiting on the monitor thread.
    fn start_all_services(&self) -> Result<(), ProcessManagerError> {
        info!("Starting all services...");
        for (name, service) in &self.config.services {
            self.start_service(name, service)?;
        }
        info!("All services started successfully.");

        thread::sleep(Duration::from_millis(200));
        Ok(())
    }

    /// Restarts all services by stopping and then starting them again, reusing the existing
    /// monitor thread if available.
    pub fn restart_services(&self) -> Result<(), ProcessManagerError> {
        info!("Restarting all services...");
        self.stop_services()?;
        self.start_all_services()?;
        self.spawn_monitor_thread()?;
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
        let hooks = service.hooks.clone();
        let pid_file = Arc::clone(&self.pid_file);
        let detach_children = self.detach_children;
        let working_dir = self.project_root.clone();

        let handle = thread::spawn(move || {
            debug!("Starting service thread for '{service_name}'");

            match Daemon::launch_attached_service(
                &service_name,
                &command,
                env,
                working_dir.clone(),
                processes.clone(),
                hooks,
                detach_children,
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
        let hooks = service.hooks.clone();
        let pid_file = Arc::clone(&self.pid_file);
        let detach_children = self.detach_children;
        let working_dir = self.project_root.clone();
        // Spawn the thread, but DO NOT join it.
        thread::spawn(move || {
            debug!("Starting service thread for '{service_name}'");

            match Daemon::launch_attached_service(
                &service_name,
                &command,
                env,
                working_dir.clone(),
                processes.clone(),
                hooks,
                detach_children,
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
    pub fn stop_service(&self, service_name: &str) -> Result<(), ProcessManagerError> {
        let pid = self.pid_file.lock()?.get(service_name);

        if let Some(process_id) = pid {
            let pid = nix::unistd::Pid::from_raw(process_id as i32);

            fn nix_error_to_io(err: nix::errno::Errno) -> std::io::Error {
                std::io::Error::from_raw_os_error(err as i32)
            }

            let mut process_running = true;

            match nix::sys::signal::kill(pid, None) {
                Ok(_) => {
                    debug!("Stopping service '{service_name}' (PID {pid})");
                }
                Err(err) => {
                    if err == nix::errno::Errno::ESRCH {
                        debug!("Service '{service_name}' no longer has a live process");
                        process_running = false;
                    } else {
                        let source = nix_error_to_io(err);
                        error!(
                            "Failed to probe service '{service_name}' before stopping: {source}"
                        );
                        return Err(ProcessManagerError::ServiceStopError {
                            service: service_name.to_string(),
                            source,
                        });
                    }
                }
            }

            if process_running {
                let supervisor_pgid = unsafe { libc::getpgid(0) };
                let child_pgid = unsafe { libc::getpgid(pid.as_raw()) };

                if child_pgid >= 0 && child_pgid != supervisor_pgid {
                    let kill_result = unsafe { libc::killpg(child_pgid, libc::SIGTERM) };
                    if kill_result < 0 {
                        let err = std::io::Error::last_os_error();
                        match err.raw_os_error() {
                            Some(code) if code == libc::ESRCH => {
                                debug!(
                                    "Process group for service '{service_name}' missing; falling back to direct signal"
                                );
                            }
                            Some(code) if code == libc::EPERM => {
                                warn!(
                                    "Insufficient permissions to signal process group {child_pgid} for '{service_name}'. Falling back to direct signal"
                                );
                            }
                            _ => {
                                error!(
                                    "Failed to kill process group {child_pgid} of service '{service_name}': {err}"
                                );
                                return Err(ProcessManagerError::ServiceStopError {
                                    service: service_name.to_string(),
                                    source: err,
                                });
                            }
                        }
                    } else {
                        debug!(
                            "Sent SIGTERM to process group {child_pgid} for service '{service_name}'"
                        );
                    }
                }

                if let Err(kill_err) =
                    nix::sys::signal::kill(pid, Some(nix::sys::signal::SIGTERM))
                {
                    if kill_err == nix::errno::Errno::ESRCH {
                        process_running = false;
                        debug!(
                            "Service '{service_name}' exited before SIGTERM could be delivered"
                        );
                    } else {
                        let source = nix_error_to_io(kill_err);
                        error!(
                            "Failed to signal service '{service_name}' directly: {source}"
                        );
                        return Err(ProcessManagerError::ServiceStopError {
                            service: service_name.to_string(),
                            source,
                        });
                    }
                }
            }

            if process_running {
                // Give the service a brief window to exit gracefully before escalating.
                const CHECKS: usize = 10;
                const INTERVAL: Duration = Duration::from_millis(100);

                for _ in 0..CHECKS {
                    std::thread::sleep(INTERVAL);
                    if matches!(
                        nix::sys::signal::kill(pid, None),
                        Err(nix::errno::Errno::ESRCH)
                    ) {
                        process_running = false;
                        break;
                    }
                }

                if process_running {
                    warn!(
                        "Service '{service_name}' did not exit after SIGTERM; sending SIGKILL"
                    );
                    if let Err(kill_err) =
                        nix::sys::signal::kill(pid, Some(nix::sys::signal::SIGKILL))
                    {
                        if kill_err != nix::errno::Errno::ESRCH {
                            let source = nix_error_to_io(kill_err);
                            error!(
                                "Failed to forcefully terminate service '{service_name}': {source}"
                            );
                            return Err(ProcessManagerError::ServiceStopError {
                                service: service_name.to_string(),
                                source,
                            });
                        }
                    } else {
                        let _ = nix::sys::signal::kill(pid, None);
                    }
                }
            }

            self.processes.lock()?.remove(service_name);
            if let Err(err) = self.pid_file.lock()?.remove(service_name) {
                match err {
                    PidFileError::ServiceNotFound => {
                        debug!("Service '{service_name}' already cleared from PID file");
                    }
                    _ => return Err(err.into()),
                }
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
    pub fn stop_services(&self) -> Result<(), ProcessManagerError> {
        let services: Vec<String> =
            self.pid_file.lock()?.services.keys().cloned().collect();

        for service in services {
            if let Err(e) = self.stop_service(&service) {
                error!("Failed to stop service '{service}': {e}");
            }
        }

        Ok(())
    }

    /// Ensures that the monitor thread is running, spawning it if necessary.
    fn spawn_monitor_thread(&self) -> Result<(), ProcessManagerError> {
        let mut handle_slot = self.monitor_handle.lock().unwrap();
        let should_spawn = match handle_slot.as_ref() {
            Some(handle) => handle.is_finished(),
            None => true,
        };

        if should_spawn {
            debug!("Starting service monitoring thread...");
            self.running.store(true, Ordering::SeqCst);

            let processes = Arc::clone(&self.processes);
            let config = Arc::clone(&self.config);
            let running = Arc::clone(&self.running);
            let pid_file = Arc::clone(&self.pid_file);
            let detach_children = self.detach_children;
            let project_root = self.project_root.clone();

            let handle = thread::spawn(move || {
                Self::monitor_loop(
                    processes,
                    config,
                    pid_file,
                    running,
                    detach_children,
                    project_root,
                );
            });

            *handle_slot = Some(handle);
        }

        Ok(())
    }

    /// Blocks on the monitoring thread if it is running.
    fn wait_for_monitor(&self) {
        if let Some(handle) = self.monitor_handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    /// Signals the monitoring thread to exit and waits for it to finish.
    pub fn shutdown_monitor(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.wait_for_monitor();
    }

    /// Monitors all running services and restarts them if they exit unexpectedly.
    fn monitor_loop(
        processes: Arc<Mutex<HashMap<String, Child>>>,
        config: Arc<Config>,
        pid_file: Arc<Mutex<PidFile>>,
        running: Arc<AtomicBool>,
        detach_children: bool,
        project_root: PathBuf,
    ) {
        while running.load(Ordering::SeqCst) {
            let mut exited_services = Vec::new();
            let mut restarted_services = Vec::new();
            let mut active_services = 0;

            {
                let mut locked_processes = processes.lock().unwrap();
                for (name, child) in locked_processes.iter_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            let exited_normally = status.success();
                            if exited_normally {
                                info!("Service '{name}' exited normally.");
                            } else {
                                warn!("Service '{name}' was terminated with {status:?}.");
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

            if !exited_services.is_empty() {
                let mut pid_file_guard = pid_file.lock().unwrap();

                for (name, exited_normally) in exited_services {
                    if pid_file_guard.get(&name).is_none() {
                        info!("Service '{name}' was manually stopped. Skipping restart.");
                    } else if !exited_normally {
                        warn!("Service '{name}' crashed. Restarting...");
                        restarted_services.push(name.clone());
                    } else {
                        debug!(
                            "Service '{name}' exited cleanly. Removing from PID file."
                        );
                        if let Err(e) = pid_file_guard.remove(&name) {
                            error!("Failed to remove '{name}' from PID file: {e}");
                        }
                    }

                    processes.lock().unwrap().remove(&name);
                }
            }

            if active_services == 0 {
                debug!("No active services detected in monitor loop.");
            }

            for name in restarted_services {
                if let Some(service) = config.services.get(&name) {
                    Self::handle_restart(
                        &name,
                        service,
                        Arc::clone(&processes),
                        detach_children,
                        Arc::clone(&pid_file),
                        project_root.clone(),
                    );
                }
            }

            thread::sleep(Duration::from_secs(2));
        }

        debug!("Monitor loop terminating.");
    }

    /// Handles restarting a service if its restart policy allows.
    fn handle_restart(
        name: &str,
        service: &ServiceConfig,
        processes: Arc<Mutex<HashMap<String, Child>>>,
        detach_children: bool,
        pid_file: Arc<Mutex<PidFile>>,
        project_root: PathBuf,
    ) {
        let name = name.to_string();
        let command = service.command.clone();
        let env = service.env.clone();
        let hooks = service.hooks.clone();

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

            if let Err(e) = Daemon::launch_attached_service(
                &name,
                &command,
                env.clone(),
                project_root.clone(),
                Arc::clone(&processes),
                hooks.clone(),
                detach_children,
            ) {
                error!("Failed to restart '{name}': {e}");
            } else if let Ok(mut pid_file_guard) = pid_file.lock()
                && let Ok(latest) = PidFile::reload()
            {
                *pid_file_guard = latest;
            }
        })
        .join();
    }
}
