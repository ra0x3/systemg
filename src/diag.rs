//! rustc-style diagnostics for sysg.
//!
//! Every user-facing failure renders as a [`Diagnostic`]: what happened, the
//! evidence sysg captured while it happened, and the exact next commands to
//! run — colored on a terminal, plain when piped, structured over IPC. The
//! design goal is the Rust compiler's: assume the user made an honest mistake
//! and hand it back with a map, not a dead end.
//!
//! Every diagnostic carries a typed [`SgCode`]. The enum *is* the error
//! taxonomy: a code that isn't a variant cannot be constructed, and adding a
//! failure mode means adding a variant. There is no stringly-typed seam.

use std::{fmt, io::IsTerminal};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Base URL for per-code documentation pages.
pub const DOCS_BASE: &str = "https://docs.sysg.dev/errors";

const RED: &str = "\x1b[1;31m";
const YELLOW: &str = "\x1b[1;33m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const UNDERLINE: &str = "\x1b[4;34m";
const RESET: &str = "\x1b[0m";

/// The stable sysg error taxonomy. Each variant owns its `SG####` string, a
/// canonical one-line title, and its docs slug; the wire form is the code
/// string so scripts and IPC stay stable across renames of the Rust variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SgCode {
    /// SG0001 — a failure with no more specific diagnosis yet.
    Catchall,
    /// SG0002 — cron history/active state could not be restored.
    CronStateRecoveryFailed,
    /// SG0003 — a cron unit could not be safely registered.
    CronRegistrationConflict,
    /// SG0004 — a finite unit that exited cleanly was misclassified as failed.
    FiniteUnitMisclassified,
    /// SG0005 — the supervisor is using an outdated or wrong manifest.
    StaleProjectConfiguration,
    /// SG0006 — a command resolves ambiguously between projects/services.
    TargetScopeAmbiguous,
    /// SG0007 — the supervisor cannot safely restart or transfer ownership.
    SupervisorRestartConflict,
    /// SG0008 — a service or `pre_start` failed during boot or restart.
    UnitStartFailed,
    /// SG0009 — status/inspect disagrees with live process state.
    StatusStateInconsistent,
    /// SG0010 — expected service logs are unavailable or misrouted.
    LogSourceUnavailable,
    /// SG0011 — a live log-follow session is stale or cannot reconnect.
    LogStreamDesynchronized,
    /// SG0012 — log output exceeded safe storage/display bounds.
    LogLimitExceeded,
    /// SG0013 — a daemonized service inherited an invalid environment.
    DaemonEnvironmentInvalid,
    /// SG0014 — the installer cannot obtain the expected binary.
    ReleaseArtifactUnavailable,
    /// SG0015 — IPC/PID/tracking disagrees with the running processes.
    SupervisorStateDesynchronized,
    /// SG0016 — a rolling deployment failed without a useful error.
    RollingDeploymentFailed,
    /// SG0102 — a service exited immediately at start, before it came up.
    UnitImmediateExit,
    /// SG0103 — a service's `pre_start` failed, so it was not started.
    PreStartFailed,
    /// SG0104 — a service never passed its configured health check.
    HealthUnmet,
    /// SG0201 — the `-p` project does not match the resolved config.
    TargetConfigMismatch,
    /// SG0202 — the command names a service or project that does not exist.
    TargetNotFound,
    /// SG0203 — a config file could not be found or read.
    ConfigFileUnreadable,
    /// SG0204 — mutually exclusive selectors were combined (e.g. --supervisor
    /// with a service/project selector).
    ConflictingSelectors,
    /// SG0301 — a restart's new manifest is invalid; nothing was changed.
    ManifestRejected,
    /// SG0302 — a reconcile ran but left units short of their manifest target.
    ReconcileIncomplete,
    /// SG0303 — a supervisor recycle stopped the old daemon but the new one did
    /// not come up.
    SupervisorRecycleFailed,
}

