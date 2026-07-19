//! Validation and diagnostics for workload-preserving supervisor upgrades.

use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::diag::{Diagnostic, SgCode};

/// Wire protocol implemented by supervisors that support live re-execution.
pub const LIVE_REEXEC_PROTOCOL: u16 = 1;

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
