use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    metrics::MetricSample,
    runtime,
    status::{ProjectRunMode, StatusSnapshot, UnitStatus},
};

/// Upper bound on how long a CLI command waits for the supervisor to reply
/// before giving up, so a wedged supervisor surfaces as an error instead of an
/// unbounded spinner.
const COMMAND_READ_TIMEOUT: Duration = Duration::from_secs(120);

/// Short bound for the diagnostic current-op probe, which must never itself hang.
const CURRENT_OP_TIMEOUT: Duration = Duration::from_secs(2);

/// Directory under `$HOME` where runtime artifacts (PID/socket files) are stored.
fn runtime_dir() -> Result<PathBuf, ControlError> {
    let path = runtime::state_dir();
    runtime::create_private_dir(&path)?;
    Ok(path)
}

/// Returns the unix socket path used to communicate with the resident supervisor.
pub fn socket_path() -> Result<PathBuf, ControlError> {
    Ok(runtime_dir()?.join("control.sock"))
}

/// Binds the control socket and restricts it to the owner (mode `0600` on Unix).
///
/// Removes any stale socket file first. The socket is the sole control channel,
/// so tightening its permissions prevents other local users from issuing
/// commands to the supervisor.
pub fn bind_control_socket() -> Result<std::os::unix::net::UnixListener, ControlError> {
    let path = socket_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }

    let listener = std::os::unix::net::UnixListener::bind(&path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            &path,
            fs::Permissions::from_mode(crate::constants::PRIVATE_FILE_MODE),
        )?;
    }

    Ok(listener)
}

/// Acquires exclusive ownership of the supervisor runtime for this process lifetime.
pub fn lock_supervisor_runtime() -> Result<fs::File, ControlError> {
    let path = runtime_dir()?.join("supervisor.lock");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
            return Err(ControlError::RuntimeBusy);
        }
        Err(err) => return Err(ControlError::Io(err)),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            &path,
            fs::Permissions::from_mode(crate::constants::PRIVATE_FILE_MODE),
        )?;
    }
    Ok(file)
}

/// Returns the path where the supervisor PID is recorded.
pub fn supervisor_pid_path() -> Result<PathBuf, ControlError> {
    Ok(runtime_dir()?.join("sysg.pid"))
}

/// Handles config hint path.
fn config_hint_path() -> Result<PathBuf, ControlError> {
    Ok(runtime_dir()?.join("config_hint"))
}

