//! Configuration validation with human-friendly diagnostics.
//!
//! Parses a manifest and, on failure, maps common error signatures to a
//! plain-language explanation, a suggested fix, and a docs link. Rendering is
//! left to the caller so it can respect color and output-format flags.

use std::{fs, path::Path};

use serde::Serialize;

use crate::{
    config::{load_config, parse_config_manifest},
    error::ProcessManagerError,
};

/// Base URL for documentation links surfaced in diagnostics.
const DOCS: &str = "https://sysg.dev";

/// A single validation problem with location and remediation guidance.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    /// 1-based line the problem points at, when known.
    pub line: Option<usize>,
    /// 1-based column the problem points at, when known.
    pub column: Option<usize>,
    /// Short machine-readable category (e.g. `missing-version`).
    pub kind: String,
    /// The raw error message describing what failed.
    pub message: String,
    /// Plain-language explanation of why this is an error.
    pub why: String,
    /// Concrete suggested fix.
    pub suggestion: String,
    /// Documentation link for further reading.
    pub doc: String,
}

/// The outcome of validating a configuration file.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    /// Path that was validated.
    pub config: String,
    /// Whether the configuration parsed and resolved cleanly.
    pub valid: bool,
    /// Zero or more diagnostics collected during validation.
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    fn ok(config: &str) -> Self {
        Self {
            config: config.to_string(),
            valid: true,
            diagnostics: Vec::new(),
        }
    }

    fn failed(config: &str, diagnostic: Diagnostic) -> Self {
        Self {
            config: config.to_string(),
            valid: false,
            diagnostics: vec![diagnostic],
        }
    }
}

/// Reads and validates the configuration at `path`, returning a report and the
/// file contents (when readable) so callers can render annotated snippets.
pub fn validate(path: &str) -> (ValidationReport, Option<String>) {
    let content = match fs::read_to_string(Path::new(path)) {
        Ok(content) => content,
        Err(err) => {
            let diagnostic = Diagnostic {
                line: None,
                column: None,
                kind: "unreadable-config".into(),
                message: err.to_string(),
                why: format!(
                    "systemg could not open '{path}', so there is nothing to validate."
                ),
                suggestion:
                    "Check the path and permissions, or pass -c <file> to point at your manifest."
                        .into(),
                doc: format!("{DOCS}/how-it-works/commands/validate"),
            };
            return (ValidationReport::failed(path, diagnostic), None);
        }
    };

    if let Err(err) = parse_config_manifest(&content) {
        let diagnostic = classify_yaml(&err);
        return (ValidationReport::failed(path, diagnostic), Some(content));
    }

    match load_config(Some(path)) {
        Ok(_) => (ValidationReport::ok(path), Some(content)),
        Err(err) => {
            let diagnostic = classify_semantic(&err);
            (ValidationReport::failed(path, diagnostic), Some(content))
        }
    }
}

/// Maps a resolved-config error (dependency graph, env expansion) to a
/// diagnostic. These surface only after the manifest parses as valid YAML.
fn classify_semantic(err: &ProcessManagerError) -> Diagnostic {
    let message = err.to_string();
    let (kind, why, suggestion, doc) = match err {
        ProcessManagerError::UnknownDependency { .. } => (
            "unknown-dependency",
            "A service lists a `depends_on` entry that no service in this manifest defines.",
            "Fix the typo, or add the missing service so the dependency resolves.",
            "/how-it-works/configuration",
        ),
        ProcessManagerError::DependencyCycle { .. } => (
            "dependency-cycle",
            "Services depend on each other in a loop, so no valid start order exists.",
            "Break the cycle by removing one of the `depends_on` edges in the loop.",
            "/how-it-works/configuration",
        ),
        ProcessManagerError::MissingEnvVar(_) => (
            "missing-env-var",
            "The config interpolates a `${VAR}` that is not set in the environment or env file.",
            "Export the variable, add it to your env file, or set it under `env.vars`.",
            "/how-it-works/configuration",
        ),
        ProcessManagerError::ConfigParseError(inner) => return classify_yaml(inner),
        _ => (
            "invalid-config",
            "The manifest parsed as YAML but failed systemg's semantic checks.",
            "Review the message below and the referenced docs for the offending field.",
            "/how-it-works/configuration",
        ),
    };

    Diagnostic {
        line: None,
        column: None,
        kind: kind.into(),
        message,
        why: why.into(),
        suggestion: suggestion.into(),
        doc: format!("{DOCS}{doc}"),
    }
}

