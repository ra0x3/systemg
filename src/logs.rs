//! Module for managing and displaying logs of system services.
use crate::{daemon::PidFile, error::LogsManagerError};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};
use tracing::debug;

#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

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
    ) -> Result<(), LogsManagerError> {
        self.show_logs_platform(service_name, pid, lines)
    }

    /// Platform-specific implementation for showing logs.
    #[cfg(target_os = "linux")]
    fn show_logs_platform(
        &self,
        service_name: &str,
        pid: u32,
        lines: usize,
    ) -> Result<(), LogsManagerError> {
        println!(
            "\n+{:-^33}+\n\
     | {:^31} |\n\
     +{:-^33}+\n",
            "-",
            format!("{} ({})", service_name, pid),
            "-"
        );

        let stdout = format!("/proc/{}/fd/1", pid);
        let stderr = format!("/proc/{}/fd/2", pid);

        if !std::path::Path::new(&stdout).exists()
            || !std::path::Path::new(&stderr).exists()
        {
            return Err(LogsManagerError::LogUnavailable(pid));
        }

        let stdout_path = resolve_log_path(service_name, "stdout");
        let stderr_path = resolve_log_path(service_name, "stderr");

        debug!("Streaming logs via tail for '{}'", service_name);

        let mut cmd = Command::new("tail");
        cmd.arg("-n")
            .arg(lines.to_string())
            .arg("-F")
            .arg(&stdout_path)
            .arg(&stderr_path);
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

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
        pid: u32,
        lines: usize,
    ) -> Result<(), LogsManagerError> {
        use std::process::{Command, Stdio};

        println!(
            "\n+{:-^33}+\n\
     | {:^31} |\n\
     +{:-^33}+\n",
            "-",
            format!("{} ({})", service_name, pid),
            "-"
        );

        let stdout_path = resolve_log_path(service_name, "stdout");
        let stderr_path = resolve_log_path(service_name, "stderr");

        debug!("Streaming logs via tail for '{}'", service_name);

        let mut cmd = Command::new("tail");
        cmd.arg("-n")
            .arg(lines.to_string())
            .arg("-F")
            .arg(&stdout_path)
            .arg(&stderr_path);
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

        let status = cmd.status()?;
        if !status.success() {
            return Err(LogsManagerError::TailCommandFailed(status.code()));
        }

        Ok(())
    }

    /// Streams logs for all active services in real-time.
    pub fn show_logs(&self, lines: usize) -> Result<(), LogsManagerError> {
        debug!("Fetching logs for all services...");
        let pid_file = self.pid_file.lock().unwrap();
        let services: Vec<String> = pid_file.services().keys().cloned().collect();

        debug!("Services: {services:?}");

        if services.is_empty() {
            println!("No active services");
            return Ok(());
        }

        for service in services {
            let pid = pid_file
                .get(&service)
                .ok_or(LogsManagerError::ServiceNotFound(service.clone()))?;
            debug!("Service: {service}, PID: {pid}");
            if let Err(err) = self.show_log(&service, pid, lines) {
                eprintln!("Failed to stream logs for '{}': {}", service, err);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir_in;

    #[test]
    fn resolve_log_path_matches_hyphenated_files() {
        static HOME_GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = HOME_GUARD.get_or_init(|| Mutex::new(())).lock().unwrap();

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