/// Message sent from CLI invocations to the resident supervisor.
#[derive(Debug, Serialize, Deserialize)]
pub enum ControlCommand {
    /// Start one or all services.
    Start {
        /// Optional service name to start. If None, starts all services.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        /// Optional project id to target.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    /// Add another project configuration to the resident supervisor.
    AddProject {
        /// Path to the project configuration file.
        config: String,
        /// Optional service name to start from the added project.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        /// Requested project run mode.
        #[serde(default)]
        mode: ProjectRunMode,
    },
    /// Stop all services for one project.
    StopProject {
        /// Stable project id to stop.
        project: String,
    },
    /// Stop one or all services.
    Stop {
        /// Optional service name to stop. If None, stops all services.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        /// Optional project id to target.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    /// Restart services, optionally with a new configuration.
    Restart {
        /// Optional path to a new configuration file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<String>,
        /// Optional service name to restart. If None, restarts all services.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        /// Optional project id to target.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    /// Shutdown the supervisor daemon.
    Shutdown,
    /// Fetch a status snapshot from the supervisor.
    Status {
        /// Whether to force live runtime collection instead of the configured snapshot mode.
        #[serde(default)]
        live: bool,
    },
    /// Inspect an individual unit with metrics.
    Inspect {
        /// Name or hash of the unit to inspect.
        unit: String,
        /// Optional project id containing the inspected unit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        /// Maximum number of samples to return.
        samples: u32,
        /// Whether to force live runtime collection instead of the configured snapshot mode.
        #[serde(default)]
        live: bool,
    },
    /// Stream logs for one or all services through the supervisor.
    Logs {
        /// Optional service name to stream. If None, streams all managed services.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        /// Optional project id to filter logs by.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        /// Number of lines to include initially.
        lines: usize,
        /// Log kind to stream. None means merged stdout+stderr.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Whether to follow the log stream until the client disconnects.
        follow: bool,
        /// Lower bound (RFC3339) on the systemg capture timestamp.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<String>,
        /// Upper bound (RFC3339) on the systemg capture timestamp.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<String>,
        /// Substring/regex pattern a line must match to be shown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grep: Option<String>,
        /// Read the full active-plus-rotated history instead of the tail.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        all: bool,
        /// Whether the client renders structured output (json/raw) and can
        /// consume per-service marker lines for attribution.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        structured: bool,
    },
    /// Clear captured logs for one or all services, inside the supervisor, so
    /// both the on-disk files and the supervisor's in-memory live-log buffer are
    /// dropped together (a CLI-side truncate leaves the reader serving stale
    /// buffered lines).
    ClearLogs {
        /// Optional service name to clear. None clears all managed services.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        /// Optional project id to scope the clear.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    /// Report the version of the resident supervisor binary.
    Version,
    /// Replace the resident supervisor binary without restarting its workloads.
    Upgrade {
        /// Canonical or resolvable path to the staged replacement binary.
        binary: String,
    },
    /// Report the operation the supervisor is currently blocked on, if any.
    CurrentOp,
    /// Spawn a dynamic child process.
    Spawn {
        /// Parent process PID (from Unix socket peer credentials).
        parent_pid: u32,
        /// Name for the spawned child.
        name: String,
        /// Command and arguments to execute.
        command: Vec<String>,
        /// Time-to-live in seconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl: Option<u64>,
        /// Optional log level for the spawned process.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_level: Option<String>,
    },
    /// Subscribe to the supervisor's initial-boot progress. The supervisor
    /// replays every boot frame recorded so far, then streams live frames as
    /// line-delimited JSON until the terminal `Done` frame.
    BootStream,
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
    /// Command failed with a structured diagnostic the client renders.
    Diag(Box<crate::diag::Diagnostic>),
    /// Current status snapshot payload.
    Status(StatusSnapshot),
    /// Inspect payload including recent samples.
    Inspect(Box<InspectPayload>),
    /// Spawn response with child PID.
    Spawned {
        /// PID of the spawned child process.
        pid: u32,
    },
    /// Version of the resident supervisor binary.
    DaemonVersion(String),
    /// Resident supervisor accepted a live upgrade to this version.
    UpgradeAccepted {
        /// Replacement version the installer should wait to observe.
        version: String,
    },
    /// The operation the supervisor is currently working on, if any.
    CurrentOp(Option<crate::opslot::OpReport>),
}

/// Result of sending a command with a short acknowledgement window.
#[derive(Debug)]
pub enum CommandAck {
    /// The supervisor responded before the timeout elapsed.
    Response(ControlResponse),
    /// The command was written successfully, but no response was immediately available.
    Pending,
}

/// Inspect response payload.
#[derive(Debug, Serialize, Deserialize)]
pub struct InspectPayload {
    /// Optional status details for the requested unit.
    pub unit: Option<UnitStatus>,
    /// Recent metric samples associated with the unit.
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
    /// The supervisor accepted the command but did not reply in time.
    #[error("supervisor did not respond in time")]
    Timeout,
    /// Another supervisor owns the runtime.
    #[error("another supervisor owns the runtime")]
    RuntimeBusy,
    /// The connecting peer is not authorized to use the control socket.
    #[error("unauthorized control socket peer (uid {0})")]
    Unauthorized(u32),
}

/// Returns the UID of the peer connected on `stream`.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::unix::io::AsRawFd;

    let mut ucred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let res = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut ucred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ucred.uid)
}

/// Returns the PID of the peer connected on `stream`.
#[cfg(target_os = "linux")]
pub fn peer_pid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::unix::io::AsRawFd;

    let mut ucred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let res = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut ucred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ucred.pid as u32)
}

