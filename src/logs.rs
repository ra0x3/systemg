//! Module for managing and displaying logs of system services.
//!
//! This module treats stderr as the primary log stream. Service output to stderr is logged
//! at debug level while stdout is logged at warn level to ensure stderr messages have priority
//! in the supervisor's log output.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;
use std::{
    collections::BTreeSet,
    env,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use tracing::debug;

use crate::{cron::CronStateFile, daemon::PidFile, error::LogsManagerError, runtime};

/// Returns the path to the log file for a given service and kind (stdout or stderr).
pub fn get_log_path(service: &str, kind: &str) -> PathBuf {
    resolve_log_path(service, kind)
}

/// Returns the canonical path for a service log without performing any existence checks.
fn canonical_log_path(service: &str, kind: &str) -> PathBuf {
    let mut path = runtime::log_dir();
    path.push(format!("{service}_{kind}.log"));
    path
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn locate_existing_log(service: &str, kind: &str) -> Option<PathBuf> {
    let canonical = canonical_log_path(service, kind);
    let directory = canonical.parent()?;
    let needle = normalize(service);
    let suffix = format!("_{kind}.log");

    let entries = fs::read_dir(directory).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let file_name = path.file_name()?.to_str()?;
        if !file_name.ends_with(&suffix) {
            continue;
        }

        if let Some(service_name) = file_name.strip_suffix(&suffix)
            && normalize(service_name) == needle
        {
            return Some(path);
        }
    }

    None
}

/// Attempts to resolve an on-disk log path for the given service and kind, falling back to the
/// canonical location when no existing file can be found.
pub fn resolve_log_path(service: &str, kind: &str) -> PathBuf {
    let canonical = canonical_log_path(service, kind);
    if canonical.exists() {
        return canonical;
    }

    locate_existing_log(service, kind).unwrap_or(canonical)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TailMode {
    Follow,
    OneShot,
}

impl TailMode {
    fn current() -> Self {
        match env::var("SYSTEMG_TAIL_MODE") {
            Ok(value) if value.eq_ignore_ascii_case("oneshot") => TailMode::OneShot,
            _ => TailMode::Follow,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn configure_command(
        self,
        cmd: &mut Command,
        lines: usize,
        stdout_path: &Path,
        stderr_path: &Path,
        kind: Option<&str>,
    ) {
        cmd.arg("-n").arg(lines.to_string());
        if matches!(self, TailMode::Follow) {
            cmd.arg("-F");
        }

        match kind {
            Some("stdout") => {
                cmd.arg(stdout_path);
            }
            Some("stderr") => {
                cmd.arg(stderr_path);
            }
            _ => {
                cmd.arg(stdout_path).arg(stderr_path);
            }
        }
    }
}

fn touch_log_file(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = OpenOptions::new().create(true).append(true).open(path);
}

#[cfg(target_os = "linux")]
fn process_fds_present(pid: u32) -> bool {
    let stdout_fd_path = format!("/proc/{pid}/fd/1");
    let stderr_fd_path = format!("/proc/{pid}/fd/2");
    let stdout_fd = Path::new(&stdout_fd_path);
    let stderr_fd = Path::new(&stderr_fd_path);
    stdout_fd.exists() || stderr_fd.exists()
}

fn resolve_tail_targets(
    service_name: &str,
    pid: Option<u32>,
) -> Result<(PathBuf, PathBuf), LogsManagerError> {
    let stdout_path = resolve_log_path(service_name, "stdout");
    let stderr_path = resolve_log_path(service_name, "stderr");

    let stdout_exists = stdout_path.exists();
    let stderr_exists = stderr_path.exists();

    if !stdout_exists {
        touch_log_file(&stdout_path);
    }
    if !stderr_exists {
        touch_log_file(&stderr_path);
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(pid_value) = pid
            && !(stdout_exists || stderr_exists || process_fds_present(pid_value))
        {
            return Err(LogsManagerError::LogUnavailable(pid_value));
        }
    }

    #[cfg(not(target_os = "linux"))]
    let _ = pid;

    Ok((stdout_path, stderr_path))
}

/// Creates the log directory if it doesn't exist and spawns a thread to write logs to file.
pub fn spawn_log_writer(service: &str, reader: impl Read + Send + 'static, kind: &str) {
    let path = get_log_path(service, kind);
    let service_label = service.to_string();
    thread::spawn(move || {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        let file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => f,
            Err(err) => {
                eprintln!("Warning: Unable to open log file at {:?}: {}", path, err);
                return;
            }
        };
        let mut file = file;

        let reader = BufReader::new(reader);
        for line in reader.lines().map_while(Result::ok) {
            debug!("[{service_label}] {line}");
            let _ = writeln!(file, "{line}");
        }
    });
}

/// Spawns a thread to capture and log output from dynamically spawned child processes.
///
/// # Arguments
///
/// * `root_service` - Optional parent service name for log organization
/// * `child_name` - Name of the child process being logged
/// * `pid` - Process ID of the child
/// * `reader` - Reader for the child's output stream
/// * `kind` - Type of stream (e.g., "stdout" or "stderr")
/// * `echo_to_console` - Whether to echo output to console in addition to file
pub fn spawn_dynamic_child_log_writer(
    root_service: Option<&str>,
    child_name: &str,
    pid: u32,
    reader: impl Read + Send + 'static,
    kind: &str,
    echo_to_console: bool,
) {
    let owner_component = root_service
        .map(normalize)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dynamic".to_string());
    let child_component = normalize(child_name);
    let child_component = if child_component.is_empty() {
        "child".to_string()
    } else {
        child_component
    };

    let mut path = runtime::log_dir();
    path.push("spawn");
    let file_name = format!(
        "{}_{}_{}_{}.log",
        owner_component, child_component, pid, kind
    );
    path.push(file_name);

    let owner_label = root_service.map(str::to_string);
    let child_label = child_name.to_string();

    thread::spawn(move || {
        if let Some(parent) = path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            eprintln!(
                "Warning: Unable to create spawn log directory {:?}: {}",
                parent, err
            );
            return;
        }

        let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => f,
            Err(err) => {
                eprintln!("Warning: Unable to open spawn log file {:?}: {}", path, err);
                return;
            }
        };

        let reader = BufReader::new(reader);
        for line in reader.lines().map_while(Result::ok) {
            if echo_to_console {
                let owner = owner_label.as_deref().unwrap_or("spawn");
                println!("[{}:{}] {}", owner, child_label, line);
            }

            if let Err(err) = writeln!(file, "{line}") {
                eprintln!("Warning: Failed to write spawn log {:?}: {}", path, err);
                break;
            }
        }
    });
}

/// Initializes logging for a service by spawning threads to write stdout and stderr to log files.
pub struct LogManager {
    /// The PID file containing service names and their respective PIDs.
    pid_file: Arc<Mutex<PidFile>>,
}

impl LogManager {
    /// Creates a new `LogManager` instance.
    pub fn new(pid_file: Arc<Mutex<PidFile>>) -> Self {
        Self { pid_file }
    }

    /// Shows the logs for a specific service's stdout/stderr in real-time.
    pub fn show_log(
        &self,
        service_name: &str,
        pid: u32,
        lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            Some(pid),
            lines,
            kind,
            TailMode::current(),
        )
    }

    /// Shows a one-shot snapshot of logs for a specific service.
    pub fn show_log_snapshot(
        &self,
        service_name: &str,
        pid: u32,
        lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            Some(pid),
            lines,
            kind,
            TailMode::OneShot,
        )
    }

    /// Shows logs for a service that is not currently running.
    pub fn show_inactive_log(
        &self,
        service_name: &str,
        lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            None,
            lines,
            kind,
            TailMode::current(),
        )
    }

    /// Shows a one-shot snapshot of logs for a service that is not currently running.
    pub fn show_inactive_log_snapshot(
        &self,
        service_name: &str,
        lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform_with_mode(
            service_name,
            None,
            lines,
            kind,
            TailMode::OneShot,
        )
    }

    /// Platform-specific implementation for showing logs.
    #[cfg(target_os = "linux")]
    fn show_logs_platform_with_mode(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        mode: TailMode,
    ) -> Result<(), LogsManagerError> {
        println!(
            "\n+{:-^33}+\n\
     | {:^31} |\n\
     +{:-^33}+\n",
            "-",
            Self::format_log_title(service_name, pid),
            "-"
        );

        let (stdout_path, stderr_path) = resolve_tail_targets(service_name, pid)?;

        debug!("Streaming logs via tail for '{}'", service_name);

        let mut cmd = Command::new("tail");
        #[cfg(target_os = "linux")]
        {
            if let Some(pid_value) = pid
                && !process_fds_present(pid_value)
            {
                debug!(
                    "Falling back to log files for '{}' because /proc/{pid_value} fds are unavailable",
                    service_name
                );
            }
        }
        mode.configure_command(&mut cmd, lines, &stdout_path, &stderr_path, kind);
        cmd.stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());

        let status = cmd.status()?;
        if !status.success() {
            return Err(LogsManagerError::TailCommandFailed(status.code()));
        }

        Ok(())
    }

    /// macOS implementation for showing logs using log files.
    #[cfg(target_os = "macos")]
    fn show_logs_platform_with_mode(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
        mode: TailMode,
    ) -> Result<(), LogsManagerError> {
        println!(
            "\n+{:-^33}+\n\
     | {:^31} |\n\
     +{:-^33}+\n",
            "-",
            Self::format_log_title(service_name, pid),
            "-"
        );

        let (stdout_path, stderr_path) = resolve_tail_targets(service_name, pid)?;

        debug!("Streaming logs via tail for '{}'", service_name);

        let mut cmd = Command::new("tail");
        mode.configure_command(&mut cmd, lines, &stdout_path, &stderr_path, kind);
        cmd.stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());

        let status = cmd.status()?;
        if !status.success() {
            return Err(LogsManagerError::TailCommandFailed(status.code()));
        }

        Ok(())
    }

    /// Streams logs for all active services in real-time.
    pub fn show_logs(
        &self,
        lines: usize,
        kind: Option<&str>,
        config_path: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_with_mode(lines, kind, config_path, TailMode::current())
    }

    /// Streams one-shot snapshots for all active services.
    pub fn show_logs_snapshot(
        &self,
        lines: usize,
        kind: Option<&str>,
        config_path: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_with_mode(lines, kind, config_path, TailMode::OneShot)
    }

    fn show_logs_with_mode(
        &self,
        lines: usize,
        kind: Option<&str>,
        config_path: Option<&str>,
        mode: TailMode,
    ) -> Result<(), LogsManagerError> {
        debug!("Fetching logs for all services...");

        println!(
            "\n\
            ╭{}╮\n\
            │ ⚠️  Showing latest logs per service (stdout & stderr)             │\n\
            │                                                                   │\n\
            │ For complete logs, run: sysg logs <service>                      │\n\
            ╰{}╯\n",
            "─".repeat(67),
            "─".repeat(67)
        );

        if matches!(kind, None | Some("supervisor")) {
            let _ = self.show_supervisor_log(lines).map_err(|err| {
                eprintln!("Failed to show supervisor logs: {}", err);
            });

            if kind == Some("supervisor") {
                return Ok(());
            }
        }

        let pid_snapshot = {
            let guard = self.pid_file.lock().unwrap();
            guard.services().clone()
        };

        let cron_state =
            CronStateFile::load().unwrap_or_else(|_| CronStateFile::default());

        let hash_to_name: std::collections::HashMap<String, String> =
            crate::config::load_config(config_path)
                .ok()
                .map(|config| {
                    config
                        .services
                        .iter()
                        .map(|(name, svc_config)| {
                            (svc_config.compute_hash(), name.clone())
                        })
                        .collect()
                })
                .unwrap_or_default();

        let mut service_names: BTreeSet<String> = pid_snapshot.keys().cloned().collect();

        for hash in cron_state.jobs().keys() {
            if let Some(name) = hash_to_name.get(hash) {
                service_names.insert(name.clone());
            } else {
                service_names.insert(hash.clone());
            }
        }

        debug!("Services: {service_names:?}");

        if service_names.is_empty() {
            if kind.is_some() {
                println!("No active services");
            }
            return Ok(());
        }

        for service_name in service_names {
            if let Some(pid) = pid_snapshot.get(&service_name) {
                debug!("Service: {service_name}, PID: {pid}");
                let result = if matches!(mode, TailMode::OneShot) {
                    self.show_log_snapshot(&service_name, *pid, lines, kind)
                } else {
                    self.show_log(&service_name, *pid, lines, kind)
                };
                if let Err(err) = result {
                    eprintln!("Failed to stream logs for '{}': {}", service_name, err);
                }
                continue;
            }

            if let Ok(config) = crate::config::load_config(config_path)
                && let Some(service_config) = config.services.get(&service_name)
            {
                let service_hash = service_config.compute_hash();
                if let Some(_cron_job) = cron_state.jobs().get(&service_hash) {
                    debug!("Showing inactive logs for cron service '{}'", service_name);
                    let result = if matches!(mode, TailMode::OneShot) {
                        self.show_inactive_log_snapshot(&service_name, lines, kind)
                    } else {
                        self.show_inactive_log(&service_name, lines, kind)
                    };
                    if let Err(err) = result {
                        eprintln!(
                            "Failed to stream logs for '{}': {}",
                            service_name, err
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn format_log_title(service_name: &str, pid: Option<u32>) -> String {
        match pid {
            Some(pid) => format!("{service_name} ({pid})"),
            None => format!("{service_name} (offline)"),
        }
    }

    /// Shows the supervisor logs
    fn show_supervisor_log(&self, lines: usize) -> Result<(), LogsManagerError> {
        let supervisor_log = runtime::log_dir().join("supervisor.log");

        if !supervisor_log.exists() {
            return Ok(());
        }

        println!(
            "\n+{:-^33}+\n\
             | {:^31} |\n\
             +{:-^33}+\n",
            "-", "Supervisor", "-"
        );

        let mut cmd = Command::new("tail");
        cmd.arg("-n").arg(lines.to_string());
        cmd.arg(&supervisor_log);
        cmd.stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());

        let status = cmd.status()?;
        if !status.success() {
            return Err(LogsManagerError::TailCommandFailed(status.code()));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::Cursor,
        path::Path,
        thread,
        time::Duration,
    };

    use tempfile::tempdir_in;

    use super::*;

    #[test]
    fn resolve_log_path_matches_hyphenated_files() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let log_dir = canonical_log_path("dummy", "stdout")
            .parent()
            .map(Path::to_path_buf)
            .unwrap();
        fs::create_dir_all(&log_dir).unwrap();

        let target = log_dir.join("arb-rs_stdout.log");
        File::create(&target).unwrap();

        let resolved = resolve_log_path("arb_rs", "stdout");
        assert_eq!(resolved, target);

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn spawn_dynamic_child_log_writer_persists_output() {
        let _guard = crate::test_utils::env_lock();

        let base = std::env::current_dir()
            .expect("current_dir")
            .join("target/tmp-home");
        fs::create_dir_all(&base).unwrap();
        let temp = tempdir_in(&base).unwrap();
        let home = temp.path();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let reader = Cursor::new(b"hello\nworld\n".to_vec());
        super::spawn_dynamic_child_log_writer(
            Some("alpha"),
            "beta",
            123,
            reader,
            "stdout",
            false,
        );

        thread::sleep(Duration::from_millis(100));

        let log_path = crate::runtime::log_dir()
            .join("spawn")
            .join("alpha_beta_123_stdout.log");
        let contents =
            fs::read_to_string(&log_path).expect("spawn log should be written");
        assert!(contents.contains("hello"));
        assert!(contents.contains("world"));

        unsafe {
            if let Some(home) = original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }
}
