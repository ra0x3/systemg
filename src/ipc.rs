use serde::{Deserialize, Serialize};
use std::os::unix::ffi::OsStrExt;
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Directory under `$HOME` where runtime artifacts (PID/socket files) are stored.
fn runtime_dir() -> Result<PathBuf, ControlError> {
    let home = std::env::var("HOME").map_err(|_| ControlError::MissingHome)?;
    let path = PathBuf::from(home).join(".local/share/systemg");
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
    fs::write(path, pid.to_string())?;
    Ok(())
}

/// Persists the resolved config path to assist CLI fallbacks.
pub fn write_config_hint(config: &Path) -> Result<(), ControlError> {
    let hint_path = config_hint_path()?;
    if let Some(parent) = hint_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(hint_path, config.as_os_str().as_bytes())?;
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