/// Returns the PID of the peer connected on `stream`.
#[cfg(target_os = "macos")]
pub fn peer_pid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::unix::io::AsRawFd;

    let mut pid: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let res = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            0,
            libc::LOCAL_PEERPID,
            &mut pid as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        )
    };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid as u32)
}

/// Returns the UID of the peer connected on `stream`.
#[cfg(all(unix, not(target_os = "linux")))]
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::unix::io::AsRawFd;

    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    let res = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if res != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

/// Rejects control-socket peers other than the supervisor's own user or root.
///
/// The control socket grants full control over managed services, so only the
/// user running the supervisor (and root, which can bypass any check anyway) is
/// permitted to issue commands.
#[cfg(unix)]
pub fn authenticate_peer(stream: &UnixStream) -> Result<(), ControlError> {
    let peer = peer_uid(stream)?;
    let owner = unsafe { libc::getuid() };
    if peer == owner || peer == 0 {
        Ok(())
    } else {
        Err(ControlError::Unauthorized(peer))
    }
}

/// Sends a command to the supervisor and waits for a response.
pub fn send_command(command: &ControlCommand) -> Result<ControlResponse, ControlError> {
    let stream = connect_stream()?;
    stream.set_read_timeout(Some(COMMAND_READ_TIMEOUT))?;
    let mut stream = stream;
    write_command(&mut stream, command)?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    match reader.read_line(&mut response_line) {
        Ok(_) => {}
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            return Err(ControlError::Timeout);
        }
        Err(err) => return Err(err.into()),
    }

    if response_line.trim().is_empty() {
        return Err(ControlError::NotAvailable);
    }

    let response: ControlResponse = serde_json::from_str(response_line.trim())?;
    if let ControlResponse::Error(message) = &response {
        return Err(ControlError::Server(message.clone()));
    }

    Ok(response)
}

/// Fetches the supervisor's current operation without disturbing an in-flight
/// command. Returns `None` when the supervisor is idle or unreachable.
pub fn current_op() -> Option<crate::opslot::OpReport> {
    let stream = connect_stream().ok()?;
    stream.set_read_timeout(Some(CURRENT_OP_TIMEOUT)).ok()?;
    let mut stream = stream;
    write_command(&mut stream, &ControlCommand::CurrentOp).ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    match serde_json::from_str(line.trim()).ok()? {
        ControlResponse::CurrentOp(report) => report,
        _ => None,
    }
}

/// Sends a command to the supervisor without waiting for a response.
pub fn send_command_detached(command: &ControlCommand) -> Result<(), ControlError> {
    let mut stream = connect_stream()?;
    write_command(&mut stream, command)
}

/// Sends a command and waits briefly for an immediate supervisor response.
pub fn send_command_with_timeout(
    command: &ControlCommand,
    timeout: Duration,
) -> Result<CommandAck, ControlError> {
    let mut stream = connect_stream()?;
    stream.set_write_timeout(Some(timeout))?;
    write_command(&mut stream, command)?;
    stream.set_read_timeout(Some(timeout))?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    match reader.read_line(&mut response_line) {
        Ok(0) => Err(ControlError::NotAvailable),
        Ok(_) if response_line.trim().is_empty() => Err(ControlError::NotAvailable),
        Ok(_) => {
            let response: ControlResponse = serde_json::from_str(response_line.trim())?;
            Ok(CommandAck::Response(response))
        }
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            Ok(CommandAck::Pending)
        }
        Err(err) => Err(err.into()),
    }
}

fn connect_stream() -> Result<UnixStream, ControlError> {
    let path = socket_path()?;
    if !path.exists() {
        return Err(ControlError::NotAvailable);
    }

    match UnixStream::connect(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            Err(ControlError::NotAvailable)
        }
        Err(e) => Err(e.into()),
    }
}

