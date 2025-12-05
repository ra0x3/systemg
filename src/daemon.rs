//! Module for managing and monitoring system services.
use crate::logs::{resolve_log_path, spawn_log_writer};
use reqwest::blocking::Client;
use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use serde_yaml;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File},
    io::{BufRead, BufReader, ErrorKind},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tracing::{debug, error, info, warn};

use crate::config::{
    Config, EnvConfig, HealthCheckConfig, HookAction, HookOutcome, HookStage,
    ServiceConfig, SkipConfig,
};
use crate::error::{PidFileError, ProcessManagerError, ServiceStateError};

/// Build the environment map for a service, giving inline `env.vars` precedence over entries loaded
/// from `env.file`.
fn collect_service_env(
    env: &Option<EnvConfig>,
    project_root: &Path,
    service_name: &str,
) -> HashMap<String, String> {
    let mut resolved = HashMap::new();

    if let Some(env_config) = env {
        if let Some(file_path) = env_config.path(project_root) {
            match fs::read_to_string(&file_path) {
                Ok(content) => {
                    for raw_line in content.lines() {
                        let line = raw_line.trim();
                        if line.is_empty() || line.starts_with('#') {
                            continue;
                        }

                        if let Some((key, value)) = line.split_once('=') {
                            let key = key.trim().to_string();
                            let mut value = value.trim().to_string();

                            if value.starts_with('"')
                                && value.ends_with('"')
                                && value.len() >= 2
                            {
                                value = value[1..value.len() - 1].to_string();
                            }

                            resolved.entry(key).or_insert(value);
                        } else {
                            warn!(
                                "Ignoring malformed line in env file for '{}': {}",
                                service_name, line
                            );
                        }
                    }
                }
                Err(err) => {
                    error!("Failed to read env file for '{}': {}", service_name, err);
                }
            }
        }

        if let Some(vars) = &env_config.vars {
            for (key, value) in vars {
                resolved.insert(key.clone(), value.clone());
            }
        }
    }

    resolved
}

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

/// Enumerates the persisted lifecycle states for managed services.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLifecycleStatus {
    Running,
    Skipped,
    ExitedSuccessfully,
    ExitedWithError,
    Stopped,
}

/// Persisted service runtime metadata used to inform status reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStateEntry {
    pub status: ServiceLifecycleStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

/// Persistent record of the last-known state for every managed service.
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct ServiceStateFile {
    services: HashMap<String, ServiceStateEntry>,
}

impl ServiceStateFile {
    fn path() -> PathBuf {
        let home = std::env::var("HOME").expect("HOME not set");
        PathBuf::from(format!("{}/.local/share/systemg/state.json", home))
    }

    pub fn load() -> Result<Self, ServiceStateError> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(path)?;
        let state = serde_json::from_str::<Self>(&contents)?;
        Ok(state)
    }

    pub fn save(&self) -> Result<(), ServiceStateError> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn services(&self) -> &HashMap<String, ServiceStateEntry> {
        &self.services
    }

    pub fn get(&self, service: &str) -> Option<&ServiceStateEntry> {
        self.services.get(service)
    }

    pub fn set(
        &mut self,
        service: &str,
        status: ServiceLifecycleStatus,
        pid: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), ServiceStateError> {
        self.services.insert(
            service.to_string(),
            ServiceStateEntry {
                status,
                pid,
                exit_code,
                signal,
            },
        );
        self.save()
    }

    pub fn remove(&mut self, service: &str) -> Result<(), ServiceStateError> {
        if self.services.remove(service).is_some() {
            self.save()
        } else {
            Err(ServiceStateError::ServiceNotFound)
        }
    }
}

/// Run a hook command with the provided environment variables.
fn run_hook(
    action: &HookAction,
    env: &Option<EnvConfig>,
    stage: HookStage,
    outcome: HookOutcome,
    service_name: &str,
    project_root: &Path,
) {
    let hook_label = format!("{}.{}", stage.as_ref(), outcome.as_ref());
    debug!(
        "Running {} hook for '{}': `{}`",
        hook_label, service_name, action.command
    );

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&action.command);

    for (key, value) in collect_service_env(env, project_root, service_name) {
        cmd.env(key, value);
    }

    let timeout = match action.timeout.as_deref() {
        Some(raw_timeout) => match Daemon::parse_duration(raw_timeout) {
            Ok(duration) => Some(duration),
            Err(err) => {
                error!(
                    "Invalid timeout '{}' for hook {} on '{}': {}",
                    raw_timeout, hook_label, service_name, err
                );
                None
            }
        },
        None => None,
    };

    match cmd.spawn() {
        Ok(mut child) => {
            let wait_result = match timeout {
                Some(duration) => wait_with_timeout(&mut child, duration),
                None => child.wait().map(Some),
            };

            match wait_result {
                Ok(Some(status)) => {
                    if status.success() {
                        debug!(
                            "{} hook for '{}' completed successfully.",
                            hook_label, service_name
                        );
                    } else {
                        warn!(
                            "{} hook for '{}' exited with status: {:?}",
                            hook_label, service_name, status
                        );
                    }
                }
                Ok(None) => {
                    if let Some(duration) = timeout {
                        warn!(
                            "{} hook for '{}' timed out after {:?}. Terminating hook process.",
                            hook_label, service_name, duration
                        );
                    } else {
                        warn!(
                            "{} hook for '{}' did not complete but no timeout was configured.",
                            hook_label, service_name
                        );
                    }
                    if let Err(err) = child.kill() {
                        error!(
                            "Failed to terminate timed-out hook {} for '{}': {}",
                            hook_label, service_name, err
                        );
                    }
                    let _ = child.wait();
                }
                Err(err) => {
                    error!(
                        "Failed while waiting for hook {} on '{}': {}",
                        hook_label, service_name, err
                    );
                }
            }
        }
        Err(e) => {
            error!(
                "Failed to run {} hook for '{}': {}",
                hook_label, service_name, e
            );
        }
    }
}

/// Wait for a child process with a timeout, returning `Ok(None)` on timeout.
fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> std::io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait()? {
            Some(status) => return Ok(Some(status)),
            None => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Snapshot of the currently observed state for a managed service during readiness probing.
#[derive(Debug)]
enum ServiceProbe {
    NotStarted,
    Running,
    Exited(ExitStatus),
}

/// Indicates when a service is considered ready for dependents or has already completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceReadyState {
    Running,
    CompletedSuccess,
}

/// Represents an existing service instance that has been temporarily detached while a
/// replacement is brought up during a rolling restart.
struct DetachedService {
    child: Child,
    pid: u32,
}

/// Manages services, ensuring they start, stop, and restart as needed.
pub struct Daemon {
    /// Shared map of running service processes.
    processes: Arc<Mutex<HashMap<String, Child>>>,
    /// Reference to the service configuration.
    config: Arc<Config>,
    /// The PID file for tracking service PIDs.
    pid_file: Arc<Mutex<PidFile>>,
    /// Persistent state for recording service lifecycle transitions.
    state_file: Arc<Mutex<ServiceStateFile>>,
    /// Whether child services should be detached from systemg (legacy behavior).
    detach_children: bool,
    /// Base directory for resolving relative service commands and assets.
    project_root: PathBuf,
    /// Flag indicating whether the monitoring loop should remain active.
    running: Arc<AtomicBool>,
    /// Handle to the background monitoring thread once spawned.
    monitor_handle: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    /// Tracks the number of restart attempts for each service.
    restart_counts: Arc<Mutex<HashMap<String, u32>>>,
}

