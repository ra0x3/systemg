use crate::{daemon::PidFile, error::LogsManagerError};
use colored::*;
use std::{
    fs,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};
use tracing::debug;

pub struct LogManager {
    /// The PID file containing service names and their respective PIDs.
    pid_file: Arc<Mutex<PidFile>>,
}

impl LogManager {
    /// Creates a new `LogManager` instance.
    pub fn new(pid_file: Arc<Mutex<PidFile>>) -> Self {
        Self { pid_file }
    }

    /// Streams logs for a specific service's stdout/stderr in real-time.
    pub fn show_log(
        &self,
        service_name: &str,
        pid: u32,
        lines: usize,
    ) -> Result<(), LogsManagerError> {
        debug!("Showing log for service: {service_name} (PID: {pid})");

        println!(
            "\n+{:-^33}+\n\
     | {:^31} |\n\
     +{:-^33}+\n",
            "-",                                   // Top border
            format!("{} ({})", service_name, pid), // Centered service name and PID
            "-"                                    // Bottom border
        );

        let command = if cfg!(target_os = "macos") {
            // macOS doesn't provide easy access to another process's stdout/stderr
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
            return Ok(());
        } else {
            // On Linux, check if the process has a TTY
            let tty_path = format!("/proc/{}/fd/0", pid);
            if let Ok(tty_target) = fs::read_link(&tty_path) {
                if tty_target.to_string_lossy().starts_with("/dev/pts/") {
                    format!("tail -n {} -f {}", lines, tty_target.to_string_lossy())
                } else {
                    // Fallback: Tail stdout and stderr directly
                    format!("tail -n {} -f /proc/{}/fd/1 /proc/{}/fd/2", lines, pid, pid)
                }
            } else {
                // If no TTY found, just tail stdout/stderr
                format!("tail -n {} -f /proc/{}/fd/1 /proc/{}/fd/2", lines, pid, pid)
            }
        };

        let handle = thread::spawn(move || {
            debug!("Executing command: {command}");
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(command);
            cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

            let _ = cmd.spawn().map(|mut child| child.wait());
        });

        handle.join().expect("Failed to join thread");

        Ok(())
    }

    /// Streams logs for all active services in real-time.
    pub fn show_logs(&self, lines: usize) -> Result<(), LogsManagerError> {
        debug!("Fetching logs for all services...");
        let pid_file = self.pid_file.lock().unwrap();
        let services: Vec<String> = pid_file.services().keys().cloned().collect();

        debug!("Services: {services:?}");

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
