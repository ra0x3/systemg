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
        lines: usize,
    ) -> Result<(), LogsManagerError> {
        debug!("Fetching logs for service: {service_name}");
        let pid_file = self.pid_file.lock().unwrap();
        let pid = pid_file
            .get(service_name)
            .ok_or(LogsManagerError::ServiceNotFound(service_name.to_string()))?;

        println!(
            "\n+-----------------------------+\n\
         |   {} ({})   |\n\
         +-----------------------------+\n",
            service_name, pid
        );

        let command = format!(
            "stdbuf -oL -eL tail -n {} -f /proc/{}/fd/1 /proc/{}/fd/2",
            lines, pid, pid
        );

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
            let _ = self.show_log(&service, lines);
        }

        Ok(())
    }
}
