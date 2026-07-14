//! rustc-style diagnostics for sysg.
//!
//! Every user-facing failure renders as a [`Diagnostic`]: what happened, the
//! evidence sysg captured while it happened, and the exact next commands to
//! run — colored on a terminal, plain when piped, structured over IPC. The
//! design goal is the Rust compiler's: assume the user made an honest mistake
//! and hand it back with a map, not a dead end.

use std::{fmt, io::IsTerminal};

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Origin {
    /// Path to the file, as the user knows it.
    pub file: String,
    /// 1-indexed line, when resolvable.
    pub line: Option<usize>,
    /// Dotted key path inside the file, e.g. `services.api.health_check`.
    pub key: Option<String>,
}

/// A labeled block of captured facts, e.g. the service's last output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// Short label, e.g. `last output`.
    pub label: String,
    /// The captured lines, already trimmed to a reasonable count.
    pub lines: Vec<String>,
}

/// An actionable next step.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Severity of the report.
    pub severity: Severity,
    /// Stable sysg error code, e.g. `SG0104`.
    pub code: String,
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
    pub fn error(code: &str, title: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: code.to_string(),
            title: title.into(),
            origin: None,
            notes: Vec::new(),
            evidence: Vec::new(),
            help: Vec::new(),
        }
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
        let url = format!("{DOCS_BASE}/{}", self.code);
        self.help.push(Help::Link { url });
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
            self.code,
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
        Diagnostic::error("SG0104", "service `api` failed to become healthy")
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
        assert_eq!(back.code, "SG0104");
        assert_eq!(back.render(false), sample().render(false));
    }
}
