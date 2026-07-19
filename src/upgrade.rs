//! Validation and diagnostics for workload-preserving supervisor upgrades.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::{
    config::EffectiveLogsConfig,
    diag::{Diagnostic, SgCode},
    runtime,
    status::ProjectRunMode,
};

/// Wire protocol implemented by supervisors that support live re-execution.
pub const LIVE_REEXEC_PROTOCOL: u16 = 1;

/// Serialized supervisor handoff schema understood by this binary.
pub const HANDOFF_SCHEMA_VERSION: u16 = 1;

/// Maximum time allowed for a candidate binary to report upgrade metadata.
const TARGET_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between candidate process completion checks.
const TARGET_PROBE_INTERVAL: Duration = Duration::from_millis(20);

/// Metadata emitted by a sysg binary for live-upgrade compatibility checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveUpgradeInfo {
    /// Binary release version.
    pub version: Version,
    /// Live re-execution protocol version.
    pub protocol: u16,
}

impl LiveUpgradeInfo {
    /// Returns metadata for the currently executing binary.
    pub fn current() -> Self {
        Self {
            version: Version::parse(env!("CARGO_PKG_VERSION"))
                .expect("Cargo package version must be valid semver"),
            protocol: LIVE_REEXEC_PROTOCOL,
        }
    }
}

/// Validated candidate accepted for live supervisor re-execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeTarget {
    /// Canonical executable path.
    pub path: PathBuf,
    /// Metadata reported by the executable.
    pub info: LiveUpgradeInfo,
}

/// Kernel identity of one service process transferred across supervisor re-exec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffProcess {
    /// Service name within its project.
    pub service: String,
    /// Process identifier retained across re-exec.
    pub pid: u32,
    /// Service process-group identifier.
    pub pgid: i32,
    /// Kernel process start time used to reject PID reuse.
    pub started: u64,
}

/// Inherited service-output pipe restored by the replacement supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffLogPipe {
    /// Project that owns the output stream.
    pub project: String,
    /// Service that owns the output stream.
    pub service: String,
    /// Stable stream label, either `stdout` or `stderr`.
    pub stream: String,
    /// Descriptor inherited across `exec`.
    pub fd: i32,
    /// Unterminated bytes already consumed from the pipe.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<u8>,
    /// Rotation and sink policy used by the canonical log writer.
    pub settings: EffectiveLogsConfig,
}

/// In-memory lifecycle bookkeeping restored for one project daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffDaemonState {
    /// Verified service processes retained across re-exec.
    pub processes: Vec<HandoffProcess>,
    /// Services explicitly stopped by the user.
    pub manual_stops: Vec<String>,
    /// Services whose automatic restart policy is temporarily suppressed.
    pub restart_suppressed: Vec<String>,
    /// Restart attempts accumulated for each service.
    pub restart_counts: BTreeMap<String, u32>,
    /// Dependents held down until failed dependencies recover.
    pub stopped_for_dependency: BTreeMap<String, Vec<String>>,
}

/// Manifest and runtime state for one project managed by the supervisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffProject {
    /// Stable project id used to select it from a multi-project manifest.
    pub project_id: String,
    /// Canonical path associated with the manifest.
    pub config_path: PathBuf,
    /// Semantic manifest hash used to reject changes during handoff.
    pub config_hash: String,
    /// Foreground or daemonized attachment mode.
    pub mode: ProjectRunMode,
    /// Whether this project remains registered with the supervisor.
    pub active: bool,
    /// Process and lifecycle bookkeeping owned by the project daemon.
    pub daemon: HandoffDaemonState,
}

/// Complete workload-ownership record serialized before supervisor re-exec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorHandoff {
    /// Handoff schema used to decode this record.
    pub schema: u16,
    /// Live re-execution protocol used by both binaries.
    pub protocol: u16,
    /// Canonical previous binary used for rollback.
    pub source_binary: PathBuf,
    /// Resident version restored when replacement initialization fails.
    pub source_version: Version,
    /// Version expected from the replacement binary.
    pub target_version: Version,
    /// Replacement initialization error retained across rollback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_reason: Option<String>,
    /// Inherited supervisor runtime-lock descriptor.
    pub lock_fd: i32,
    /// Inherited control-listener descriptor.
    pub listener_fd: i32,
    /// Optional single-service filter used for the original boot.
    pub service_filter: Option<String>,
    /// Whether service stderr was forwarded to supervisor stdout.
    pub pipe_stderr: bool,
    /// Primary project retained even when currently stopped.
    pub primary: HandoffProject,
    /// Additional registered projects keyed by stable project id.
    pub projects: BTreeMap<String, HandoffProject>,
    /// Service-output descriptors retained across re-exec.
    pub log_pipes: Vec<HandoffLogPipe>,
}

