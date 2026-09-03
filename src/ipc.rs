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
    config::supervisor::SupervisorConfig,
    metrics::MetricSample,
    runtime,
    status::{ProjectRunMode, StatusSnapshot, UnitStatus},
};

/// How long a read slice waits before the client pauses to check that the
/// supervisor is still alive.
///
/// A flat deadline on the whole command was wrong: everything except the four
/// cached reads is queued onto the single owner thread and answered only once
/// it completes, so a restart of enough units outran the clock and the client
/// declared a command that went on to succeed "not applied". Liveness, not
/// elapsed time, is what separates a slow mutation from a dead supervisor.
const COMMAND_POLL_SLICE: Duration = Duration::from_secs(5);

/// Hard bound on one liveness probe, connect included.
const PROBE_WINDOW: Duration = Duration::from_secs(5);

/// Ceiling on a single accumulated response, guarding only against unbounded
/// growth from a peer that never terminates its line.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// How long the supervisor may fail every liveness probe before the client
/// stops waiting, so a genuinely dead socket still surfaces as an error.
const UNRESPONSIVE_GRACE: Duration = Duration::from_secs(30);

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
        /// Client-generated id for this operation's progress journal.
        ///
        /// Carried on the mutation so the supervisor registers exactly the
        /// journal the client already subscribed to. A key derived from the
        /// target instead would cross-wire two identical concurrent commands
        /// and could not tell a re-run from the one still in flight.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        watch: Option<String>,
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
        /// Client-generated id for this operation's progress journal.
        ///
        /// The boot this queues runs on its own thread and outlives the
        /// command, so the journal is held open by a lease until the last
        /// project it started has finished.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        watch: Option<String>,
    },
    /// Stop all services for one project.
    StopProject {
        /// Stable project id to stop.
        project: String,
        /// Client-generated id for this operation's progress journal.
        ///
        /// Carried on the mutation so the supervisor registers exactly the
        /// journal the client already subscribed to. A key derived from the
        /// target instead would cross-wire two identical concurrent commands
        /// and could not tell a re-run from the one still in flight.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        watch: Option<String>,
    },
    /// Stop one or all services.
    Stop {
        /// Optional service name to stop. If None, stops all services.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service: Option<String>,
        /// Optional project id to target.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        /// Client-generated id for this operation's progress journal.
        ///
        /// Carried on the mutation so the supervisor registers exactly the
        /// journal the client already subscribed to. A key derived from the
        /// target instead would cross-wire two identical concurrent commands
        /// and could not tell a re-run from the one still in flight.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        watch: Option<String>,
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
        /// Bounce every declared unit instead of reconciling the manifest delta.
        ///
        /// Defaults to false, which is the reconcile the supervisor has always
        /// done — so an old CLI talking to a new supervisor keeps its exact
        /// behavior, including the SG0304 that now guards it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        all: bool,
        /// Client-generated id for this operation's progress journal.
        ///
        /// Carried on the mutation so the supervisor registers exactly the
        /// journal the client already subscribed to. A key derived from the
        /// target instead would cross-wire two identical concurrent commands
        /// and could not tell a re-run from the one still in flight.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        watch: Option<String>,
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
    /// Report which projects declare a service, straight from the supervisor's
    /// loaded configs.
    ///
    /// Distinct from a status snapshot: that is a periodically-rebuilt cache
    /// which drops a project whose state was momentarily unreadable, so it
    /// cannot answer "is this service declared". This reads the loaded manifests
    /// themselves, which is the only authoritative source for the question.
    DeclaringProjects {
        /// Bare service name to look up.
        service: String,
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
    /// Subscribe to the progress of a mutation already in flight.
    ///
    /// Separate from [`ControlCommand::BootStream`] because the boot journal is
    /// sealed by its terminal frame and never reopens: a restart or stop needs
    /// its own journal, and the client has to say which one it is watching.
    /// Older supervisors answer this with an error, which the CLI treats as
    /// "no live tree available" and falls back to its spinner.
    OpStream {
        /// Identifies the operation whose journal to serve.
        op: String,
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
    /// Command failed with a structured diagnostic the client renders.
    Diag(Box<crate::diag::Diagnostic>),
    /// Project ids answering a lookup, in the supervisor's own order.
    Projects(Vec<String>),
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
    /// The supervisor stopped answering its control socket while the command
    /// was in flight.
    #[error("supervisor did not respond in time")]
    Timeout,
    /// The supervisor is answering, and still working on the command, but has
    /// not finished within the client's budget. The command was accepted and
    /// is not cancelled by this.
    #[error("supervisor is still running the command")]
    StillRunning,
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

/// How long the client is willing to wait on one command, and on what terms.
#[derive(Debug, Clone, Copy)]
struct WaitPolicy {
    /// How long one read blocks before pausing to probe for liveness.
    slice: Duration,
    /// How long one liveness probe may take before it counts as unanswered.
    probe_window: Duration,
    /// How long the supervisor may fail every probe before the wait is over.
    unresponsive_grace: Duration,
    /// Backstop against a deadlocked owner thread; `None` waits forever.
    budget: Option<Duration>,
}

impl WaitPolicy {
    /// The default policy, with the budget taken from `supervisor.xml`.
    ///
    /// Read per command rather than cached, so raising the budget takes effect
    /// on the next command instead of after a supervisor restart. The slice,
    /// probe window and grace stay fixed: they describe how liveness is
    /// detected, not how patient the operator is.
    fn current() -> Self {
        Self {
            slice: COMMAND_POLL_SLICE,
            probe_window: PROBE_WINDOW,
            unresponsive_grace: UNRESPONSIVE_GRACE,
            budget: SupervisorConfig::load_or_default()
                .timeouts
                .command_wait_budget(),
        }
    }
}

/// Sends a command to the supervisor and waits for a response.
///
/// The wait is bounded by the supervisor's liveness rather than by the
/// command's duration: a queued mutation holds the socket open for as long as
/// the owner thread takes, and abandoning it mid-flight would report a command
/// that is still running — and will still be applied — as one that failed.
pub fn send_command(command: &ControlCommand) -> Result<ControlResponse, ControlError> {
    send_command_with_policy(command, WaitPolicy::current())
}

fn send_command_with_policy(
    command: &ControlCommand,
    policy: WaitPolicy,
) -> Result<ControlResponse, ControlError> {
    let stream = connect_stream()?;
    stream.set_read_timeout(Some(policy.slice))?;
    let mut stream = stream;
    write_command(&mut stream, command)?;

    let mut reader = BufReader::new(stream);
    let mut response = Vec::new();
    let started = std::time::Instant::now();
    let mut unresponsive_since: Option<std::time::Instant> = None;

    loop {
        // One `fill_buf` is one underlying read, so the slice bounds each pass
        // and the checks below always get to run. `read_until` would keep
        // looping inside itself on a peer that trickles bytes without a
        // newline, and neither the probe nor the budget would ever be reached.
        // Bytes already taken stay in `response`, so a reply split across
        // slices is resumed rather than corrupted — which `read_line` could not
        // promise for a partial multi-byte character.
        let taken = match reader.fill_buf() {
            Ok(buffered) => {
                let end = buffered
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map(|at| at + 1);
                let take = end.unwrap_or(buffered.len());
                response.extend_from_slice(&buffered[..take]);
                (take, end.is_some(), buffered.is_empty())
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                (0, false, false)
            }
            Err(err) => return Err(err.into()),
        };
        let (take, complete, eof) = taken;
        reader.consume(take);
        if complete || eof {
            break;
        }
        // Only an out-of-memory guard, set far above any real reply: a status
        // snapshot has no declared size limit, so a tight cap here would turn a
        // large but legitimate response into a failure. What actually bounds a
        // dribbling peer is the budget below.
        if response.len() > MAX_RESPONSE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "supervisor response exceeded the maximum size",
            )
            .into());
        }

        // Bytes arriving are themselves proof of life, so the probe is only
        // spent on a silent slice. The budget is checked either way: a peer
        // trickling bytes without a newline would otherwise be bounded only by
        // the cap above, which at a slow enough drip is no bound at all.
        let responsive = take > 0 || probe_within(policy.probe_window).is_ok();
        if responsive {
            unresponsive_since = None;
        } else {
            let since = *unresponsive_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= policy.unresponsive_grace {
                return Err(ControlError::Timeout);
            }
        }

        // Only a supervisor that just answered can be reported as still
        // working. With a probe outstanding the honest outcome is unknown, so
        // the wait continues until the grace above resolves it either way.
        if responsive
            && policy
                .budget
                .is_some_and(|budget| started.elapsed() >= budget)
        {
            return Err(ControlError::StillRunning);
        }
    }

    let response = String::from_utf8(response).map_err(|err| {
        io::Error::new(io::ErrorKind::InvalidData, err.utf8_error().to_string())
    })?;
    if response.trim().is_empty() {
        return Err(ControlError::NotAvailable);
    }

    let response: ControlResponse = serde_json::from_str(response.trim())?;
    if let ControlResponse::Error(message) = &response {
        return Err(ControlError::Server(message.clone()));
    }

    Ok(response)
}