/// Returns the PID that owns the live supervisor socket.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn supervisor_peer_pid() -> Result<u32, ControlError> {
    let stream = connect_stream()?;
    peer_pid(&stream).map_err(ControlError::Io)
}

fn write_command(
    stream: &mut UnixStream,
    command: &ControlCommand,
) -> Result<(), ControlError> {
    let payload = serde_json::to_vec(command)?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// Sends a command to the supervisor and copies the raw response bytes into the provided writer.
pub fn stream_command_output(
    command: &ControlCommand,
    writer: impl Write,
) -> Result<(), ControlError> {
    stream_command_output_interruptible(command, writer, None)
}

/// Like [`stream_command_output`], but publishes a clone of the live connection
/// into `shutdown_slot` so another thread can `shutdown(Both)` it to unblock the
/// copy loop immediately (e.g. on Ctrl-C). Without a slot this is identical to
/// [`stream_command_output`].
pub fn stream_command_output_interruptible(
    command: &ControlCommand,
    mut writer: impl Write,
    shutdown_slot: Option<&std::sync::Mutex<Option<UnixStream>>>,
) -> Result<(), ControlError> {
    let path = socket_path()?;
    if !path.exists() {
        return Err(ControlError::NotAvailable);
    }

    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            return Err(ControlError::NotAvailable);
        }
        Err(e) => return Err(e.into()),
    };
    let payload = serde_json::to_vec(command)?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    // Hand a clone of the connection to the caller so it can force-close the read
    // side from another thread; a shutdown() unblocks the io::copy below at once.
    if let Some(slot) = shutdown_slot
        && let Ok(clone) = stream.try_clone()
        && let Ok(mut guard) = slot.lock()
    {
        *guard = Some(clone);
    }

    let mut reader = BufReader::new(stream);
    io::copy(&mut reader, &mut writer)?;
    writer.flush()?;
    Ok(())
}

/// Subscribes to boot progress and invokes `on_frame` for each frame the
/// supervisor streams, returning once the terminal `Done` frame arrives (or the
/// stream closes). Frames are line-delimited JSON.
pub fn stream_boot_frames(
    mut on_frame: impl FnMut(crate::start::BootFrame),
) -> Result<(), ControlError> {
    let path = socket_path()?;
    if !path.exists() {
        return Err(ControlError::NotAvailable);
    }

    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            return Err(ControlError::NotAvailable);
        }
        Err(e) => return Err(e.into()),
    };
    write_command(&mut stream, &ControlCommand::BootStream)?;

    let reader = BufReader::new(stream);
    let mut completed = false;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let frame: crate::start::BootFrame = serde_json::from_str(line.trim())?;
        let done = frame.is_done();
        on_frame(frame);
        if done {
            completed = true;
            break;
        }
    }
    if completed {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "boot stream ended before its terminal frame",
        )
        .into())
    }
}

/// Utility to read a command from a `UnixStream`. Used by the supervisor event loop.
pub fn read_command(stream: &mut UnixStream) -> Result<ControlCommand, ControlError> {
    let cap = crate::constants::MAX_CONTROL_LINE;
    let mut reader = BufReader::new(stream).take(cap + 1);
    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf)?;

    if buf.len() as u64 > cap {
        return Err(ControlError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "control command exceeds maximum length",
        )));
    }

    let line = String::from_utf8(buf)
        .map_err(|e| ControlError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;

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
        runtime::create_private_dir(parent)?;
    }
    runtime::write_private_file(&path, pid.to_string())?;
    Ok(())
}

/// Path holding the content hash of the last-submitted manifest, beside the
/// config-path hint. Lets a bare command (no `-c`) detect that the on-disk file
/// drifted from what the supervisor loaded.
fn config_hint_hash_path() -> Result<PathBuf, ControlError> {
    Ok(runtime_dir()?.join("config_hint_hash"))
}