impl SupervisorHandoff {
    /// Persists this handoff in the private runtime directory and syncs it before
    /// the supervisor executes the replacement binary.
    pub fn persist(&self) -> io::Result<PathBuf> {
        let directory = runtime::state_dir();
        runtime::create_private_dir(&directory)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = directory.join(format!(
            "upgrade-handoff-{}-{stamp}.json",
            std::process::id()
        ));
        self.write_to(&path)?;
        Ok(path)
    }

    /// Replaces and syncs the contents of an existing private handoff record.
    pub fn write_to(&self, path: &Path) -> io::Result<()> {
        let encoded = serde_json::to_vec(self).map_err(io::Error::other)?;
        runtime::write_private_file(path, encoded)?;
        fs::OpenOptions::new().read(true).open(path)?.sync_all()
    }

    /// Loads and validates a private handoff record.
    pub fn load(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() || metadata.mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "supervisor handoff file is not private",
            ));
        }
        let handoff: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        if handoff.schema != HANDOFF_SCHEMA_VERSION
            || handoff.protocol != LIVE_REEXEC_PROTOCOL
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported handoff schema {} or protocol {}",
                    handoff.schema, handoff.protocol
                ),
            ));
        }
        Ok(handoff)
    }
}

/// Re-executes the previous supervisor binary after replacement initialization
/// failed, retaining the same descriptors and handoff record.
pub fn rollback_handoff(path: &Path, reason: impl Into<String>) -> io::Result<()> {
    use std::ffi::CString;

    let mut state = SupervisorHandoff::load(path)?;
    state.target_version = state.source_version.clone();
    state.rollback_reason = Some(reason.into());
    state.write_to(path)?;
    let values = [
        state.source_binary.to_string_lossy().to_string(),
        "supervise".to_string(),
        "--config".to_string(),
        state.primary.config_path.to_string_lossy().to_string(),
        "--handoff".to_string(),
        path.to_string_lossy().to_string(),
    ];
    let args = values
        .iter()
        .map(|value| {
            CString::new(value.as_str()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "rollback argument contains a NUL byte",
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    nix::unistd::execv(&args[0], &args)
        .map(|_| ())
        .map_err(io::Error::other)
}

impl UpgradeTarget {
    /// Validates the candidate file, probes its protocol, and checks patch-line
    /// compatibility with the resident supervisor.
    pub fn inspect(
        path: &Path,
        current: &LiveUpgradeInfo,
    ) -> Result<Self, Box<Diagnostic>> {
        let canonical = trusted_executable(path)?;
        let info = probe_target(&canonical)?;
        validate_compatibility(current, &info)?;
        Ok(Self {
            path: canonical,
            info,
        })
    }
}

/// Builds SG0503 for runtime activity that prevents a stable handoff.
pub fn environment_unsafe(reason: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        SgCode::UpgradeEnvironmentUnsafe,
        "the supervisor is not ready for a live upgrade",
    )
    .note(reason)
    .help_cmd("inspect current work", "sysg status")
    .help_docs()
}

/// Builds SG0504 for a failure before the replacement process image takes over.
pub fn handoff_failed(reason: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        SgCode::UpgradeHandoffFailed,
        "the supervisor could not hand off its runtime",
    )
    .note(reason)
    .help_cmd("read the supervisor log", "sysg logs --supervisor")
    .help_docs()
}

/// Builds SG0505 for a replacement that could not restore handed-off state.
pub fn resume_failed(reason: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        SgCode::UpgradeResumeFailed,
        "the replacement supervisor could not resume the runtime",
    )
    .note(reason)
    .help_cmd("read the supervisor log", "sysg logs --supervisor")
    .help_docs()
}

