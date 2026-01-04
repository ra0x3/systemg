use crate::{
    metrics::MetricSample,
    runtime,
    status::{StatusSnapshot, UnitStatus},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Directory under `$HOME` where runtime artifacts (PID/socket files) are stored.
fn runtime_dir() -> Result<PathBuf, ControlError> {
    let path = runtime::state_dir();
    fs::create_dir_all(&path)?;
    Ok(path)
}

/// Returns the unix socket path used to communicate with the resident supervisor.
pub fn socket_path() -> Result<PathBuf, ControlError> {
    Ok(runtime_dir()?.join("control.sock"))
}

/// Returns the path where the supervisor PID is recorded.
pub fn supervisor_pid_path() -> Result<PathBuf, ControlError> {
    Ok(runtime_dir()?.join("sysg.pid"))
}

fn config_hint_path() -> Result<PathBuf, ControlError> {
    Ok(runtime_dir()?.join("config_hint"))
}

/// Message sent from CLI invocations to the resident supervisor.
#[derive(Debug, Serialize, Deserialize)]
pub enum ControlCommand {
    /// Start one or all services.
    Start {
        /// Optional service name to start. If None, starts all services.
        service: Option<String>,
    },
    /// Stop one or all services.
    Stop {
        /// Optional service name to stop. If None, stops all services.
        service: Option<String>,
    },
    /// Restart services, optionally with a new configuration.
    Restart {
        /// Optional path to a new configuration file.
        config: Option<String>,
        /// Optional service name to restart. If None, restarts all services.
        service: Option<String>,
    },
    /// Shutdown the supervisor daemon.
    Shutdown,
    /// Fetch the cached status snapshot from the supervisor.
    Status,
    /// Inspect an individual unit with metrics.
    Inspect {
        /// Name or hash of the unit to inspect.
        unit: String,
        /// Maximum number of samples to return.
        samples: u32,
    },
}

/// Response sent by the supervisor.
#[derive(Debug, Serialize, Deserialize)]
pub enum ControlResponse {
    /// Command completed successfully.
    Ok,
    /// Command completed with a status message.
    Message(String),
    /// Command failed with an error message.
    Error(String),
    /// Current status snapshot payload.
    Status(StatusSnapshot),
    /// Inspect payload including recent samples.
    Inspect(Box<InspectPayload>),
}

/// Inspect response payload.
#[derive(Debug, Serialize, Deserialize)]
pub struct InspectPayload {
    pub unit: Option<UnitStatus>,
    #[serde(default)]
    pub samples: Vec<MetricSample>,
}

/// Errors raised by the control channel helpers.
#[derive(Debug, Error)]
pub enum ControlError {
    /// Control socket I/O error.
    #[error("control socket I/O failed: {0}")]
    Io(#[from] io::Error),
    /// Error serializing or deserializing control messages.
    #[error("failed to serialise control message: {0}")]
    Serde(#[from] serde_json::Error),
    /// HOME environment variable not set.
    #[error("HOME environment variable not set")]
    MissingHome,
    /// Supervisor reported an error.
    #[error("supervisor reported error: {0}")]
    Server(String),
    /// Control socket not available or supervisor not running.
    #[error("control socket not available")]
    NotAvailable,
}

/// Sends a command to the supervisor and waits for a response.
pub fn send_command(command: &ControlCommand) -> Result<ControlResponse, ControlError> {
    let path = socket_path()?;
    if !path.exists() {
        return Err(ControlError::NotAvailable);
    }

    let mut stream = UnixStream::connect(path)?;
    let payload = serde_json::to_vec(command)?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line)?;

    if response_line.trim().is_empty() {
        return Err(ControlError::NotAvailable);
    }

    let response: ControlResponse = serde_json::from_str(response_line.trim())?;
    if let ControlResponse::Error(message) = &response {
        return Err(ControlError::Server(message.clone()));
    }

    Ok(response)
}

/// Utility to read a command from a `UnixStream`. Used by the supervisor event loop.
pub fn read_command(stream: &mut UnixStream) -> Result<ControlCommand, ControlError> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    if line.trim().is_empty() {
        return Err(ControlError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty control command",
        )));
    }

    Ok(serde_json::from_str(line.trim())?)
}

/// Writes a response to the connected CLI client.
pub fn write_response(
    stream: &mut UnixStream,
    response: &ControlResponse,
) -> Result<(), ControlError> {
    let payload = serde_json::to_vec(response)?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// Persists the supervisor PID for later CLI detection.
pub fn write_supervisor_pid(pid: libc::pid_t) -> Result<(), ControlError> {
    let path = supervisor_pid_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, pid.to_string())?;
    Ok(())
}

/// Persists the resolved config path to assist CLI fallbacks.
pub fn write_config_hint(config: &Path) -> Result<(), ControlError> {
    let hint_path = config_hint_path()?;
    if let Some(parent) = hint_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let config_str = config.to_string_lossy();
    fs::write(hint_path, config_str.as_bytes())?;
    Ok(())
}

/// Reads the supervisor PID if present.
pub fn read_supervisor_pid() -> Result<Option<libc::pid_t>, ControlError> {
    let path = supervisor_pid_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(path)?;
    contents
        .trim()
        .parse::<libc::pid_t>()
        .map(Some)
        .map_err(|e| ControlError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))
}

/// Reads the persisted config path hint if available.
pub fn read_config_hint() -> Result<Option<PathBuf>, ControlError> {
    let hint_path = config_hint_path()?;
    if !hint_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(hint_path)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    Ok(Some(PathBuf::from(trimmed)))
}

