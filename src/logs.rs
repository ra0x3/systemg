//! Module for managing and displaying logs of system services.
use crate::{cron::CronStateFile, daemon::PidFile, error::LogsManagerError};
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;

/// Returns the path to the log file for a given service and kind (stdout or stderr).
pub fn get_log_path(service: &str, kind: &str) -> PathBuf {
    resolve_log_path(service, kind)
}

/// Returns the canonical path for a service log without performing any existence checks.
fn canonical_log_path(service: &str, kind: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let mut path = PathBuf::from(home);
    path.push(".local/share/systemg/logs");
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
                // Default: show stdout first, then stderr
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

/// Creates the log directory if it doesn't exist.
pub fn spawn_log_writer(service: &str, reader: impl Read + Send + 'static, kind: &str) {
    let service = service.to_string();
    let kind = kind.to_string();
    thread::spawn(move || {
        let path = get_log_path(&service, &kind);
        fs::create_dir_all(path.parent().unwrap()).ok();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("Unable to open log file");

        let reader = BufReader::new(reader);
        for line in reader.lines().map_while(Result::ok) {
            let _ = writeln!(file, "{line}");
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
        self.show_logs_platform(service_name, Some(pid), lines, kind)
    }

    /// Shows logs for a service that is not currently running.
    pub fn show_inactive_log(
        &self,
        service_name: &str,
        lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform(service_name, None, lines, kind)
    }

    /// Platform-specific implementation for showing logs.
    #[cfg(target_os = "linux")]
    fn show_logs_platform(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
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
        let mode = TailMode::current();
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
    fn show_logs_platform(
        &self,
        service_name: &str,
        pid: Option<u32>,
        lines: usize,
        kind: Option<&str>,
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
        TailMode::current().configure_command(
            &mut cmd,
            lines,
            &stdout_path,
            &stderr_path,
            kind,
        );
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
        _lines: usize,
        kind: Option<&str>,
    ) -> Result<(), LogsManagerError> {
        debug!("Fetching logs for all services...");

        // Display alert message for users
        println!(
            "\n\
            ╭{}╮\n\
            │ ⚠️  Showing first 20 logs per service (stdout & stderr)           │\n\
            │                                                                   │\n\
            │ For complete logs, run: sysg logs <service>                      │\n\
            ╰{}╯\n",
            "─".repeat(67),
            "─".repeat(67)
        );

        // Show supervisor logs first if no service specified and kind is not specified or is "supervisor"
        if matches!(kind, None | Some("supervisor")) {
            let _ = self.show_supervisor_log(20).map_err(|err| {
                eprintln!("Failed to show supervisor logs: {}", err);
            });

            // If kind is "supervisor", only show supervisor logs
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

        let mut services: BTreeSet<String> = pid_snapshot.keys().cloned().collect();
        services.extend(cron_state.jobs().keys().cloned());

        debug!("Services: {services:?}");

        if services.is_empty() {
            if kind.is_some() {
                println!("No active services");
            }
            return Ok(());
        }

        for service in services {
            if let Some(pid) = pid_snapshot.get(&service) {
                debug!("Service: {service}, PID: {pid}");
                if let Err(err) = self.show_log(&service, *pid, 20, kind) {
                    eprintln!("Failed to stream logs for '{}': {}", service, err);
                }
                continue;
            }

            if let Some(cron_job) = cron_state.jobs().get(&service) {
                debug!(
                    "Showing inactive logs for cron service '{}' with {} history entries",
                    service,
                    cron_job.execution_history.len()
                );
                if let Err(err) = self.show_inactive_log(&service, 20, kind) {
                    eprintln!("Failed to stream logs for '{}': {}", service, err);
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
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let supervisor_log =
            PathBuf::from(format!("{}/.local/share/systemg/supervisor.log", home));

        if !supervisor_log.exists() {
            return Ok(()); // Silently skip if supervisor log doesn't exist yet
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
    use super::*;
    use std::fs::{self, File};
    use std::path::Path;
    use tempfile::tempdir_in;

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
    }
}