/// Persists the resolved config path — and a hash of its current content — to
/// assist CLI fallbacks and detect a dirtied manifest.
pub fn write_config_hint(config: &Path) -> Result<(), ControlError> {
    let hint_path = config_hint_path()?;
    if let Some(parent) = hint_path.parent() {
        runtime::create_private_dir(parent)?;
    }
    let config_str = config.to_string_lossy();
    runtime::write_private_file(&hint_path, config_str.as_bytes())?;

    if let Some(hash) = manifest_content_hash(config) {
        let hash_path = config_hint_hash_path()?;
        runtime::write_private_file(&hash_path, hash.as_bytes())?;
    }
    Ok(())
}

/// Hashes a manifest file by its parsed, canonicalized content so cosmetic edits
/// (whitespace, comments, key order) don't read as a change, but any real
/// manifest change does. Returns `None` if the file cannot be read or parsed.
pub fn manifest_content_hash(config: &Path) -> Option<String> {
    let content = fs::read_to_string(config).ok()?;
    let configs = crate::config::parse_config_projects(&content).ok()?;
    let mut fingerprints: Vec<String> = Vec::new();
    for config in &configs {
        let mut svc: Vec<String> = config
            .services
            .iter()
            .map(|(name, service)| format!("{name}={}", service.compute_hash()))
            .collect();
        svc.sort();
        fingerprints.push(format!("{}:{}", config.project.id, svc.join(",")));
    }
    fingerprints.sort();
    Some(fingerprints.join("\n"))
}

/// Whether the on-disk manifest at the recorded hint path drifted from the hash
/// the supervisor last loaded. `false` when there is no hint, no recorded hash,
/// or the file is unreadable — the cache is used as-is in those cases.
pub fn manifest_is_dirty() -> bool {
    let Ok(Some(hint)) = read_config_hint() else {
        return false;
    };
    let Ok(hash_path) = config_hint_hash_path() else {
        return false;
    };
    let Ok(recorded) = fs::read_to_string(&hash_path) else {
        return false;
    };
    match manifest_content_hash(&hint) {
        Some(current) => current != recorded.trim(),
        None => false,
    }
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

    if let Ok(hash_path) = config_hint_hash_path()
        && hash_path.exists()
    {
        let _ = fs::remove_file(hash_path);
    }

    Ok(())
}