/// Verifies that a resident release can understand a staged target's live
/// upgrade request before the client sends the new IPC variant.
pub fn validate_resident_version(
    resident: &str,
    target: &LiveUpgradeInfo,
) -> Result<Version, Box<Diagnostic>> {
    let resident = Version::parse(resident).map_err(|err| {
        incompatible(format!(
            "resident supervisor reported invalid version `{resident}`: {err}"
        ))
    })?;
    if resident.major != target.version.major
        || resident.minor != target.version.minor
        || resident >= target.version
    {
        return Err(incompatible(format!(
            "resident {resident} cannot live-upgrade to {}",
            target.version
        )));
    }
    Ok(resident)
}

/// Resolves and verifies the filesystem trust boundary for a candidate binary.
fn trusted_executable(path: &Path) -> Result<PathBuf, Box<Diagnostic>> {
    let canonical = fs::canonicalize(path).map_err(|err| {
        target_invalid(format!("could not resolve `{}`: {err}", path.display()))
    })?;
    let metadata = fs::metadata(&canonical).map_err(|err| {
        target_invalid(format!(
            "could not inspect `{}`: {err}",
            canonical.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(target_invalid(format!(
            "`{}` is not a regular file",
            canonical.display()
        )));
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(target_invalid(format!(
            "`{}` is not executable",
            canonical.display()
        )));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(target_invalid(format!(
            "`{}` is writable by its group or other users",
            canonical.display()
        )));
    }
    let owner = metadata.uid();
    let current = unsafe { libc::geteuid() };
    if owner != current && owner != 0 {
        return Err(target_invalid(format!(
            "`{}` is owned by uid {owner}, not uid {current} or root",
            canonical.display()
        )));
    }
    Ok(canonical)
}

/// Executes the candidate's metadata command within a bounded interval.
fn probe_target(path: &Path) -> Result<LiveUpgradeInfo, Box<Diagnostic>> {
    let mut child = Command::new(path)
        .arg("upgrade-info")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| target_invalid(format!("could not execute candidate: {err}")))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < TARGET_PROBE_TIMEOUT => {
                thread::sleep(TARGET_PROBE_INTERVAL);
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(target_invalid(format!(
                    "candidate did not report metadata within {}s",
                    TARGET_PROBE_TIMEOUT.as_secs()
                )));
            }
            Err(err) => {
                return Err(target_invalid(format!(
                    "could not wait for candidate metadata: {err}"
                )));
            }
        }
    };
    if !status.success() {
        return Err(target_invalid(format!(
            "candidate metadata command exited with {status}"
        )));
    }
    let mut output = String::new();
    child
        .stdout
        .take()
        .ok_or_else(|| target_invalid("candidate metadata output was unavailable"))?
        .read_to_string(&mut output)
        .map_err(|err| {
            target_invalid(format!("could not read candidate metadata: {err}"))
        })?;
    serde_json::from_str(output.trim())
        .map_err(|err| target_invalid(format!("candidate metadata was invalid: {err}")))
}

/// Checks protocol equality and patch-line version ordering.
fn validate_compatibility(
    current: &LiveUpgradeInfo,
    target: &LiveUpgradeInfo,
) -> Result<(), Box<Diagnostic>> {
    if target.protocol != current.protocol {
        return Err(incompatible(format!(
            "live-reexec protocol {} cannot hand off to protocol {}",
            current.protocol, target.protocol
        )));
    }
    if target.version.major != current.version.major
        || target.version.minor != current.version.minor
    {
        return Err(incompatible(format!(
            "live upgrade supports patch releases within {}.{}, not {}",
            current.version.major, current.version.minor, target.version
        )));
    }
    if target.version <= current.version {
        return Err(incompatible(format!(
            "target {} must be newer than resident {}",
            target.version, current.version
        )));
    }
    Ok(())
}

/// Builds SG0501 for an invalid or untrusted executable candidate.
fn target_invalid(reason: impl Into<String>) -> Box<Diagnostic> {
    Box::new(
        Diagnostic::error(
            SgCode::UpgradeTargetInvalid,
            "the upgrade target is not a trusted sysg executable",
        )
        .note(reason)
        .help_docs(),
    )
}

/// Builds SG0502 for a candidate outside the live-upgrade compatibility line.
fn incompatible(reason: impl Into<String>) -> Box<Diagnostic> {
    Box::new(
        Diagnostic::error(
            SgCode::UpgradeIncompatible,
            "the upgrade target is not live-reexec compatible",
        )
        .note(reason)
        .help_docs(),
    )
}
