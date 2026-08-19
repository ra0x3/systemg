//! The `version` command: what this binary is, and what the resident supervisor
//! is actually running.
//!
//! These drift apart on any install. `upgrade-supervisor` re-execs the
//! supervisor inside its existing PID, so a box can keep serving an old build
//! from a long-lived process while `sysg --version` reports the new binary
//! sitting on disk.

use std::{fmt, sync::mpsc, thread, time::Duration};

use serde::Serialize;

use crate::ipc::{self, CommandAck, ControlCommand, ControlResponse};

/// Version of the binary serving this process.
pub const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Hard ceiling on the whole probe. A wedged supervisor stops accepting
/// connections, and `connect()` on a full backlog blocks with no timeout of its
/// own, so the budget is enforced from outside the probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What this binary and the resident supervisor are each running.
#[derive(Debug, Clone, Serialize)]
pub struct VersionReport {
    /// Version of the binary that served this command.
    pub cli: String,
    /// Version the resident supervisor reports, when one answers.
    pub supervisor: Option<String>,
    /// PID holding the control socket.
    pub supervisor_pid: Option<u32>,
    /// Path that PID is executing, where the kernel exposes it.
    pub supervisor_binary: Option<String>,
}

impl VersionReport {
    /// Probes the resident supervisor, tolerating its absence or its silence.
    pub fn collect() -> Self {
        let (supervisor, supervisor_pid) = probe();

        Self {
            cli: CLI_VERSION.to_string(),
            supervisor,
            supervisor_pid,
            supervisor_binary: supervisor_pid.and_then(running_binary),
        }
    }

    /// Whether the supervisor is serving a different build than this binary.
    pub fn drifted(&self) -> bool {
        self.supervisor
            .as_deref()
            .is_some_and(|version| version != self.cli)
    }
}

impl fmt::Display for VersionReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sysg {} (this binary)", self.cli)?;

        match (&self.supervisor, self.supervisor_pid) {
            (Some(version), Some(pid)) => {
                write!(f, "\nsupervisor {version} (pid {pid})")?
            }
            (Some(version), None) => write!(f, "\nsupervisor {version}")?,
            (None, Some(pid)) => {
                write!(f, "\nsupervisor unreachable (pid {pid} holds the socket)")?;
            }
            (None, None) => return write!(f, "\nsupervisor not running"),
        }

        if let Some(binary) = &self.supervisor_binary {
            write!(f, "\n  executing {binary}")?;
        }

        if self.drifted() {
            write!(
                f,
                "\n  drift: the supervisor is still serving {}; rerun the installer to activate {}",
                self.supervisor.as_deref().unwrap_or(""),
                self.cli
            )?;
        }

        Ok(())
    }
}

/// Asks the resident supervisor for its version and the PID that answered,
/// under a ceiling the probe itself cannot enforce.
///
/// The probe runs on a worker because `connect()` to a supervisor that has
/// stopped accepting is unbounded. Reporting the version is the last thing this
/// process does, so a worker still stuck on a dead socket dies with it rather
/// than outliving the answer.
fn probe() -> (Option<String>, Option<u32>) {
    let (tx, rx) = mpsc::channel();

    if thread::Builder::new()
        .name("sysg-version-probe".into())
        .spawn(move || {
            let _ = tx.send(ipc::send_command_with_peer(
                &ControlCommand::Version,
                PROBE_TIMEOUT / 2,
            ));
        })
        .is_err()
    {
        return (None, None);
    }

    match rx.recv_timeout(PROBE_TIMEOUT) {
        Ok(Ok((CommandAck::Response(ControlResponse::DaemonVersion(version)), pid))) => {
            (Some(version), pid)
        }
        Ok(Ok((_, pid))) => (None, pid),
        _ => (None, None),
    }
}

/// Resolves the executable a PID is running from the kernel's own view.
#[cfg(target_os = "linux")]
fn running_binary(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

/// The kernel exposes no equivalent of `/proc/<pid>/exe` here.
#[cfg(not(target_os = "linux"))]
fn running_binary(_pid: u32) -> Option<String> {
    None
}
