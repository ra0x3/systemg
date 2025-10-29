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

#[cfg(not(target_os = "linux"))]
use colored::*;

/// Returns the path to the log file for a given service and kind (stdout or stderr).
pub fn get_log_path(service: &str, kind: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(format!(
        "{}/.local/share/systemg/logs/{}_{}.log",
        home, service, kind
    ))
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
        use std::path::Path;

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

        if !Path::new(&stdout).exists() || !Path::new(&stderr).exists() {
            return Err(LogsManagerError::LogUnavailable(pid));
        }

        let stdout_path = get_log_path(service_name, "stdout");
        let stderr_path = get_log_path(service_name, "stderr");

        let command = format!(
            "tail -n {} -f {} {}",
            lines,
            stdout_path.display(),
            stderr_path.display()
        );
        debug!("Executing command: {command}");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

        let child = cmd.spawn()?.wait();

        if let Err(e) = child {
            return Err(LogsManagerError::LogProcessError(e));
        }

        Ok(())
    }

    /// Fallback implementation for macOS and other platforms where log tailing is not supported.
    #[cfg(target_os = "macos")]
    fn show_logs_platform(
        &self,
        service_name: &str,
        pid: u32,
        _lines: usize,
    ) -> Result<(), LogsManagerError> {
        println!(
            "\n+{:-^33}+\n\
     | {:^31} |\n\
     +{:-^33}+\n",
            "-",
            format!("{} ({})", service_name, pid),
            "-"
        );

        let github_issue_url = "https://github.com/ra0x3/systemg/issues/new";
        println!(
                "\n{}\n{}\n{}\n{}\n{}\n{}\n\n",
                "⚠️  WARNING: Log Tailing Not Supported on macOS".bold().yellow(),
                "--------------------------------------------".yellow(),
                "MacOS does not provide a straightforward way to tail stdout/stderr of an existing process.".yellow(),
                "If you believe this should be supported, please open an issue at:".yellow(),
                github_issue_url.blue().underline(),
                "Thank you for helping improve systemg!".yellow()
            );

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
            let _ = self.show_log(&service, pid, lines);
        }

        Ok(())
    }
}