/// Runs the liveness probe under a hard wall-clock bound.
///
/// The socket timeouts inside the probe cover the write and the read but not
/// `UnixStream::connect`, which has no timeout of its own and blocks while the
/// listener's accept backlog is full — exactly the state a saturated supervisor
/// is in. Handing the probe to a thread and bounding the join makes "did not
/// answer inside the window" the answer in every case, including that one. The
/// thread is abandoned rather than cancelled; it ends on its own when the
/// connect resolves.
fn probe_within(
    window: Duration,
) -> Result<Option<crate::opslot::OpReport>, ControlError> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("sysg-liveness-probe".into())
        .spawn(move || {
            let _ = tx.send(probe_current_op());
        })
        .map_err(ControlError::Io)?;
    match rx.recv_timeout(window) {
        Ok(result) => result,
        Err(_) => Err(ControlError::Timeout),
    }
}

/// Asks the supervisor what it is working on, distinguishing "idle" from
/// "unreachable" so a liveness check can tell the two apart.
///
/// Any well-formed reply counts as alive, an error response included: a
/// supervisor old enough to reject `CurrentOp` outright is still answering its
/// socket, and reading that as wedged would abandon a healthy command.
pub fn probe_current_op() -> Result<Option<crate::opslot::OpReport>, ControlError> {
    let stream = connect_stream()?;
    stream.set_read_timeout(Some(CURRENT_OP_TIMEOUT))?;
    stream.set_write_timeout(Some(CURRENT_OP_TIMEOUT))?;
    let mut stream = stream;
    write_command(&mut stream, &ControlCommand::CurrentOp)?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err(ControlError::NotAvailable);
    }
    match serde_json::from_str(line.trim())? {
        ControlResponse::CurrentOp(report) => Ok(report),
        _ => Ok(None),
    }
}

