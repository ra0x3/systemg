use crate::daemon::PidFile;
use crate::error::LogsManagerError;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
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
        debug!("Showing log for service: {service_name}");

        println!(
            "\n+-----------------------------+\n\
         |   {} ({})   |\n\
         +-----------------------------+\n",
            service_name, pid
        );

        // macOS: Find the TTY of the process and tail it
        let command = if cfg!(target_os = "macos") {
            format!(
                r#"tty=$(ps -o tty= -p {pid} | tr -d ' '); \
            [ -n "$tty" ] && tail -n {lines} -f /dev/"$tty""#
            )
        } else {
            // Linux: Tail the process's stdout/stderr
            format!("tail -n {} -f /proc/{}/fd/1 /proc/{}/fd/2", lines, pid, pid)
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