/// Maps a YAML/schema parse error to a diagnostic with a curated fix.
fn classify_yaml(err: &serde_yaml::Error) -> Diagnostic {
    let message = err.to_string();
    let location = err.location();
    let line = location.as_ref().map(|loc| loc.line());
    let column = location.as_ref().map(|loc| loc.column());
    let lower = message.to_lowercase();

    let (kind, why, suggestion, doc) = if lower.contains("missing field `version`") {
        (
            "missing-version",
            "Every manifest must declare its schema version at the top level.",
            "Add `version: \"2\"` as the first key in the file.",
            "/how-it-works/configuration",
        )
    } else if lower.contains("unsupported manifest version")
        || lower.contains("no longer supported")
    {
        (
            "unsupported-version",
            "The declared version is not one systemg knows how to read.",
            "Set `version: \"2\"` — the current supported schema version.",
            "/how-it-works/configuration",
        )
    } else if lower.contains("missing field `command`") {
        (
            "missing-command",
            "Each service needs a command telling systemg what process to run.",
            "Add a `command:` line under the service (e.g. `command: \"./run.sh\"`).",
            "/how-it-works/configuration",
        )
    } else if lower.contains("missing field `services`") {
        (
            "missing-services",
            "A manifest with no `services` map has nothing to supervise.",
            "Add a `services:` block with at least one named service.",
            "/how-it-works/configuration",
        )
    } else if lower.contains("health check requires at least one") {
        (
            "invalid-health-check",
            "A health check must probe something: either an HTTP url or a command.",
            "Give the health_check a `url:` or a `command:` (plus optional interval/timeout/retries).",
            "/how-it-works/configuration",
        )
    } else if lower.contains("project.id") {
        (
            "invalid-project-id",
            "The project id is the durable namespace for this stack's runtime state.",
            "Use a non-empty id of ASCII letters, numbers, '_', '-', or '.'.",
            "/how-it-works/state",
        )
    } else {
        (
            "invalid-yaml",
            "systemg could not parse this file as a valid v1 manifest.",
            "Check the highlighted line for indentation, quoting, or an unexpected key.",
            "/how-it-works/configuration",
        )
    };

    Diagnostic {
        line,
        column,
        kind: kind.into(),
        message,
        why: why.into(),
        suggestion: suggestion.into(),
        doc: format!("{DOCS}{doc}"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;

    fn write_config(contents: &str) -> (tempfile::TempDir, String) {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("systemg.yaml");
        let mut file = fs::File::create(&path).expect("create");
        file.write_all(contents.as_bytes()).expect("write");
        (dir, path.to_string_lossy().to_string())
    }

    #[test]
    fn valid_config_reports_ok() {
        let (_dir, path) =
            write_config("version: \"2\"\nservices:\n  api:\n    command: \"echo ok\"\n");
        let (report, content) = validate(&path);
        assert!(report.valid);
        assert!(report.diagnostics.is_empty());
        assert!(content.is_some());
    }

    #[test]
    fn missing_version_is_classified() {
        let (_dir, path) = write_config("services:\n  api:\n    command: \"echo ok\"\n");
        let (report, _) = validate(&path);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "missing-version");
    }

    #[test]
    fn unsupported_version_is_classified() {
        let (_dir, path) =
            write_config("version: \"3\"\nservices:\n  api:\n    command: \"echo ok\"\n");
        let (report, _) = validate(&path);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "unsupported-version");
    }

    #[test]
    fn bad_health_check_is_classified() {
        let (_dir, path) = write_config(
            "version: \"2\"\nservices:\n  api:\n    command: \"echo ok\"\n    deployment:\n      health_check:\n        interval: \"2s\"\n",
        );
        let (report, _) = validate(&path);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "invalid-health-check");
    }

    #[test]
    fn unreadable_config_is_reported() {
        let (report, content) = validate("/nonexistent/path/systemg.yaml");
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "unreadable-config");
        assert!(content.is_none());
    }

    #[test]
    fn unknown_dependency_is_classified() {
        let (_dir, path) = write_config(
            "version: \"2\"\nservices:\n  api:\n    command: \"echo ok\"\n    depends_on: [missing]\n",
        );
        let (report, _) = validate(&path);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "unknown-dependency");
    }

    #[test]
    fn dependency_cycle_is_classified() {
        let (_dir, path) = write_config(
            "version: \"2\"\nservices:\n  a:\n    command: \"x\"\n    depends_on: [b]\n  b:\n    command: \"y\"\n    depends_on: [a]\n",
        );
        let (report, _) = validate(&path);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "dependency-cycle");
    }

    #[test]
    fn location_is_captured_for_syntax_errors() {
        let (_dir, path) = write_config(
            "version: \"2\"\nservices:\n  api:\n   command: \"x\"\n  bad: [unclosed\n",
        );
        let (report, _) = validate(&path);
        assert!(!report.valid);
        assert!(report.diagnostics[0].line.is_some());
    }
}