/// Fetches the supervisor's current operation without disturbing an in-flight
/// command. Returns `None` when the supervisor is idle or unreachable.
pub fn current_op() -> Option<crate::opslot::OpReport> {
    probe_current_op().ok().flatten()
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
    exchange(connect_stream()?, command, timeout)
}

/// Sends a command and reports the PID that answered it. Both come off the same
/// connection, so a reply can never be attributed to a supervisor other than the
/// one that produced it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn send_command_with_peer(
    command: &ControlCommand,
    timeout: Duration,
) -> Result<(CommandAck, Option<u32>), ControlError> {
    let stream = connect_stream()?;
    let peer = peer_pid(&stream).ok();
    exchange(stream, command, timeout).map(|ack| (ack, peer))
}

/// Writes one command on `stream` and reads its response under `timeout`.
fn exchange(
    mut stream: UnixStream,
    command: &ControlCommand,
    timeout: Duration,
) -> Result<CommandAck, ControlError> {
    stream.set_write_timeout(Some(timeout))?;
    write_command(&mut stream, command)?;
    stream.set_read_timeout(Some(timeout))?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    let ack = match reader.read_line(&mut response_line) {
        Ok(0) => return Err(ControlError::NotAvailable),
        Ok(_) if response_line.trim().is_empty() => {
            return Err(ControlError::NotAvailable);
        }
        Ok(_) => {
            let response: ControlResponse = serde_json::from_str(response_line.trim())?;
            CommandAck::Response(response)
        }
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            CommandAck::Pending
        }
        Err(err) => return Err(err.into()),
    };
    Ok(ack)
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
    on_frame: impl FnMut(crate::start::BootFrame),
) -> Result<(), ControlError> {
    stream_frames(ControlCommand::BootStream, on_frame)
}