/// Clears runtime files only if they still belong to `owner_pid`.
///
/// A daemon shutting down must not delete a successor's runtime files. During a
/// recycle the CLI stops the old daemon and immediately forks a new one that
/// binds a fresh socket and writes its own pid; the old daemon's teardown runs
/// ~2s behind, so a path-only `cleanup_runtime` would unlink the live
/// successor's socket and pid file, leaving it alive but undiscoverable. This
/// variant no-ops when the on-disk pid no longer names `owner_pid`, so a dying
/// predecessor can never clobber whoever took over.
pub fn cleanup_runtime_owned(owner_pid: libc::pid_t) -> Result<(), ControlError> {
    let still_ours = match read_supervisor_pid() {
        Ok(Some(pid)) => pid == owner_pid,
        Ok(None) => true,
        Err(_) => false,
    };
    if !still_ours {
        return Ok(());
    }
    cleanup_runtime()
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use tempfile::tempdir;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn bind_control_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);

        let listener = bind_control_socket().expect("bind control socket");
        drop(listener);
        let path = socket_path().unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, crate::constants::PRIVATE_FILE_MODE);

        cleanup_runtime().unwrap();
        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    #[test]
    fn control_command_serialization() {
        let start = ControlCommand::Start {
            service: Some("test_service".to_string()),
            project: None,
        };
        let json = serde_json::to_string(&start).unwrap();
        assert!(json.contains("Start"));
        assert!(json.contains("test_service"));

        let stop = ControlCommand::Stop {
            service: None,
            project: None,
        };
        let json = serde_json::to_string(&stop).unwrap();
        assert!(json.contains("Stop"));

        let restart = ControlCommand::Restart {
            config: Some("config.yaml".to_string()),
            service: Some("service".to_string()),
            project: None,
        };
        let json = serde_json::to_string(&restart).unwrap();
        assert!(json.contains("Restart"));
        assert!(json.contains("config.yaml"));
        assert!(!json.contains("project"));

        let shutdown = ControlCommand::Shutdown;
        let json = serde_json::to_string(&shutdown).unwrap();
        assert!(json.contains("Shutdown"));

        let inspect = ControlCommand::Inspect {
            unit: "svc".to_string(),
            project: None,
            samples: 10,
            live: true,
        };
        let json = serde_json::to_string(&inspect).unwrap();
        assert!(json.contains("Inspect"));
        assert!(json.contains("\"samples\":10"));
        assert!(json.contains("\"live\":true"));

        let status = ControlCommand::Status { live: true };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("Status"));
        assert!(json.contains("\"live\":true"));
    }

    #[test]
    fn restart_omits_null_optional_fields() {
        let restart = ControlCommand::Restart {
            config: Some("sysg.config.yaml".to_string()),
            service: None,
            project: None,
        };

        let json = serde_json::to_string(&restart).expect("serialize restart");

        assert_eq!(json, r#"{"Restart":{"config":"sysg.config.yaml"}}"#);
    }

    #[test]
    fn restart_deserializes_missing_and_null_optional_fields() {
        let missing = r#"{"Restart":{"config":"sysg.config.yaml"}}"#;
        let parsed: ControlCommand =
            serde_json::from_str(missing).expect("deserialize missing fields");
        assert!(matches!(
            parsed,
            ControlCommand::Restart {
                config: Some(_),
                service: None,
                project: None
            }
        ));

        let explicit_null =
            r#"{"Restart":{"config":"sysg.config.yaml","service":null,"project":null}}"#;
        let parsed: ControlCommand =
            serde_json::from_str(explicit_null).expect("deserialize null fields");
        assert!(matches!(
            parsed,
            ControlCommand::Restart {
                config: Some(_),
                service: None,
                project: None
            }
        ));
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

        cleanup_runtime().unwrap();
        let read_pid = read_supervisor_pid().unwrap();
        assert_eq!(read_pid, None);

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

        cleanup_runtime().unwrap();
        let hint = read_config_hint().unwrap();
        assert_eq!(hint, None);

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

        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                return;
            }
            Err(err) => panic!("failed to bind test socket: {err}"),
        };

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();

            let cmd = read_command(&mut stream).unwrap();
            assert!(matches!(cmd, ControlCommand::Start { .. }));

            let response = ControlResponse::Message("Started".to_string());
            write_response(&mut stream, &response).unwrap();
        });

        std::thread::sleep(std::time::Duration::from_millis(100));

        let mut stream = UnixStream::connect(&socket_path).unwrap();
        let command = ControlCommand::Start {
            service: Some("test".to_string()),
            project: None,
        };
        let payload = serde_json::to_vec(&command).unwrap();
        stream.write_all(&payload).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let response: ControlResponse = serde_json::from_str(line.trim()).unwrap();

        assert!(matches!(response, ControlResponse::Message(msg) if msg == "Started"));
    }

    #[test]
    fn read_command_rejects_oversized_line() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("oversize.sock");

        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => return,
            Err(err) => panic!("failed to bind test socket: {err}"),
        };

        std::thread::spawn(move || {
            if let Ok(mut stream) = UnixStream::connect(&socket_path) {
                let payload =
                    vec![b'a'; (crate::constants::MAX_CONTROL_LINE as usize) + 16];
                let _ = stream.write_all(&payload);
                let _ = stream.flush();
            }
        });

        let (mut stream, _) = listener.accept().unwrap();
        let result = read_command(&mut stream);
        assert!(matches!(
            result,
            Err(ControlError::Io(err)) if err.kind() == io::ErrorKind::InvalidData
        ));
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

        let hint_path = config_hint_path().unwrap();
        fs::create_dir_all(hint_path.parent().unwrap()).unwrap();
        fs::write(&hint_path, "").unwrap();

        let hint = read_config_hint().unwrap();
        assert_eq!(hint, None);

        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }
}