impl Daemon {
    /// Initializes a new `Daemon` with an empty process map and a shared config reference.
    pub fn new(
        config: Config,
        pid_file: Arc<Mutex<PidFile>>,
        state_file: Arc<Mutex<ServiceStateFile>>,
        detach_children: bool,
    ) -> Self {
        debug!("Initializing daemon...");

        let project_root = config
            .project_dir
            .as_ref()
            .and_then(|dir| {
                let trimmed = dir.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(trimmed))
                }
            })
            .unwrap_or_else(|| PathBuf::from("."));

        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            config: Arc::new(config),
            pid_file,
            state_file,
            detach_children,
            running: Arc::new(AtomicBool::new(false)),
            monitor_handle: Arc::new(Mutex::new(None)),
            project_root,
            restart_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Convenience constructor that loads the PID file automatically.
    pub fn from_config(
        config: Config,
        detach_children: bool,
    ) -> Result<Self, ProcessManagerError> {
        let pid_file = Arc::new(Mutex::new(PidFile::load()?));
        let state_file = Arc::new(Mutex::new(ServiceStateFile::load()?));
        Ok(Self::new(config, pid_file, state_file, detach_children))
    }

    /// Explicitly records a skipped service in the persistent state store, clearing any stale PID.
    pub fn mark_service_skipped(&self, service: &str) -> Result<(), ProcessManagerError> {
        self.mark_skipped(service)
    }

    fn update_state(
        &self,
        service: &str,
        status: ServiceLifecycleStatus,
        pid: Option<u32>,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), ProcessManagerError> {
        let mut state = self.state_file.lock()?;
        state.set(service, status, pid, exit_code, signal)?;
        Ok(())
    }

    fn mark_running(&self, service: &str, pid: u32) -> Result<(), ProcessManagerError> {
        self.update_state(
            service,
            ServiceLifecycleStatus::Running,
            Some(pid),
            None,
            None,
        )
    }

    fn mark_skipped(&self, service: &str) -> Result<(), ProcessManagerError> {
        {
            let mut pid_guard = self.pid_file.lock()?;
            if let Err(err) = pid_guard.remove(service)
                && !matches!(err, PidFileError::ServiceNotFound)
            {
                return Err(err.into());
            }
        }

        self.update_state(service, ServiceLifecycleStatus::Skipped, None, None, None)
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
        detach_children: bool,
    ) -> Result<u32, ProcessManagerError> {
        debug!("Launching service: '{service_name}' with command: `{command}`");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd.current_dir(&working_dir);

        debug!("Executing command: {cmd:?}");

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let merged_env = collect_service_env(&env, &working_dir, service_name);
        if !merged_env.is_empty() {
            let keys: Vec<_> = merged_env.keys().cloned().collect();
            debug!("Setting environment variables: {:?}", keys);
            for (key, value) in merged_env {
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

    /// Ensures the background monitor thread is running.
    ///
    /// The supervisor starts services individually in daemon mode, so it needs
    /// a way to activate the monitor loop afterwards. This method is a thin
    /// wrapper around the internal `spawn_monitor_thread` helper that keeps the
    /// existing guard logic (only spawning when no active monitor exists).
    pub fn ensure_monitoring(&self) -> Result<(), ProcessManagerError> {
        self.spawn_monitor_thread()
    }

    /// Executes the start workflow without waiting on the monitor thread.
    fn start_all_services(&self) -> Result<(), ProcessManagerError> {
        info!("Starting all services...");

        let order = self.config.service_start_order()?;
        let mut healthy_services = HashSet::new();
        let mut failed_services = HashSet::new();
        let mut first_error: Option<ProcessManagerError> = None;

        'service_loop: for service_name in order {
            let service = match self.config.services.get(&service_name) {
                Some(service) => service,
                None => continue,
            };

            if let Some(skip_config) = &service.skip {
                match skip_config {
                    SkipConfig::Flag(true) => {
                        info!("Skipping service '{service_name}' due to skip flag");
                        healthy_services.insert(service_name.clone());
                        continue 'service_loop;
                    }
                    SkipConfig::Flag(false) => {
                        debug!(
                            "Skip flag for '{service_name}' disabled; starting service"
                        );
                    }
                    SkipConfig::Command(skip_command) => {
                        match self.evaluate_skip_condition(&service_name, skip_command) {
                            Ok(true) => {
                                info!(
                                    "Skipping service '{service_name}' due to skip condition"
                                );
                                healthy_services.insert(service_name.clone());
                                continue 'service_loop;
                            }
                            Ok(false) => {
                                debug!(
                                    "Skip condition for '{service_name}' evaluated to false, starting service"
                                );
                            }
                            Err(err) => {
                                error!(
                                    "Failed to evaluate skip condition for '{service_name}': {err}"
                                );
                                if first_error.is_none() {
                                    first_error = Some(err);
                                }
                                failed_services.insert(service_name.clone());
                                continue 'service_loop;
                            }
                        }
                    }
                }
            }

            if let Some(deps) = &service.depends_on {
                for dep in deps {
                    if failed_services.contains(dep) {
                        error!(
                            "Skipping start of '{service_name}' because dependency '{dep}' failed."
                        );
                        if first_error.is_none() {
                            first_error = Some(ProcessManagerError::DependencyFailed {
                                service: service_name.clone(),
                                dependency: dep.clone(),
                            });
                        }
                        failed_services.insert(service_name.clone());
                        continue 'service_loop;
                    }

                    if !healthy_services.contains(dep) {
                        error!(
                            "Skipping start of '{service_name}' because dependency '{dep}' is not running."
                        );
                        if first_error.is_none() {
                            first_error = Some(ProcessManagerError::DependencyError {
                                service: service_name.clone(),
                                dependency: dep.clone(),
                            });
                        }
                        failed_services.insert(service_name.clone());
                        continue 'service_loop;
                    }
                }
            }

            match self.start_service(&service_name, service) {
                Ok(ServiceReadyState::Running) => {
                    healthy_services.insert(service_name.clone());
                }
                Ok(ServiceReadyState::CompletedSuccess) => {
                    info!("Service '{service_name}' completed successfully.");
                    healthy_services.insert(service_name.clone());
                }
                Err(err) => {
                    error!("Failed to start service '{service_name}': {err}");
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                    failed_services.insert(service_name.clone());
                }
            }
        }

        if let Some(err) = first_error {
            return Err(err);
        }

        info!("All services started successfully.");

        thread::sleep(Duration::from_millis(200));
        Ok(())
    }

    /// Evaluates a skip condition command for a service.
    /// Returns Ok(true) if the service should be skipped (command exits with status 0),
    /// Ok(false) if the service should not be skipped (command exits with non-zero status),
    /// or Err if the command fails to execute.
    pub fn evaluate_skip_condition(
        &self,
        service_name: &str,
        skip_command: &str,
    ) -> Result<bool, ProcessManagerError> {
        debug!("Evaluating skip condition for '{service_name}': `{skip_command}`");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(skip_command);
        cmd.current_dir(&self.project_root);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());

        match cmd.status() {
            Ok(status) => {
                let should_skip = status.success();
                debug!(
                    "Skip condition for '{service_name}' evaluated to: {should_skip} (exit code: {:?})",
                    status.code()
                );
                Ok(should_skip)
            }
            Err(e) => {
                error!("Failed to execute skip condition for '{service_name}': {e}");
                Err(ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source: e,
                })
            }
        }
    }

    /// Polls the newly spawned service until it is confirmed running or exits.
    ///
    /// Returns [`ServiceReadyState::Running`] once the process stays alive across consecutive
    /// polls, [`ServiceReadyState::CompletedSuccess`] for one-shot services that exit cleanly,
    /// or a [`ProcessManagerError::ServiceStartError`] if the process terminates with failure or
    /// fails to signal readiness within the allotted time window.
    fn wait_for_service_ready(
        &self,
        service_name: &str,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        Self::wait_for_service_ready_with_handles(
            service_name,
            &self.processes,
            &self.pid_file,
        )
    }

    fn wait_for_service_ready_with_handles(
        service_name: &str,
        processes: &Arc<Mutex<HashMap<String, Child>>>,
        pid_file: &Arc<Mutex<PidFile>>,
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        const MAX_WAIT: Duration = Duration::from_secs(5);
        const POLL_INTERVAL: Duration = Duration::from_millis(50);

        let mut waited = Duration::ZERO;
        let mut seen_running_once = false;

        while waited <= MAX_WAIT {
            match Self::probe_service_state(service_name, processes, pid_file)? {
                ServiceProbe::Running => {
                    if seen_running_once {
                        return Ok(ServiceReadyState::Running);
                    }

                    seen_running_once = true;
                    thread::sleep(POLL_INTERVAL);
                    waited += POLL_INTERVAL;
                    continue;
                }
                ServiceProbe::Exited(status) => {
                    if status.success() {
                        return Ok(ServiceReadyState::CompletedSuccess);
                    }

                    let message = match status.code() {
                        Some(code) => format!("process exited with status {code}"),
                        None => format!("process terminated unexpectedly: {status:?}"),
                    };

                    return Err(ProcessManagerError::ServiceStartError {
                        service: service_name.to_string(),
                        source: std::io::Error::other(message),
                    });
                }
                ServiceProbe::NotStarted => {
                    thread::sleep(POLL_INTERVAL);
                    waited += POLL_INTERVAL;
                    continue;
                }
            }
        }

        Err(ProcessManagerError::ServiceStartError {
            service: service_name.to_string(),
            source: std::io::Error::new(
                ErrorKind::TimedOut,
                "service did not report a running state in time",
            ),
        })
    }

    /// Attempts to determine the current state of a tracked service without blocking.
    ///
    /// Uses `try_wait` to check the underlying child process and updates the PID file if the
    /// service has exited. To avoid holding the process map lock longer than necessary, the child
    /// handle is temporarily removed and inserted back when still running.
    fn probe_service_state(
        service_name: &str,
        processes: &Arc<Mutex<HashMap<String, Child>>>,
        pid_file: &Arc<Mutex<PidFile>>,
    ) -> Result<ServiceProbe, ProcessManagerError> {
        let mut processes_guard = processes.lock()?;

        if let Some(mut child) = processes_guard.remove(service_name) {
            match child.try_wait() {
                Ok(Some(status)) => {
                    drop(processes_guard);

                    let mut pid_guard = pid_file.lock()?;
                    if let Err(err) = pid_guard.remove(service_name)
                        && !matches!(err, PidFileError::ServiceNotFound)
                    {
                        return Err(err.into());
                    }

                    return Ok(ServiceProbe::Exited(status));
                }
                Ok(None) => {
                    processes_guard.insert(service_name.to_string(), child);
                    return Ok(ServiceProbe::Running);
                }
                Err(e) => {
                    processes_guard.insert(service_name.to_string(), child);
                    return Err(ProcessManagerError::ServiceStartError {
                        service: service_name.to_string(),
                        source: e,
                    });
                }
            }
        }

        Ok(ServiceProbe::NotStarted)
    }

    /// Restarts all services by stopping and then starting them again, reusing the existing
    /// monitor thread if available.
    pub fn restart_services(&self) -> Result<(), ProcessManagerError> {
        info!("Restarting all services...");

        let order = self.config.service_start_order()?;
        let mut restarted_services = Vec::new();

        for service_name in order {
            let service = match self.config.services.get(&service_name) {
                Some(service) => service,
                None => continue,
            };

            restarted_services.push(service_name.clone());

            let strategy = service
                .deployment
                .as_ref()
                .and_then(|deployment| deployment.strategy.as_deref())
                .unwrap_or("immediate");

            match strategy {
                "rolling" => {
                    self.rolling_restart_service(&service_name, service)?;
                }
                "immediate" => {
                    self.immediate_restart_service(&service_name, service)?;
                }
                other => {
                    warn!(
                        "Unknown deployment strategy '{other}' for service '{service_name}', falling back to immediate restart."
                    );
                    self.immediate_restart_service(&service_name, service)?;
                }
            }
        }

        self.spawn_monitor_thread()?;
        self.verify_services_running(&restarted_services)?;
        info!("All services restarted successfully.");
        Ok(())
    }

    /// Restarts a single service, honoring its deployment strategy.
    pub fn restart_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<(), ProcessManagerError> {
        let strategy = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.strategy.as_deref())
            .unwrap_or("immediate");

        match strategy {
            "rolling" => {
                self.rolling_restart_service(name, service)?;
            }
            "immediate" => {
                self.immediate_restart_service(name, service)?;
            }
            other => {
                warn!(
                    "Unknown deployment strategy '{other}' for service '{name}', falling back to immediate restart."
                );
                self.immediate_restart_service(name, service)?;
            }
        }

        self.verify_services_running(&[name.to_string()])?;

        Ok(())
    }

    /// Performs a rolling restart keeping the previous instance alive until the replacement is
    /// verified healthy.
    fn rolling_restart_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<(), ProcessManagerError> {
        info!("Performing rolling restart for service: {name}");

        let mut previous = self.detach_service_handle(name)?;

        if let Some(pre_start) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.pre_start.as_ref())
        {
            info!("Running pre-start command for '{name}': {pre_start}");
            if let Err(err) = self.run_pre_start_command(name, pre_start) {
                if let Some(detached) = previous.take() {
                    self.restore_detached_service(name, detached)?;
                }
                return Err(err);
            }
        }

        let start_state = match self.start_service(name, service) {
            Ok(state) => state,
            Err(err) => {
                if let Err(stop_err) = self.stop_service(name) {
                    warn!(
                        "Failed to stop new instance of '{name}' after restart error: {stop_err}"
                    );
                }

                if previous.is_some() && Self::logs_indicate_port_conflict(name) {
                    warn!(
                        "Detected port conflict while restarting '{name}'. Falling back to immediate restart semantics."
                    );

                    if let Some(detached) = previous.take() {
                        self.terminate_detached_service(name, detached)?;
                    }

                    self.start_service(name, service)?
                } else {
                    if let Some(detached) = previous.take() {
                        self.restore_detached_service(name, detached)?;
                    }
                    return Err(err);
                }
            }
        };

        if matches!(start_state, ServiceReadyState::CompletedSuccess) {
            info!("Service '{name}' exited successfully immediately after restart.");
        }

        if let Some(health_check) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.health_check.as_ref())
            && let Err(err) = self.wait_for_health_check(name, health_check)
        {
            error!("Health check failed for '{name}' during rolling restart: {err}");
            if let Err(stop_err) = self.stop_service(name) {
                warn!(
                    "Failed to stop new instance of '{name}' after health check failure: {stop_err}"
                );
            }
            if let Some(detached) = previous.take() {
                self.restore_detached_service(name, detached)?;
            }
            return Err(err);
        }

        if let Some(grace_period) = service
            .deployment
            .as_ref()
            .and_then(|deployment| deployment.grace_period.as_ref())
        {
            let duration = match Self::parse_duration(grace_period) {
                Ok(duration) => duration,
                Err(err) => {
                    error!(
                        "Failed to parse grace period '{grace_period}' for '{name}': {err}"
                    );
                    if let Err(stop_err) = self.stop_service(name) {
                        warn!(
                            "Failed to stop new instance of '{name}' after grace period parse error: {stop_err}"
                        );
                    }
                    if let Some(detached) = previous.take() {
                        self.restore_detached_service(name, detached)?;
                    }
                    return Err(err);
                }
            };

            if !duration.is_zero() {
                info!(
                    "Waiting {:?} before stopping previous instance of '{name}'",
                    duration
                );
                thread::sleep(duration);
            }
        }

        if let Some(detached) = previous.take() {
            self.terminate_detached_service(name, detached)?;
        }

        Ok(())
    }

    /// Performs an immediate restart by stopping and starting the service sequentially.
    fn immediate_restart_service(
        &self,
        name: &str,
        service: &ServiceConfig,
    ) -> Result<(), ProcessManagerError> {
        info!("Performing immediate restart for service: {name}");

        self.stop_service(name)?;
        let start_state = self.start_service(name, service)?;

        if let ServiceReadyState::CompletedSuccess = start_state {
            info!("Service '{name}' completed successfully immediately after restart.");
        }

        Ok(())
    }

    /// Runs the configured pre-start command prior to launching a replacement service instance.
    fn run_pre_start_command(
        &self,
        service_name: &str,
        command: &str,
    ) -> Result<(), ProcessManagerError> {
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;
        use std::thread;

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.project_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source,
            })?;

        let service_name_owned = service_name.to_string();

        // Stream stdout in a separate thread
        let stdout_handle = child.stdout.take().map(|stdout| {
            let service_name = service_name_owned.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    info!("[{service_name} pre-start] {line}");
                }
            })
        });

        // Stream stderr in a separate thread
        let stderr_handle = child.stderr.take().map(|stderr| {
            let service_name = service_name_owned.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    warn!("[{service_name} pre-start] {line}");
                }
            })
        });

        // Wait for the process to complete
        let status =
            child
                .wait()
                .map_err(|source| ProcessManagerError::ServiceStartError {
                    service: service_name.to_string(),
                    source,
                })?;

        // Wait for output threads to finish
        if let Some(handle) = stdout_handle {
            let _ = handle.join();
        }
        if let Some(handle) = stderr_handle {
            let _ = handle.join();
        }

        if !status.success() {
            let message = format!("Pre-start command exited with status {}", status);
            return Err(ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source: std::io::Error::other(message),
            });
        }

        Ok(())
    }

    /// Waits for the configured health check to report success before completing the rolling
    /// restart.
    fn wait_for_health_check(
        &self,
        service_name: &str,
        health_check: &HealthCheckConfig,
    ) -> Result<(), ProcessManagerError> {
        let timeout = if let Some(raw_timeout) = &health_check.timeout {
            Self::parse_duration(raw_timeout)?
        } else {
            Duration::from_secs(30)
        };

        let retries = health_check.retries.unwrap_or(3).max(1);
        let retry_interval = Duration::from_secs(2);
        let client = Client::builder().timeout(timeout).build().map_err(|err| {
            ProcessManagerError::ServiceStartError {
                service: service_name.to_string(),
                source: std::io::Error::other(err.to_string()),
            }
        })?;

        let deadline = Instant::now() + timeout;

        for attempt in 1..=retries {
            match self.perform_health_check(&client, &health_check.url) {
                Ok(true) => {
                    info!(
                        "Health check passed for '{service_name}' on attempt {attempt}"
                    );
                    return Ok(());
                }
                Ok(false) => {
                    debug!(
                        "Health check attempt {attempt} failed for '{service_name}', retrying in {:?}",
                        retry_interval
                    );
                }
                Err(err) => {
                    debug!(
                        "Health check attempt {attempt} returned error for '{service_name}': {err}",
                    );
                }
            }

            if Instant::now() >= deadline {
                break;
            }

            if attempt != retries {
                thread::sleep(retry_interval);
            }
        }

        Err(ProcessManagerError::ServiceStartError {
            service: service_name.to_string(),
            source: std::io::Error::other(format!(
                "Health check did not succeed within {:?} after {} attempts",
                timeout, retries
            )),
        })
    }

    /// Performs a single health check request and evaluates the response.
    fn perform_health_check(
        &self,
        client: &Client,
        url: &str,
    ) -> Result<bool, std::io::Error> {
        let response = client
            .get(url)
            .send()
            .map_err(|err| std::io::Error::other(err.to_string()))?;

        Ok(response.status().is_success())
    }

    /// Parses a user-facing duration string in the format `<number>[s|m|h]`.
    fn parse_duration(raw: &str) -> Result<Duration, ProcessManagerError> {
        let value = raw.trim();
        if value.is_empty() {
            return Err(Self::config_error("Duration value cannot be empty"));
        }

        let (amount_str, multiplier) = if let Some(stripped) = value.strip_suffix('s') {
            (stripped.trim(), 1)
        } else if let Some(stripped) = value.strip_suffix('m') {
            (stripped.trim(), 60)
        } else if let Some(stripped) = value.strip_suffix('h') {
            (stripped.trim(), 3600)
        } else {
            (value, 1)
        };

        let amount: u64 = amount_str.parse().map_err(|_| {
            Self::config_error(format!("Invalid duration value: '{raw}'"))
        })?;

        Ok(Duration::from_secs(amount.saturating_mul(multiplier)))
    }

    fn logs_indicate_port_conflict(service_name: &str) -> bool {
        const MAX_LINES: usize = 50;

        let path = resolve_log_path(service_name, "stderr");
        if !path.exists() {
            return false;
        }

        match File::open(&path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                let mut buffer: VecDeque<String> = VecDeque::with_capacity(MAX_LINES);

                for line in reader.lines().map_while(Result::ok) {
                    if buffer.len() == MAX_LINES {
                        buffer.pop_front();
                    }
                    buffer.push_back(line);
                }

                buffer.iter().rev().any(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower.contains("address already in use")
                        || lower.contains("os error 48")
                        || lower.contains("os error 98")
                        || lower.contains("eaddrinuse")
                })
            }
            Err(err) => {
                debug!(
                    "Unable to inspect stderr logs for '{}' while detecting port conflicts: {err}",
                    service_name
                );
                false
            }
        }
    }

    fn should_verify_service(service: &ServiceConfig) -> bool {
        if matches!(service.restart_policy.as_deref(), Some("never")) {
            return false;
        }

        if service.cron.is_some() {
            return false;
        }

        true
    }

    fn verify_services_running(
        &self,
        services: &[String],
    ) -> Result<(), ProcessManagerError> {
        const POST_RESTART_VERIFY_ATTEMPTS: usize = 2;
        const POST_RESTART_VERIFY_DELAY: Duration = Duration::from_millis(200);

        let mut failed = Vec::new();

        for service_name in services {
            let Some(service_cfg) = self.config.services.get(service_name) else {
                continue;
            };

            if !Self::should_verify_service(service_cfg) {
                continue;
            }

            let mut stable = true;

            for attempt in 0..POST_RESTART_VERIFY_ATTEMPTS {
                if attempt > 0 {
                    thread::sleep(POST_RESTART_VERIFY_DELAY);
                }

                match Self::probe_service_state(
                    service_name,
                    &self.processes,
                    &self.pid_file,
                )? {
                    ServiceProbe::Running => continue,
                    ServiceProbe::NotStarted => {
                        stable = false;
                        break;
                    }
                    ServiceProbe::Exited(status) => {
                        if status.success() {
                            info!(
                                "Service '{service_name}' exited immediately after restart with status {status}"
                            );
                        } else {
                            warn!(
                                "Service '{service_name}' crashed immediately after restart with status {status}"
                            );
                        }
                        stable = false;
                        break;
                    }
                }
            }

            if !stable {
                failed.push(service_name.clone());
            }
        }

        if failed.is_empty() {
            Ok(())
        } else {
            Err(ProcessManagerError::ServicesNotRunning { services: failed })
        }
    }

    /// Helper for constructing a configuration parse error wrapped in our domain error type.
    fn config_error(message: impl Into<String>) -> ProcessManagerError {
        ProcessManagerError::ConfigParseError(serde_yaml::Error::custom(message.into()))
    }

    /// Removes the current child handle for a service while keeping the underlying process alive.
    fn detach_service_handle(
        &self,
        service_name: &str,
    ) -> Result<Option<DetachedService>, ProcessManagerError> {
        let detached_child = self.processes.lock()?.remove(service_name);

        if let Some(child) = detached_child {
            let pid = self
                .pid_file
                .lock()?
                .pid_for(service_name)
                .unwrap_or(child.id());

            Ok(Some(DetachedService { child, pid }))
        } else {
            Ok(None)
        }
    }

    /// Restores a previously detached service handle back into normal supervision.
    fn restore_detached_service(
        &self,
        service_name: &str,
        detached: DetachedService,
    ) -> Result<(), ProcessManagerError> {
        self.processes
            .lock()?
            .insert(service_name.to_string(), detached.child);

        self.pid_file.lock()?.insert(service_name, detached.pid)?;

        info!("Restored original instance of '{service_name}' after restart failure.");

        Ok(())
    }

    /// Terminates the detached instance once the replacement is known healthy.
    fn terminate_detached_service(
        &self,
        service_name: &str,
        mut detached: DetachedService,
    ) -> Result<(), ProcessManagerError> {
        let pid = detached.pid;
        if let Err(err) = Self::terminate_pid(pid, service_name) {
            error!("Failed to terminate previous instance of '{service_name}': {err}");
            return Err(err);
        }

        // Best-effort wait to reap the child and avoid zombies.
        if let Err(err) = detached.child.wait() {
            warn!(
                "Failed to wait on previous instance of '{service_name}' after termination: {err}"
            );
        }

        info!(
            "Old instance of '{service_name}' terminated successfully during rolling restart."
        );

        Ok(())
    }

    /// Sends termination signals mirroring the standard stop workflow for a specific PID.
    fn terminate_pid(pid: u32, service_name: &str) -> Result<(), ProcessManagerError> {
        fn nix_error_to_io(err: nix::errno::Errno) -> std::io::Error {
            std::io::Error::from_raw_os_error(err as i32)
        }

        let pid = nix::unistd::Pid::from_raw(pid as i32);
        let mut process_running = true;

        match nix::sys::signal::kill(pid, None) {
            Ok(_) => {
                debug!("Stopping previous instance of '{service_name}' (PID {pid})");
            }
            Err(err) => {
                if err == nix::errno::Errno::ESRCH {
                    process_running = false;
                } else {
                    let source = nix_error_to_io(err);
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
                        Some(code) if code == libc::ESRCH => {}
                        Some(code) if code == libc::EPERM => {
                            warn!(
                                "Insufficient permissions to signal process group {child_pgid} for '{service_name}'. Falling back to direct signal"
                            );
                        }
                        _ => {
                            return Err(ProcessManagerError::ServiceStopError {
                                service: service_name.to_string(),
                                source: err,
                            });
                        }
                    }
                }
            }

            if let Err(err) = nix::sys::signal::kill(pid, Some(nix::sys::signal::SIGTERM))
            {
                if err == nix::errno::Errno::ESRCH {
                    process_running = false;
                } else {
                    let source = nix_error_to_io(err);
                    return Err(ProcessManagerError::ServiceStopError {
                        service: service_name.to_string(),
                        source,
                    });
                }
            }
        }

        if process_running {
            const CHECKS: usize = 10;
            const INTERVAL: Duration = Duration::from_millis(100);

            for _ in 0..CHECKS {
                thread::sleep(INTERVAL);
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
                    "Previous instance of '{service_name}' did not exit after SIGTERM; sending SIGKILL"
                );
                if let Err(err) =
                    nix::sys::signal::kill(pid, Some(nix::sys::signal::SIGKILL))
                    && err != nix::errno::Errno::ESRCH
                {
                    let source = nix_error_to_io(err);
                    return Err(ProcessManagerError::ServiceStopError {
                        service: service_name.to_string(),
                        source,
                    });
                }
            }
        }

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
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        info!("Starting service: {name}");

        // Check skip flag
        if let Some(skip_config) = &service.skip {
            match skip_config {
                SkipConfig::Flag(true) => {
                    info!("Skipping service '{name}' due to skip flag");
                    self.mark_skipped(name)?;
                    return Ok(ServiceReadyState::CompletedSuccess);
                }
                SkipConfig::Flag(false) => {
                    debug!("Skip flag for '{name}' disabled; starting service");
                }
                SkipConfig::Command(skip_command) => {
                    match self.evaluate_skip_condition(name, skip_command) {
                        Ok(true) => {
                            info!("Skipping service '{name}' due to skip condition");
                            self.mark_skipped(name)?;
                            return Ok(ServiceReadyState::CompletedSuccess);
                        }
                        Ok(false) => {
                            debug!(
                                "Skip condition for '{name}' evaluated to false, starting service"
                            );
                        }
                        Err(err) => {
                            warn!(
                                "Failed to evaluate skip condition for '{name}': {err}"
                            );
                        }
                    }
                }
            }
        }

        let processes = Arc::clone(&self.processes);
        let command = service.command.clone();
        let env = service.env.clone();
        let service_name = name.to_string();
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
        let launch_result = handle.join().map_err(|e| {
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

        match launch_result {
            Ok(pid) => {
                self.mark_running(name, pid)?;
            }
            Err(err) => {
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Error))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Error,
                        name,
                        &self.project_root,
                    );
                }
                return Err(err);
            }
        }

        let readiness = self.wait_for_service_ready(name);

        match readiness {
            Ok(state) => {
                if matches!(state, ServiceReadyState::CompletedSuccess) {
                    self.update_state(
                        name,
                        ServiceLifecycleStatus::ExitedSuccessfully,
                        None,
                        Some(0),
                        None,
                    )?;
                }
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Success))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Success,
                        name,
                        &self.project_root,
                    );
                }
                Ok(state)
            }
            Err(err) => {
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Error))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Error,
                        name,
                        &self.project_root,
                    );
                }
                Err(err)
            }
        }
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
    ) -> Result<ServiceReadyState, ProcessManagerError> {
        use std::sync::mpsc;
        use std::thread;

        info!("Starting service: {name}");

        // Check skip flag
        if let Some(skip_config) = &service.skip {
            match skip_config {
                SkipConfig::Flag(true) => {
                    info!("Skipping service '{name}' due to skip flag");
                    self.mark_skipped(name)?;
                    return Ok(ServiceReadyState::CompletedSuccess);
                }
                SkipConfig::Flag(false) => {
                    debug!("Skip flag for '{name}' disabled; starting service");
                }
                SkipConfig::Command(skip_command) => {
                    match self.evaluate_skip_condition(name, skip_command) {
                        Ok(true) => {
                            info!("Skipping service '{name}' due to skip condition");
                            self.mark_skipped(name)?;
                            return Ok(ServiceReadyState::CompletedSuccess);
                        }
                        Ok(false) => {
                            debug!(
                                "Skip condition for '{name}' evaluated to false, starting service"
                            );
                        }
                        Err(err) => {
                            warn!(
                                "Failed to evaluate skip condition for '{name}': {err}"
                            );
                        }
                    }
                }
            }
        }

        let processes = Arc::clone(&self.processes);
        let command = service.command.clone();
        let env = service.env.clone();
        let service_name = name.to_string();
        let pid_file = Arc::clone(&self.pid_file);
        let detach_children = self.detach_children;
        let working_dir = self.project_root.clone();
        let (tx, rx) = mpsc::channel();
        // Spawn the thread, but DO NOT join it.
        thread::spawn(move || {
            debug!("Starting service thread for '{service_name}'");

            let launch_result = Daemon::launch_attached_service(
                &service_name,
                &command,
                env,
                working_dir.clone(),
                processes.clone(),
                detach_children,
            );

            match launch_result {
                Ok(pid) => {
                    match pid_file.lock() {
                        Ok(mut guard) => {
                            if let Err(err) = guard.insert(&service_name, pid) {
                                error!(
                                    "Failed to record PID for service '{service_name}': {}",
                                    err
                                );
                                let _ = tx.send(Err(err.into()));
                                return;
                            }
                        }
                        Err(poison) => {
                            error!(
                                "Pid file mutex poisoned while starting '{}': {}",
                                service_name, poison
                            );
                            let _ = tx.send(Err(ProcessManagerError::from(poison)));
                            return;
                        }
                    }

                    if tx.send(Ok(pid)).is_err() {
                        return;
                    }

                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(60));
                    }
                }
                Err(e) => {
                    error!("Failed to start service '{service_name}': {e}");
                    let _ = tx.send(Err(e));
                }
            }
        });

        debug!("Service thread for '{name}' launched and detached (Linux)");

        let launch_result =
            rx.recv()
                .map_err(|recv_err| ProcessManagerError::ServiceStartError {
                    service: name.to_string(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        format!("thread failed to report launch status: {recv_err}"),
                    ),
                })?;

        match launch_result {
            Ok(pid) => {
                self.mark_running(name, pid)?;
            }
            Err(err) => {
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Error))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Error,
                        name,
                        &self.project_root,
                    );
                }
                return Err(err);
            }
        }

        let readiness = self.wait_for_service_ready(name);

        match readiness {
            Ok(state) => {
                if matches!(state, ServiceReadyState::CompletedSuccess) {
                    self.update_state(
                        name,
                        ServiceLifecycleStatus::ExitedSuccessfully,
                        None,
                        Some(0),
                        None,
                    )?;
                }
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Success))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Success,
                        name,
                        &self.project_root,
                    );
                }
                Ok(state)
            }
            Err(err) => {
                if let Some(action) = service
                    .hooks
                    .as_ref()
                    .and_then(|cfg| cfg.action(HookStage::OnStart, HookOutcome::Error))
                {
                    run_hook(
                        action,
                        &service.env,
                        HookStage::OnStart,
                        HookOutcome::Error,
                        name,
                        &self.project_root,
                    );
                }
                Err(err)
            }
        }
    }

    /// Shared stop implementation that accepts explicit handles, making it reusable from helpers
    /// that already hold references to the daemon's shared state.
    fn stop_service_with_handles(
        service_name: &str,
        processes: &Arc<Mutex<HashMap<String, Child>>>,
        pid_file: &Arc<Mutex<PidFile>>,
        state_file: &Arc<Mutex<ServiceStateFile>>,
    ) -> Result<(), ProcessManagerError> {
        let active_pid = {
            let processes_guard = processes.lock()?;
            processes_guard.get(service_name).map(|child| child.id())
        };

        let pid = if let Some(pid) = active_pid {
            Some(pid)
        } else {
            pid_file.lock()?.get(service_name)
        };

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

            processes.lock()?.remove(service_name);
            if let Err(err) = pid_file.lock()?.remove(service_name) {
                match err {
                    PidFileError::ServiceNotFound => {
                        debug!("Service '{service_name}' already cleared from PID file");
                    }
                    _ => return Err(err.into()),
                }
            }

            if let Ok(mut guard) = state_file.lock()
                && let Err(err) = guard.set(
                    service_name,
                    ServiceLifecycleStatus::Stopped,
                    None,
                    None,
                    None,
                )
            {
                warn!("Failed to persist stopped state for '{service_name}': {err}");
            }

            debug!("Service '{service_name}' stopped successfully.");
        } else {
            warn!("Service '{service_name}' not found in PID file.");
        }

        Ok(())
    }

    /// Stops a specific service by name.
    ///
    /// If the service is running, it will be terminated and removed from the process map.
    /// This function correctly handles both Unix/macOS and Linux semantics for process groups.
    pub fn stop_service(&self, service_name: &str) -> Result<(), ProcessManagerError> {
        let was_running = { self.pid_file.lock()?.get(service_name).is_some() };

        let result = Self::stop_service_with_handles(
            service_name,
            &self.processes,
            &self.pid_file,
            &self.state_file,
        );

        if was_running
            && result.is_ok()
            && let Some(service) = self.config.services.get(service_name)
            && let Some(hooks) = &service.hooks
            && let Some(action) = hooks.action(HookStage::OnStop, HookOutcome::Success)
        {
            run_hook(
                action,
                &service.env,
                HookStage::OnStop,
                HookOutcome::Success,
                service_name,
                &self.project_root,
            );
        }

        result
    }

    /// Recursively stops any services that depend (directly or indirectly) on the specified root
    /// service. Used when a dependency crashes so downstream workloads do not continue in a broken
    /// state.
    fn stop_dependents(
        root: &str,
        reverse_dependencies: &HashMap<String, Vec<String>>,
        processes: &Arc<Mutex<HashMap<String, Child>>>,
        pid_file: &Arc<Mutex<PidFile>>,
        state_file: &Arc<Mutex<ServiceStateFile>>,
    ) {
        let mut stack: Vec<String> =
            reverse_dependencies.get(root).cloned().unwrap_or_default();
        let mut visited: HashSet<String> = stack.iter().cloned().collect();

        while let Some(service) = stack.pop() {
            warn!("Stopping dependent service '{service}' because '{root}' failed.");

            if let Err(err) =
                Self::stop_service_with_handles(&service, processes, pid_file, state_file)
            {
                error!(
                    "Failed to stop dependent service '{service}' after '{root}' failure: {err}"
                );
            }

            if let Some(children) = reverse_dependencies.get(&service) {
                for child in children {
                    if visited.insert(child.clone()) {
                        stack.push(child.clone());
                    }
                }
            }
        }
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
            let state_file = Arc::clone(&self.state_file);
            let detach_children = self.detach_children;
            let project_root = self.project_root.clone();
            let restart_counts = Arc::clone(&self.restart_counts);

            let handle = thread::spawn(move || {
                Self::monitor_loop(
                    processes,
                    config,
                    pid_file,
                    state_file,
                    running,
                    detach_children,
                    project_root,
                    restart_counts,
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
    #[allow(clippy::too_many_arguments)]
    fn monitor_loop(
        processes: Arc<Mutex<HashMap<String, Child>>>,
        config: Arc<Config>,
        pid_file: Arc<Mutex<PidFile>>,
        state_file: Arc<Mutex<ServiceStateFile>>,
        running: Arc<AtomicBool>,
        detach_children: bool,
        project_root: PathBuf,
        restart_counts: Arc<Mutex<HashMap<String, u32>>>,
    ) {
        while running.load(Ordering::SeqCst) {
            let mut exited_services = Vec::new();
            let mut restarted_services = Vec::new();
            let mut failed_services = Vec::new();
            let mut active_services = 0;

            {
                let mut locked_processes = processes.lock().unwrap();
                for (name, child) in locked_processes.iter_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            if status.success() {
                                info!("Service '{name}' exited normally.");
                            } else {
                                warn!("Service '{name}' was terminated with {status:?}.");
                            }
                            exited_services.push((name.clone(), status));
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

                for (name, exit_status) in exited_services {
                    let manually_stopped = pid_file_guard.get(&name).is_none();
                    let exit_success = exit_status.success();
                    let exit_code = exit_status.code();
                    #[cfg(unix)]
                    let signal = exit_status.signal();
                    #[cfg(not(unix))]
                    let signal = None;
                    let hook_outcome = if manually_stopped || exit_success {
                        HookOutcome::Success
                    } else {
                        HookOutcome::Error
                    };

                    if let Some(service) = config.services.get(&name) {
                        let env = service.env.clone();
                        if let Some(action) = service
                            .hooks
                            .as_ref()
                            .and_then(|cfg| cfg.action(HookStage::OnStop, hook_outcome))
                        {
                            run_hook(
                                action,
                                &env,
                                HookStage::OnStop,
                                hook_outcome,
                                &name,
                                &project_root,
                            );
                        }
                    }

                    if manually_stopped {
                        info!("Service '{name}' was manually stopped. Skipping restart.");
                        if let Ok(mut state_guard) = state_file.lock()
                            && let Err(err) = state_guard.set(
                                &name,
                                ServiceLifecycleStatus::Stopped,
                                None,
                                None,
                                None,
                            )
                        {
                            warn!(
                                "Failed to persist stopped state for '{name}' after manual stop: {err}"
                            );
                        }
                    } else if !exit_success {
                        warn!("Service '{name}' crashed. Restarting...");
                        failed_services.push(name.clone());
                        restarted_services.push(name.clone());
                        if let Ok(mut state_guard) = state_file.lock()
                            && let Err(err) = state_guard.set(
                                &name,
                                ServiceLifecycleStatus::ExitedWithError,
                                None,
                                exit_code,
                                signal,
                            )
                        {
                            warn!("Failed to persist crash state for '{name}': {err}");
                        }
                    } else {
                        debug!(
                            "Service '{name}' exited cleanly. Removing from PID file."
                        );
                        if let Err(e) = pid_file_guard.remove(&name) {
                            error!("Failed to remove '{name}' from PID file: {e}");
                        }
                        if let Ok(mut state_guard) = state_file.lock()
                            && let Err(err) = state_guard.set(
                                &name,
                                ServiceLifecycleStatus::ExitedSuccessfully,
                                None,
                                exit_code.or(Some(0)),
                                signal,
                            )
                        {
                            warn!(
                                "Failed to persist clean exit state for '{name}': {err}"
                            );
                        }
                    }

                    processes.lock().unwrap().remove(&name);
                }
            }

            if !failed_services.is_empty() {
                let reverse = config.reverse_dependencies();
                for failed in failed_services {
                    Self::stop_dependents(
                        &failed,
                        &reverse,
                        &processes,
                        &pid_file,
                        &state_file,
                    );
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
                        Arc::clone(&restart_counts),
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
        restart_counts: Arc<Mutex<HashMap<String, u32>>>,
    ) {
        let name = name.to_string();
        let command = service.command.clone();
        let env = service.env.clone();
        let hooks = service.hooks.clone();
        let max_restarts = service.max_restarts;

        // Check restart count before attempting restart
        {
            let mut counts = restart_counts.lock().unwrap();
            let count = counts.entry(name.clone()).or_insert(0);
            *count += 1;

            if let Some(max) = max_restarts
                && *count > max
            {
                error!(
                    "Service '{name}' has reached maximum restart attempts ({max}). Giving up."
                );
                return;
            }
        }

        let backoff = service
            .backoff
            .as_deref()
            .unwrap_or("5s")
            .trim_end_matches('s')
            .parse::<u64>()
            .unwrap_or(5);

        let restart_counts_clone = Arc::clone(&restart_counts);

        let _ = thread::spawn(move || {
            warn!("Restarting '{name}' after {backoff} seconds...");
            thread::sleep(Duration::from_secs(backoff));

            let restart_result = Daemon::launch_attached_service(
                &name,
                &command,
                env.clone(),
                project_root.clone(),
                Arc::clone(&processes),
                detach_children,
            );

            match restart_result {
                Ok(pid) => {
                    let record_result = pid_file
                        .lock()
                        .map_err(ProcessManagerError::from)
                        .and_then(|mut guard| guard.insert(&name, pid).map_err(ProcessManagerError::from));

                    if let Err(err) = record_result {
                        error!(
                            "Failed to record PID {pid} for restarted service '{name}': {err}"
                        );

                        if let Err(stop_err) = Self::terminate_pid(pid, &name) {
                            warn!(
                                "Also failed to terminate untracked restart of '{name}': {stop_err}"
                            );
                        }

                        if let Some(hooks_cfg) = hooks.as_ref()
                            && let Some(action) =
                                hooks_cfg.action(HookStage::OnStart, HookOutcome::Error)
                        {
                            run_hook(
                                action,
                                &env,
                                HookStage::OnStart,
                                HookOutcome::Error,
                                &name,
                                &project_root,
                            );
                        }

                        if let Some(hooks_cfg) = hooks.as_ref()
                            && let Some(action) =
                                hooks_cfg.action(HookStage::OnRestart, HookOutcome::Error)
                        {
                            run_hook(
                                action,
                                &env,
                                HookStage::OnRestart,
                                HookOutcome::Error,
                                &name,
                                &project_root,
                            );
                        }

                        return;
                    }

                    match Self::wait_for_service_ready_with_handles(
                        &name,
                        &processes,
                        &pid_file,
                    ) {
                        Ok(_) => {
                            // Reset restart counter after successful restart
                            if let Ok(mut counts) = restart_counts_clone.lock() {
                                counts.insert(name.clone(), 0);
                            }

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnStart, HookOutcome::Success)
                            {
                                run_hook(
                                    action,
                                    &env,
                                    HookStage::OnStart,
                                    HookOutcome::Success,
                                    &name,
                                    &project_root,
                                );
                            }

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnRestart, HookOutcome::Success)
                            {
                                run_hook(
                                    action,
                                    &env,
                                    HookStage::OnRestart,
                                    HookOutcome::Success,
                                    &name,
                                    &project_root,
                                );
                            }

                            if let Ok(mut pid_file_guard) = pid_file.lock()
                                && let Ok(latest) = PidFile::reload()
                            {
                                *pid_file_guard = latest;
                            }
                        }
                        Err(err) => {
                            error!(
                                "Service '{name}' failed to become ready after restart: {err}"
                            );

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnStart, HookOutcome::Error)
                            {
                                run_hook(
                                    action,
                                    &env,
                                    HookStage::OnStart,
                                    HookOutcome::Error,
                                    &name,
                                    &project_root,
                                );
                            }

                            if let Some(hooks_cfg) = hooks.as_ref()
                                && let Some(action) =
                                    hooks_cfg.action(HookStage::OnRestart, HookOutcome::Error)
                            {
                                run_hook(
                                    action,
                                    &env,
                                    HookStage::OnRestart,
                                    HookOutcome::Error,
                                    &name,
                                    &project_root,
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to restart '{name}': {e}");

                    if let Some(hooks_cfg) = hooks.as_ref()
                        && let Some(action) =
                            hooks_cfg.action(HookStage::OnStart, HookOutcome::Error)
                    {
                        run_hook(
                            action,
                            &env,
                            HookStage::OnStart,
                            HookOutcome::Error,
                            &name,
                            &project_root,
                        );
                    }

                    if let Some(hooks_cfg) = hooks.as_ref()
                        && let Some(action) =
                            hooks_cfg.action(HookStage::OnRestart, HookOutcome::Error)
                    {
                        run_hook(
                            action,
                            &env,
                            HookStage::OnRestart,
                            HookOutcome::Error,
                            &name,
                            &project_root,
                        );
                    }
                }
            }
        })
        .join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, env, fs, sync::Mutex, thread, time::Duration};
    use tempfile::tempdir_in;

    /// Helper to build a minimal service definition for unit tests.
    fn make_service(command: &str, deps: &[&str]) -> ServiceConfig {
        ServiceConfig {
            command: command.to_string(),
            env: None,
            restart_policy: None,
            backoff: None,
            max_restarts: None,
            depends_on: if deps.is_empty() {
                None
            } else {
                Some(deps.iter().map(|d| d.to_string()).collect())
            },
            deployment: None,
            hooks: None,
            cron: None,
            skip: None,
        }
    }

    /// Constructs a daemon instance for tests with the provided service map rooted in `dir`.
    fn create_daemon(
        dir: &std::path::Path,
        services: HashMap<String, ServiceConfig>,
    ) -> Daemon {
        let pid_file = Arc::new(Mutex::new(PidFile::default()));
        let state_file = Arc::new(Mutex::new(ServiceStateFile::default()));
        let config = Config {
            version: "1".into(),
            services,
            project_dir: Some(dir.to_string_lossy().to_string()),
            env: None,
        };

        // Validate order to mirror load_config behaviour.
        config.service_start_order().unwrap();

        Daemon::new(config, pid_file, state_file, false)
    }

    /// Executes a test callback with a temporary HOME directory to contain PID and log files.
    fn with_temp_home<F: FnOnce(&std::path::Path)>(test: F) {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).expect("create tmp-home base");
        let temp = tempdir_in(base).expect("tempdir");
        let original = env::var("HOME").ok();
        unsafe {
            env::set_var("HOME", temp.path());
        }
        test(temp.path());
        match original {
            Some(val) => unsafe {
                env::set_var("HOME", val);
            },
            None => unsafe {
                env::remove_var("HOME");
            },
        }
    }

    #[test]
    fn logs_indicate_port_conflict_detects_common_errors() {
        with_temp_home(|_| {
            let log_path = resolve_log_path("web", "stderr");
            if let Some(dir) = log_path.parent() {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(
                &log_path,
                "Error: Server(\"Address already in use (os error 98)\")\n",
            )
            .unwrap();

            assert!(Daemon::logs_indicate_port_conflict("web"));
        });
    }

    #[test]
    fn logs_indicate_port_conflict_returns_false_when_not_present() {
        with_temp_home(|_| {
            let log_path = resolve_log_path("api", "stderr");
            if let Some(dir) = log_path.parent() {
                fs::create_dir_all(dir).unwrap();
            }
            fs::write(&log_path, "Some other failure\n").unwrap();

            assert!(!Daemon::logs_indicate_port_conflict("api"));
        });
    }

    #[test]
    fn restart_service_reports_failure_when_process_stops_immediately() {
        with_temp_home(|dir| {
            fs::write(dir.join("mode.txt"), "initial\n").unwrap();
            fs::write(
                dir.join("app.sh"),
                r#"
MODE=$(cat mode.txt)
if [ "$MODE" = "initial" ]; then
  sleep 5
else
  echo 'Error: Server("Address already in use (os error 98)")' >&2
  sleep 0.05
  exit 0
fi
"#,
            )
            .unwrap();

            let mut services = HashMap::new();
            let mut service = make_service("sh app.sh", &[]);
            service.restart_policy = Some("always".into());
            services.insert("app".into(), service);

            let daemon = create_daemon(dir, services);
            daemon.start_services_nonblocking().unwrap();

            // Ensure the initial instance is running.
            thread::sleep(Duration::from_millis(100));

            fs::write(dir.join("mode.txt"), "restart\n").unwrap();

            let svc = daemon.config.services.get("app").unwrap();
            let err = daemon.restart_service("app", svc).unwrap_err();

            match err {
                ProcessManagerError::ServicesNotRunning { services } => {
                    assert_eq!(services, vec!["app".to_string()]);
                }
                other => panic!("unexpected error: {other:?}"),
            }

            daemon.shutdown_monitor();
        });
    }

    #[test]
    fn monitor_reaps_services_that_exit_after_running_state() {
        with_temp_home(|dir| {
            fs::write(dir.join("slow_exit.sh"), "sleep 0.2\n").unwrap();

            let mut services = HashMap::new();
            let service = make_service("sh slow_exit.sh", &[]);
            services.insert("slow".into(), service);

            let daemon = create_daemon(dir, services);
            let svc = daemon.config.services.get("slow").unwrap();

            assert!(matches!(
                daemon.start_service("slow", svc).unwrap(),
                ServiceReadyState::Running
            ));

            daemon.ensure_monitoring().unwrap();
            thread::sleep(Duration::from_millis(2500));

            assert!(daemon.pid_file.lock().unwrap().get("slow").is_none());

            let state_guard = daemon.state_file.lock().unwrap();
            let entry = state_guard.services().get("slow").unwrap();
            assert_eq!(entry.status, ServiceLifecycleStatus::ExitedSuccessfully);

            daemon.shutdown_monitor();
        });
    }

    #[test]
    fn parse_duration_supports_common_units() {
        assert_eq!(
            Daemon::parse_duration("10s").unwrap(),
            Duration::from_secs(10)
        );
        assert_eq!(
            Daemon::parse_duration("5m").unwrap(),
            Duration::from_secs(300)
        );
        assert_eq!(
            Daemon::parse_duration("2h").unwrap(),
            Duration::from_secs(7200)
        );
        assert_eq!(
            Daemon::parse_duration("15").unwrap(),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn parse_duration_rejects_invalid_strings() {
        assert!(matches!(
            Daemon::parse_duration(""),
            Err(ProcessManagerError::ConfigParseError(_))
        ));
        assert!(matches!(
            Daemon::parse_duration("abc"),
            Err(ProcessManagerError::ConfigParseError(_))
        ));
    }

    #[test]
    fn services_start_in_dependency_order() {
        with_temp_home(|dir| {
            fs::write(dir.join("db.sh"), "echo db >> order.log\n").unwrap();
            fs::write(dir.join("web.sh"), "echo web >> order.log\n").unwrap();
            fs::write(dir.join("worker.sh"), "echo worker >> order.log\n").unwrap();

            let mut services = HashMap::new();
            services.insert("db".into(), make_service("sh db.sh", &[]));
            services.insert("web".into(), make_service("sh web.sh", &["db"]));
            services.insert("worker".into(), make_service("sh worker.sh", &["web"]));

            let daemon = create_daemon(dir, services);
            daemon.start_services_nonblocking().unwrap();
            daemon.shutdown_monitor();

            let content = fs::read_to_string(dir.join("order.log")).unwrap();
            let lines: Vec<_> = content.lines().collect();
            assert_eq!(lines, vec!["db", "web", "worker"]);
        });
    }

    #[test]
    fn dependent_not_started_when_dependency_fails() {
        with_temp_home(|dir| {
            fs::write(dir.join("fail.sh"), "exit 1\n").unwrap();
            fs::write(dir.join("dependent.sh"), "echo dependent >> started.log\n")
                .unwrap();

            let mut services = HashMap::new();
            services.insert("fail".into(), make_service("sh fail.sh", &[]));
            services.insert(
                "dependent".into(),
                make_service("sh dependent.sh", &["fail"]),
            );

            let daemon = create_daemon(dir, services);
            let result = daemon.start_services_nonblocking();
            assert!(result.is_err());
            assert!(!dir.join("started.log").exists());
            daemon.shutdown_monitor();
        });
    }

    #[test]
    fn dependents_stopped_when_dependency_crashes() {
        with_temp_home(|dir| {
            fs::write(
                dir.join("parent.sh"),
                "echo parent >> events.log\nsleep 1\nexit 1\n",
            )
            .unwrap();
            fs::write(dir.join("child.sh"), "echo child >> events.log\nsleep 30\n")
                .unwrap();

            let mut services = HashMap::new();
            services.insert("parent".into(), make_service("sh parent.sh", &[]));
            services.insert("child".into(), make_service("sh child.sh", &["parent"]));

            let daemon = create_daemon(dir, services);
            daemon.start_services_nonblocking().unwrap();

            thread::sleep(Duration::from_secs(4));

            let child_pid = daemon.pid_file.lock().unwrap().get("child");
            assert!(child_pid.is_none());

            daemon.shutdown_monitor();
        });
    }
}