/// Streams the progress frames of a mutation already in flight.
///
/// Returns [`ControlError::NotAvailable`] when the supervisor does not know the
/// operation — it finished before the client attached, or the daemon predates
/// `OpStream` — so the caller falls back to a plain spinner rather than hanging.
pub fn stream_op_frames(
    op: &str,
    on_frame: impl FnMut(crate::start::BootFrame),
) -> Result<(), ControlError> {
    stream_frames(ControlCommand::OpStream { op: op.to_string() }, on_frame)
}

/// Subscribes to a progress stream and replays its frames until the terminal one.
fn stream_frames(
    command: ControlCommand,
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
    write_command(&mut stream, &command)?;

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

/// Decodes one raw control frame (a single newline-delimited line, cap
/// already enforced by the reader) into a [`ControlCommand`]. Pure so the
/// fuzz harness exercises the exact production path.
pub fn decode_control_frame(buf: &[u8]) -> Result<ControlCommand, ControlError> {
    if buf.len() as u64 > crate::constants::MAX_CONTROL_LINE {
        return Err(ControlError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "control command exceeds maximum length",
        )));
    }

    let line = std::str::from_utf8(buf)
        .map_err(|e| ControlError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;

    if line.trim().is_empty() {
        return Err(ControlError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty control command",
        )));
    }

    Ok(serde_json::from_str(line.trim())?)
}

/// Utility to read a command from a `UnixStream`. Used by the supervisor event loop.
pub fn read_command(stream: &mut UnixStream) -> Result<ControlCommand, ControlError> {
    let cap = crate::constants::MAX_CONTROL_LINE;
    let mut reader = BufReader::new(stream).take(cap + 1);
    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf)?;

    decode_control_frame(&buf)
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

/// Records that a service manager owns the supervisor with this PID, so a CLI
/// that would otherwise stop and respawn it can refuse instead: recycling a
/// managed supervisor fights the manager for ownership and leaves the winner
/// unsupervised. Stale records are harmless — the PID is compared against the
/// live supervisor's.
pub fn write_managed_marker(pid: libc::pid_t, config: &Path) -> Result<(), ControlError> {
    let path = managed_marker_path()?;
    if let Some(parent) = path.parent() {
        runtime::create_private_dir(parent)?;
    }
    runtime::write_private_file(&path, format!("{pid}\n{}", config.display()))?;
    Ok(())
}