impl SgCode {
    /// The stable `SG####` string. This is the wire and docs identity.
    pub fn as_str(self) -> &'static str {
        match self {
            SgCode::Catchall => "SG0001",
            SgCode::CronStateRecoveryFailed => "SG0002",
            SgCode::CronRegistrationConflict => "SG0003",
            SgCode::FiniteUnitMisclassified => "SG0004",
            SgCode::StaleProjectConfiguration => "SG0005",
            SgCode::TargetScopeAmbiguous => "SG0006",
            SgCode::SupervisorRestartConflict => "SG0007",
            SgCode::UnitStartFailed => "SG0008",
            SgCode::StatusStateInconsistent => "SG0009",
            SgCode::LogSourceUnavailable => "SG0010",
            SgCode::LogStreamDesynchronized => "SG0011",
            SgCode::LogLimitExceeded => "SG0012",
            SgCode::DaemonEnvironmentInvalid => "SG0013",
            SgCode::ReleaseArtifactUnavailable => "SG0014",
            SgCode::SupervisorStateDesynchronized => "SG0015",
            SgCode::RollingDeploymentFailed => "SG0016",
            SgCode::UnitImmediateExit => "SG0102",
            SgCode::PreStartFailed => "SG0103",
            SgCode::HealthUnmet => "SG0104",
            SgCode::TargetConfigMismatch => "SG0201",
            SgCode::TargetNotFound => "SG0202",
            SgCode::ConfigFileUnreadable => "SG0203",
            SgCode::ConflictingSelectors => "SG0204",
            SgCode::ManifestRejected => "SG0301",
            SgCode::ReconcileIncomplete => "SG0302",
            SgCode::SupervisorRecycleFailed => "SG0303",
        }
    }

    /// The docs URL for this code.
    pub fn docs_url(self) -> String {
        format!("{DOCS_BASE}/{}", self.as_str())
    }

    /// Every code, so callers can enumerate or round-trip the taxonomy.
    pub const ALL: [SgCode; 26] = [
        SgCode::Catchall,
        SgCode::CronStateRecoveryFailed,
        SgCode::CronRegistrationConflict,
        SgCode::FiniteUnitMisclassified,
        SgCode::StaleProjectConfiguration,
        SgCode::TargetScopeAmbiguous,
        SgCode::SupervisorRestartConflict,
        SgCode::UnitStartFailed,
        SgCode::StatusStateInconsistent,
        SgCode::LogSourceUnavailable,
        SgCode::LogStreamDesynchronized,
        SgCode::LogLimitExceeded,
        SgCode::DaemonEnvironmentInvalid,
        SgCode::ReleaseArtifactUnavailable,
        SgCode::SupervisorStateDesynchronized,
        SgCode::RollingDeploymentFailed,
        SgCode::UnitImmediateExit,
        SgCode::PreStartFailed,
        SgCode::HealthUnmet,
        SgCode::TargetConfigMismatch,
        SgCode::TargetNotFound,
        SgCode::ConfigFileUnreadable,
        SgCode::ConflictingSelectors,
        SgCode::ManifestRejected,
        SgCode::ReconcileIncomplete,
        SgCode::SupervisorRecycleFailed,
    ];
}

impl fmt::Display for SgCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The wire form does not resolve to a known code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSgCode(pub String);

impl fmt::Display for UnknownSgCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown sysg code `{}`", self.0)
    }
}

impl std::error::Error for UnknownSgCode {}

impl std::str::FromStr for SgCode {
    type Err = UnknownSgCode;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|c| c.as_str() == code)
            .ok_or_else(|| UnknownSgCode(code.to_string()))
    }
}

impl Serialize for SgCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SgCode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// How severe a diagnostic is; controls the header color and label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    /// The operation failed.
    Error,
    /// The operation proceeded but something is off.
    Warning,
    /// Informational context.
    Note,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Severity::Error => RED,
            Severity::Warning => YELLOW,
            Severity::Note => CYAN,
        }
    }
}

/// Where in the user's world the problem originates, e.g. a config file key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    /// Path to the file, as the user knows it.
    pub file: String,
    /// 1-indexed line, when resolvable.
    pub line: Option<usize>,
    /// Dotted key path inside the file, e.g. `services.api.health_check`.
    pub key: Option<String>,
}

/// A labeled block of captured facts, e.g. the service's last output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Short label, e.g. `last output`.
    pub label: String,
    /// The captured lines, already trimmed to a reasonable count.
    pub lines: Vec<String>,
}

/// An actionable next step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Help {
    /// A command the user can run, with a short reason.
    Command {
        /// What running it shows or does, e.g. `view logs`.
        label: String,
        /// The exact command line.
        cmd: String,
    },
    /// A documentation link.
    Link {
        /// The URL.
        url: String,
    },
}

/// A structured, renderable failure report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Severity of the report.
    pub severity: Severity,
    /// Stable sysg error code.
    pub code: SgCode,
    /// One-line statement of what happened.
    pub title: String,
    /// Where the problem originates, when known.
    pub origin: Option<Origin>,
    /// Plain-sentence facts about what sysg observed.
    pub notes: Vec<String>,
    /// Captured output blocks.
    pub evidence: Vec<Evidence>,
    /// Next steps.
    pub help: Vec<Help>,
}