/// Clears the supervisor PID and removes the socket file.
pub fn cleanup_runtime() -> Result<(), ControlError> {
    if let Ok(path) = socket_path()
        && path.exists()
    {
        let _ = fs::remove_file(path);
    }

    if let Ok(pid_path) = supervisor_pid_path()
        && pid_path.exists()
    {
        let _ = fs::remove_file(pid_path);
    }

    if let Ok(config_path) = config_hint_path()
        && config_path.exists()
    {
        let _ = fs::remove_file(config_path);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use tempfile::tempdir;

    #[test]
    fn control_command_serialization() {
        // Test Start command
        let start = ControlCommand::Start {
            service: Some("test_service".to_string()),
        };
        let json = serde_json::to_string(&start).unwrap();
        assert!(json.contains("Start"));
        assert!(json.contains("test_service"));

        // Test Stop command
        let stop = ControlCommand::Stop { service: None };
        let json = serde_json::to_string(&stop).unwrap();
        assert!(json.contains("Stop"));

        // Test Restart command
        let restart = ControlCommand::Restart {
            config: Some("config.yaml".to_string()),
            service: Some("service".to_string()),
        };
        let json = serde_json::to_string(&restart).unwrap();
        assert!(json.contains("Restart"));
        assert!(json.contains("config.yaml"));

        // Test Shutdown command
        let shutdown = ControlCommand::Shutdown;
        let json = serde_json::to_string(&shutdown).unwrap();
        assert!(json.contains("Shutdown"));

        let inspect = ControlCommand::Inspect {
            unit: "svc".to_string(),
            samples: 10,
        };
        let json = serde_json::to_string(&inspect).unwrap();
        assert!(json.contains("Inspect"));
        assert!(json.contains("\"samples\":10"));
    }

    #[test]
    fn control_response_serialization() {
        let ok = ControlResponse::Ok;
        let json = serde_json::to_string(&ok).unwrap();
        assert!(json.contains("Ok"));

        let message = ControlResponse::Message("Service started".to_string());
        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("Message"));
        assert!(json.contains("Service started"));

        let error = ControlResponse::Error("Failed to stop".to_string());
        let json = serde_json::to_string(&error).unwrap();
        assert!(json.contains("Error"));
        assert!(json.contains("Failed to stop"));

        let inspect_payload = InspectPayload {
            unit: None,
            samples: Vec::new(),
        };
        let json =
            serde_json::to_string(&ControlResponse::Inspect(Box::new(inspect_payload)))
                .unwrap();
        assert!(json.contains("Inspect"));
    }

    #[test]
    fn write_and_read_supervisor_pid() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let pid = 12345;
        write_supervisor_pid(pid).unwrap();

        let read_pid = read_supervisor_pid().unwrap();
        assert_eq!(read_pid, Some(pid));

        // Cleanup
        cleanup_runtime().unwrap();
        let read_pid = read_supervisor_pid().unwrap();
        assert_eq!(read_pid, None);

        // Restore original HOME
        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn write_and_read_config_hint() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let config = PathBuf::from("/path/to/config.yaml");
        write_config_hint(&config).unwrap();

        let hint = read_config_hint().unwrap();
        assert_eq!(hint, Some(config));

        // Cleanup
        cleanup_runtime().unwrap();
        let hint = read_config_hint().unwrap();
        assert_eq!(hint, None);

        // Restore original HOME
        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn send_command_no_socket() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let command = ControlCommand::Shutdown;
        let result = send_command(&command);

        assert!(matches!(result, Err(ControlError::NotAvailable)));

        // Restore original HOME
        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn write_and_read_command_response() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("test.sock");

        // Create a Unix socket pair for testing
        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                return;
            }
            Err(err) => panic!("failed to bind test socket: {err}"),
        };

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();

            // Read command
            let cmd = read_command(&mut stream).unwrap();
            assert!(matches!(cmd, ControlCommand::Start { .. }));

            // Write response
            let response = ControlResponse::Message("Started".to_string());
            write_response(&mut stream, &response).unwrap();
        });

        // Give the thread time to start
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Connect and send command
        let mut stream = UnixStream::connect(&socket_path).unwrap();
        let command = ControlCommand::Start {
            service: Some("test".to_string()),
        };
        let payload = serde_json::to_vec(&command).unwrap();
        stream.write_all(&payload).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();

        // Read response
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let response: ControlResponse = serde_json::from_str(line.trim()).unwrap();

        assert!(matches!(response, ControlResponse::Message(msg) if msg == "Started"));
    }

    #[test]
    fn control_error_from_io_error() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let ctrl_err: ControlError = io_err.into();

        match ctrl_err {
            ControlError::Io(_) => {}
            _ => panic!("Expected Io error variant"),
        }
    }

    #[test]
    fn control_error_from_serde_error() {
        let json = "{invalid json}";
        let serde_err = serde_json::from_str::<ControlCommand>(json).unwrap_err();
        let ctrl_err: ControlError = serde_err.into();

        match ctrl_err {
            ControlError::Serde(_) => {}
            _ => panic!("Expected Serde error variant"),
        }
    }

    #[test]
    fn runtime_dir_creation() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let dir = runtime_dir().unwrap();
        assert!(dir.ends_with(".local/share/systemg"));
        assert!(dir.exists());

        // Restore original HOME
        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn socket_path_generation() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let path = socket_path().unwrap();
        assert!(path.ends_with("control.sock"));

        // Restore original HOME
        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
    }

    #[test]
    fn empty_config_hint_handled() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        // Write empty string
        let hint_path = config_hint_path().unwrap();
        fs::create_dir_all(hint_path.parent().unwrap()).unwrap();
        fs::write(&hint_path, "").unwrap();

        // Should return None for empty content
        let hint = read_config_hint().unwrap();
        assert_eq!(hint, None);

        // Restore original HOME
        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }
}