/// Drops any manager-ownership record, so a supervisor this manager did not
/// start never inherits its predecessor's claim — a record left behind by a
/// killed supervisor would otherwise block a legitimate recycle the moment the
/// kernel reissued its PID.
pub fn clear_managed_marker() -> Result<(), ControlError> {
    let path = managed_marker_path()?;
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// Whether a service manager owns the supervisor that is running now.
pub fn supervisor_is_managed() -> bool {
    managed_owner_in(&runtime::state_dir()).is_some()
}

/// The manifest a manager-owned supervisor is booting in `runtime_dir`, when
/// one is live there. Reads another account's runtime as well as this one's, so
/// a unit can be checked against the runtime it will actually target rather
/// than the runtime of whoever ran the command.
pub fn managed_owner_in(runtime_dir: &Path) -> Option<PathBuf> {
    let pid = live_supervisor_pid_in(runtime_dir)?;
    let recorded = std::fs::read_to_string(runtime_dir.join("managed")).ok()?;
    let (marked_pid, config) = recorded.split_once('\n')?;
    (marked_pid.trim().parse::<libc::pid_t>().ok()? == pid)
        .then(|| PathBuf::from(config.trim()))
}

/// The PID of a supervisor that is running in `runtime_dir` right now.
pub fn live_supervisor_pid_in(runtime_dir: &Path) -> Option<libc::pid_t> {
    let recorded = std::fs::read_to_string(runtime_dir.join("sysg.pid")).ok()?;
    let pid = recorded.trim().parse::<libc::pid_t>().ok()?;
    (unsafe { libc::kill(pid, 0) } == 0).then_some(pid)
}

/// Where the manager-ownership record lives.
fn managed_marker_path() -> Result<PathBuf, ControlError> {
    Ok(runtime_dir()?.join("managed"))
}

/// Persists the resolved config path to assist CLI fallbacks.
pub fn write_config_hint(config: &Path) -> Result<(), ControlError> {
    let hint_path = config_hint_path()?;
    if let Some(parent) = hint_path.parent() {
        runtime::create_private_dir(parent)?;
    }
    let config_str = config.to_string_lossy();
    runtime::write_private_file(&hint_path, config_str.as_bytes())?;
    Ok(())
}

/// Hashes a manifest file by its parsed, canonicalized content so cosmetic edits
/// (whitespace, comments, key order) don't read as a change, but any real
/// manifest change does. Includes are resolved first, so a fragment edit reads
/// as a change and a broken fragment fails with its include chain.
pub fn manifest_content_hash(
    config: &Path,
) -> Result<String, crate::error::ProcessManagerError> {
    let content = fs::read_to_string(config)?;
    let content = crate::config::resolve_includes(&content, config)?;
    manifest_fingerprint(&content)
}

/// Fingerprints already include-resolved manifest text; the content-addressed
/// core of [`manifest_content_hash`], for callers that must hash the exact
/// bytes they captured rather than a second disk read.
pub fn manifest_fingerprint(
    content: &str,
) -> Result<String, crate::error::ProcessManagerError> {
    let configs = crate::config::parse_config_projects(content)?;
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
    Ok(fingerprints.join("\n"))
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

    if let Ok(managed_path) = managed_marker_path()
        && managed_path.exists()
    {
        let _ = fs::remove_file(managed_path);
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
    use std::{
        os::unix::net::UnixListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

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

    /// A supervisor stand-in: answers `CurrentOp` however `probe_reply` says,
    /// and replies to the one mutation after `delay` — or never, when
    /// `mutation_reply` is `None`.
    struct FakeSupervisor {
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeSupervisor {
        fn spawn(
            listener: UnixListener,
            probe_reply: Option<ControlResponse>,
            delay: Duration,
            mutation_reply: Option<Vec<u8>>,
        ) -> Self {
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let handle = std::thread::spawn(move || {
                let mut held = Vec::new();
                while !worker_stop.load(Ordering::Relaxed) {
                    let mut stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(_) => break,
                    };
                    stream.set_nonblocking(false).unwrap();
                    let command = match read_command(&mut stream) {
                        Ok(command) => command,
                        Err(_) => continue,
                    };
                    if matches!(command, ControlCommand::CurrentOp) {
                        if let Some(reply) = &probe_reply {
                            let _ = write_response(&mut stream, reply);
                        }
                        continue;
                    }
                    match &mutation_reply {
                        Some(reply) => {
                            let reply = reply.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(delay);
                                let _ = stream.write_all(&reply);
                                let _ = stream.flush();
                            });
                        }
                        // Held open, never answered: the owner thread is wedged
                        // but the socket is not.
                        None => held.push(stream),
                    }
                }
            });
            Self {
                stop,
                handle: Some(handle),
            }
        }
    }

    impl Drop for FakeSupervisor {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn test_policy(budget: Option<Duration>) -> WaitPolicy {
        WaitPolicy {
            slice: Duration::from_millis(50),
            probe_window: Duration::from_millis(500),
            unresponsive_grace: Duration::from_millis(200),
            budget,
        }
    }

    /// Binds the runtime control socket under a temp `HOME`, returning the
    /// listener and a guard that restores the environment.
    fn fake_runtime(temp: &tempfile::TempDir) -> (UnixListener, Option<String>) {
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
        (
            bind_control_socket().expect("bind control socket"),
            original_home,
        )
    }

    fn restore_home(original_home: Option<String>) {
        match original_home {
            Some(val) => unsafe { std::env::set_var("HOME", val) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
    }

    fn restart_all() -> ControlCommand {
        ControlCommand::Restart {
            all: false,
            service: None,
            project: None,
            config: None,
            watch: None,
        }
    }

    #[test]
    fn a_reply_slower_than_one_slice_is_still_awaited() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let (listener, original_home) = fake_runtime(&temp);
        let reply = {
            let mut bytes =
                serde_json::to_vec(&ControlResponse::Message("done".into())).unwrap();
            bytes.push(b'\n');
            bytes
        };
        let _supervisor = FakeSupervisor::spawn(
            listener,
            Some(ControlResponse::CurrentOp(None)),
            Duration::from_millis(400),
            Some(reply),
        );

        let result = send_command_with_policy(&restart_all(), test_policy(None));

        assert!(
            matches!(&result, Ok(ControlResponse::Message(message)) if message == "done"),
            "a mutation that outlasts a read slice must not be abandoned: {result:?}"
        );
        restore_home(original_home);
    }

    #[test]
    fn a_reply_split_across_slices_is_not_corrupted() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let (listener, original_home) = fake_runtime(&temp);
        // Split mid-way through the multi-byte character, so a resumed read
        // that dropped its partial bytes would produce invalid UTF-8.
        let message = "réstarted ✓";
        let mut reply =
            serde_json::to_vec(&ControlResponse::Message(message.into())).unwrap();
        reply.push(b'\n');
        let split = reply.len() / 2;
        let (head, tail) = reply.split_at(split);
        let (head, tail) = (head.to_vec(), tail.to_vec());

        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_nonblocking(false).unwrap();
                let command = match read_command(&mut stream) {
                    Ok(command) => command,
                    Err(_) => continue,
                };
                if matches!(command, ControlCommand::CurrentOp) {
                    let _ =
                        write_response(&mut stream, &ControlResponse::CurrentOp(None));
                    continue;
                }
                let head = head.clone();
                let tail = tail.clone();
                std::thread::spawn(move || {
                    let _ = stream.write_all(&head);
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_millis(200));
                    let _ = stream.write_all(&tail);
                    let _ = stream.flush();
                });
            }
        });

        let result = send_command_with_policy(&restart_all(), test_policy(None));

        stop.store(true, Ordering::Relaxed);
        let _ = worker.join();
        assert!(
            matches!(&result, Ok(ControlResponse::Message(got)) if got == message),
            "a reply split across slices must be resumed, not truncated: {result:?}"
        );
        restore_home(original_home);
    }

    #[test]
    fn a_supervisor_that_stops_answering_ends_the_wait() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let (listener, original_home) = fake_runtime(&temp);
        let _supervisor =
            FakeSupervisor::spawn(listener, None, Duration::from_secs(60), None);

        let result = send_command_with_policy(&restart_all(), test_policy(None));

        assert!(
            matches!(result, Err(ControlError::Timeout)),
            "a socket that answers nothing is a dead supervisor: {result:?}"
        );
        restore_home(original_home);
    }

    #[test]
    fn an_error_reply_to_the_probe_still_counts_as_alive() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let (listener, original_home) = fake_runtime(&temp);
        // A supervisor old enough to reject CurrentOp is still answering; the
        // wait must not read that as wedged.
        let _supervisor = FakeSupervisor::spawn(
            listener,
            Some(ControlResponse::Error("unknown command".into())),
            Duration::from_secs(60),
            None,
        );

        let result = send_command_with_policy(
            &restart_all(),
            test_policy(Some(Duration::from_millis(400))),
        );

        assert!(
            matches!(result, Err(ControlError::StillRunning)),
            "a responsive supervisor must exhaust the budget, not the grace: {result:?}"
        );
        restore_home(original_home);
    }

    #[test]
    fn a_wedged_owner_thread_reports_the_command_as_still_running() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let (listener, original_home) = fake_runtime(&temp);
        let _supervisor = FakeSupervisor::spawn(
            listener,
            Some(ControlResponse::CurrentOp(None)),
            Duration::from_secs(60),
            None,
        );

        let result = send_command_with_policy(
            &restart_all(),
            test_policy(Some(Duration::from_millis(400))),
        );

        assert!(
            matches!(result, Err(ControlError::StillRunning)),
            "an answering supervisor that never finishes is not a refusal: {result:?}"
        );
        restore_home(original_home);
    }

    #[test]
    fn a_peer_dribbling_bytes_forever_still_hits_the_budget() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let (listener, original_home) = fake_runtime(&temp);

        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let writer_stop = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_nonblocking(false).unwrap();
                if read_command(&mut stream).is_err() {
                    continue;
                }
                // Never a newline: the reply never completes, but the stream is
                // never silent either, so nothing about it looks unresponsive.
                let writer_stop = Arc::clone(&writer_stop);
                std::thread::spawn(move || {
                    while !writer_stop.load(Ordering::Relaxed) {
                        if stream.write_all(b" ").is_err() {
                            return;
                        }
                        let _ = stream.flush();
                        std::thread::sleep(Duration::from_millis(10));
                    }
                });
            }
        });

        let result = send_command_with_policy(
            &restart_all(),
            test_policy(Some(Duration::from_millis(300))),
        );

        stop.store(true, Ordering::Relaxed);
        let _ = worker.join();
        assert!(
            matches!(result, Err(ControlError::StillRunning)),
            "an unterminated reply must still be bounded by the budget: {result:?}"
        );
        restore_home(original_home);
    }

    /// Points the runtime at a temp `HOME` and writes `supervisor.xml` there.
    fn fake_supervisor_config(temp: &tempfile::TempDir, body: &str) -> Option<String> {
        let original_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp.path());
        }
        crate::runtime::init(crate::runtime::RuntimeMode::User);
        crate::runtime::set_drop_privileges(false);
        let path = SupervisorConfig::path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        original_home
    }

    #[test]
    fn the_budget_comes_from_the_supervisor_config() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = fake_supervisor_config(
            &temp,
            "<supervisor><timeouts><command_wait_secs>30</command_wait_secs></timeouts></supervisor>",
        );

        assert_eq!(WaitPolicy::current().budget, Some(Duration::from_secs(30)));

        restore_home(original_home);
    }

    #[test]
    fn a_zero_budget_in_the_config_waits_forever() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = fake_supervisor_config(
            &temp,
            "<supervisor><timeouts><command_wait_secs>0</command_wait_secs></timeouts></supervisor>",
        );

        assert_eq!(WaitPolicy::current().budget, None);

        restore_home(original_home);
    }

    #[test]
    /// A config predating the setting — or none at all — keeps the built-in
    /// backstop rather than waiting forever.
    fn a_config_without_the_setting_keeps_the_default_budget() {
        let _guard = crate::test_utils::env_lock();
        let temp = tempdir().unwrap();
        let original_home = fake_supervisor_config(
            &temp,
            // A parseable file that simply predates the setting: a malformed
            // one would fall back to defaults wholesale and prove nothing
            // about the field's own default.
            "<supervisor><logs><max_bytes>42</max_bytes><max_files>7</max_files></logs></supervisor>",
        );

        assert_eq!(
            WaitPolicy::current().budget,
            Some(crate::constants::COMMAND_WAIT_BUDGET)
        );

        fs::remove_file(SupervisorConfig::path()).unwrap();
        assert_eq!(
            WaitPolicy::current().budget,
            Some(crate::constants::COMMAND_WAIT_BUDGET),
            "a missing config must not be read as a missing backstop"
        );

        restore_home(original_home);
    }

    #[test]
    fn control_command_serialization() {
        let start = ControlCommand::Start {
            service: Some("test_service".to_string()),
            project: None,
            watch: None,
        };
        let json = serde_json::to_string(&start).unwrap();
        assert!(json.contains("Start"));
        assert!(json.contains("test_service"));

        let stop = ControlCommand::Stop {
            service: None,
            project: None,
            watch: None,
        };
        let json = serde_json::to_string(&stop).unwrap();
        assert!(json.contains("Stop"));

        let restart = ControlCommand::Restart {
            all: false,
            config: Some("config.yaml".to_string()),
            service: Some("service".to_string()),
            project: None,
            watch: None,
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
            all: false,
            config: Some("sysg.config.yaml".to_string()),
            service: None,
            project: None,
            watch: None,
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
                project: None,
                ..
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
                project: None,
                ..
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
            watch: None,
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

    #[test]
    fn decode_control_frame_rejects_over_cap() {
        let cap = crate::constants::MAX_CONTROL_LINE as usize;
        assert!(decode_control_frame(&vec![b' '; cap + 1]).is_err());
    }

    #[test]
    fn decode_control_frame_accepts_exact_cap_with_padding() {
        let cap = crate::constants::MAX_CONTROL_LINE as usize;
        let json = serde_json::to_string(&ControlCommand::CurrentOp).unwrap();
        let mut buf = json.into_bytes();
        buf.resize(cap, b' ');
        assert!(decode_control_frame(&buf).is_ok());
    }

    #[test]
    fn decode_control_frame_accepts_newline_terminated() {
        let mut buf = serde_json::to_string(&ControlCommand::CurrentOp)
            .unwrap()
            .into_bytes();
        buf.push(b'\n');
        assert!(decode_control_frame(&buf).is_ok());
    }

    #[test]
    fn decode_control_frame_rejects_invalid_utf8() {
        assert!(decode_control_frame(&[0xff, 0xfe, b'{', b'}']).is_err());
    }

    #[test]
    fn decode_control_frame_rejects_empty_and_whitespace() {
        assert!(decode_control_frame(b"").is_err());
        assert!(decode_control_frame(b"   \n").is_err());
    }
}