impl Diagnostic {
    /// Starts an error-severity diagnostic with the given code and title.
    pub fn error(code: SgCode, title: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            title: title.into(),
            origin: None,
            notes: Vec::new(),
            evidence: Vec::new(),
            help: Vec::new(),
        }
    }

    /// The stable `SG####` string for this diagnostic's code.
    pub fn code_str(&self) -> &'static str {
        self.code.as_str()
    }

    /// Attaches the originating file/key.
    pub fn origin(
        mut self,
        file: impl Into<String>,
        line: Option<usize>,
        key: Option<String>,
    ) -> Self {
        self.origin = Some(Origin {
            file: file.into(),
            line,
            key,
        });
        self
    }

    /// Adds a plain-sentence observation.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Adds a labeled block of captured lines.
    pub fn evidence(mut self, label: impl Into<String>, lines: Vec<String>) -> Self {
        if !lines.is_empty() {
            self.evidence.push(Evidence {
                label: label.into(),
                lines,
            });
        }
        self
    }

    /// Adds a runnable next step.
    pub fn help_cmd(mut self, label: impl Into<String>, cmd: impl Into<String>) -> Self {
        self.help.push(Help::Command {
            label: label.into(),
            cmd: cmd.into(),
        });
        self
    }

    /// Adds the documentation link for this diagnostic's code.
    pub fn help_docs(mut self) -> Self {
        self.help.push(Help::Link {
            url: self.code.docs_url(),
        });
        self
    }

    /// Renders with ANSI colors when appropriate for stderr.
    pub fn render_for_terminal(&self) -> String {
        let color =
            std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        self.render(color)
    }

    /// Renders the diagnostic; `color` toggles ANSI escapes.
    pub fn render(&self, color: bool) -> String {
        let paint = |code: &'static str| if color { code } else { "" };
        let reset = paint(RESET);
        let mut out = String::new();

        out.push_str(&format!(
            "{}{}[{}]{}{}: {}{}\n",
            paint(self.severity.color()),
            self.severity.label(),
            self.code.as_str(),
            reset,
            paint(BOLD),
            self.title,
            reset,
        ));

        if let Some(origin) = &self.origin {
            let mut place = origin.file.clone();
            if let Some(line) = origin.line {
                place.push_str(&format!(":{line}"));
            }
            if let Some(key) = &origin.key {
                place.push_str(&format!(" ({key})"));
            }
            out.push_str(&format!("  {}-->{} {}\n", paint(CYAN), reset, place));
        }

        if !self.notes.is_empty() {
            out.push('\n');
            for note in &self.notes {
                out.push_str(&format!("  {note}\n"));
            }
        }

        for block in &self.evidence {
            out.push('\n');
            out.push_str(&format!("  {}{}:{}\n", paint(DIM), block.label, reset));
            for line in &block.lines {
                out.push_str(&format!("  {}\u{2502}{} {}\n", paint(DIM), reset, line));
            }
        }

        if !self.help.is_empty() {
            out.push('\n');
            out.push_str(&format!("  {}help:{}\n", paint(GREEN), reset));
            let width = self
                .help
                .iter()
                .map(|h| match h {
                    Help::Command { label, .. } => label.len(),
                    Help::Link { .. } => 4,
                })
                .max()
                .unwrap_or(0);
            for help in &self.help {
                match help {
                    Help::Command { label, cmd } => {
                        out.push_str(&format!(
                            "    {label:<width$}  {}{}{}\n",
                            paint(BOLD),
                            cmd,
                            reset,
                        ));
                    }
                    Help::Link { url } => {
                        out.push_str(&format!(
                            "    {:<width$}  {}{}{}\n",
                            "docs",
                            paint(UNDERLINE),
                            url,
                            reset,
                        ));
                    }
                }
            }
        }

        out
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.render(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Diagnostic {
        Diagnostic::error(
            SgCode::HealthUnmet,
            "service `api` failed to become healthy",
        )
        .origin(
            "sysg.yaml",
            Some(31),
            Some("services.api.health_check".into()),
        )
        .note("5 health checks failed over 45s")
        .evidence(
            "last output",
            vec!["password authentication failed".to_string()],
        )
        .help_cmd("view logs", "sysg logs -s api")
        .help_docs()
    }

    #[test]
    fn plain_render_has_code_title_evidence_and_help() {
        let text = sample().render(false);
        assert!(text.contains("error[SG0104]"));
        assert!(text.contains("failed to become healthy"));
        assert!(text.contains("--> sysg.yaml:31 (services.api.health_check)"));
        assert!(text.contains("password authentication failed"));
        assert!(text.contains("sysg logs -s api"));
        assert!(text.contains(&format!("{DOCS_BASE}/SG0104")));
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn colored_render_uses_ansi_and_survives_roundtrip() {
        let text = sample().render(true);
        assert!(text.contains(RED));
        let json = serde_json::to_string(&sample()).unwrap();
        let back: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(back.code, SgCode::HealthUnmet);
        assert_eq!(back.render(false), sample().render(false));
    }

    #[test]
    fn code_strings_are_unique_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for code in SgCode::ALL {
            assert!(
                seen.insert(code.as_str()),
                "duplicate code {}",
                code.as_str()
            );
            assert_eq!(code.as_str().parse(), Ok(code));
        }
    }

    #[test]
    fn unknown_code_fails_to_deserialize() {
        assert!(serde_json::from_str::<SgCode>("\"SG9999\"").is_err());
    }
}
