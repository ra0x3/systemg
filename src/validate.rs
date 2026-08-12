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
    /// Severity of the finding: `error` fails validation, `warning` does not.
    pub severity: String,
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
    /// Whether the validated mode could actually start this manifest.
    pub startable: bool,
    /// Zero or more diagnostics collected during validation.
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    fn failed(config: &str, diagnostic: Diagnostic) -> Self {
        Self {
            config: config.to_string(),
            valid: false,
            startable: false,
            diagnostics: vec![diagnostic],
        }
    }
}

/// Host facts the enforceability pass consults, injectable so the evaluator
/// stays pure and unit-testable. `probe()` captures the real host.
pub struct HostFacts {
    /// Whether a named user exists on this host.
    pub user_exists: fn(&str) -> bool,
    /// Whether a named group exists on this host.
    pub group_exists: fn(&str) -> bool,
    /// Whether this build can enforce Linux-only keys at all.
    pub linux: bool,
}

impl HostFacts {
    /// Captures the running host.
    pub fn probe() -> Self {
        Self {
            user_exists: |name| {
                nix::unistd::User::from_name(name).ok().flatten().is_some()
            },
            group_exists: |name| {
                nix::unistd::Group::from_name(name).ok().flatten().is_some()
            },
            linux: cfg!(target_os = "linux"),
        }
    }
}

/// Keys a service declares that only a root (`--sys`) supervisor can enforce.
/// Only effective requests count: empty lists, `false` booleans, and absent
/// profiles are not privileged asks.
fn root_required_keys(service: &crate::config::ServiceConfig) -> Vec<&'static str> {
    let mut keys = Vec::new();
    if service.user.is_some() {
        keys.push("user");
    }
    if service.group.is_some() {
        keys.push("group");
    }
    if service
        .supplementary_groups
        .as_ref()
        .is_some_and(|g| !g.is_empty())
    {
        keys.push("supplementary_groups");
    }
    if service.capabilities.as_ref().is_some_and(|c| !c.is_empty()) {
        keys.push("capabilities");
    }
    if service.limits.as_ref().is_some_and(|l| l.cgroup.is_some()) {
        keys.push("limits.cgroup");
    }
    if let Some(iso) = &service.isolation {
        let effective = iso.network.unwrap_or(false)
            || iso.mount.unwrap_or(false)
            || iso.pid.unwrap_or(false)
            || iso.user.unwrap_or(false)
            || iso.private_devices.unwrap_or(false)
            || iso.private_tmp.unwrap_or(false)
            || iso.seccomp.as_ref().is_some_and(|v| !v.is_empty())
            || iso.apparmor_profile.as_ref().is_some_and(|v| !v.is_empty())
            || iso.selinux_context.as_ref().is_some_and(|v| !v.is_empty());
        if effective {
            keys.push("isolation");
        }
    }
    keys
}

/// Linux-only among the root-required keys: unenforceable on other platforms
/// no matter the privilege level.
fn linux_only(key: &str) -> bool {
    matches!(key, "capabilities" | "limits.cgroup" | "isolation")
}

/// Mode-enforceability pass: a manifest can be well-formed yet not startable
/// in the mode validate was asked about. User mode: root-requiring keys are
/// error findings (`start` would refuse, SG0705). System mode: they pass;
/// named accounts missing on this host are warnings (the target host may
/// differ), and Linux-only keys on a non-Linux build stay errors.
pub fn mode_findings(
    config: &crate::config::Config,
    system_mode: bool,
    host: &HostFacts,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    let mut names: Vec<&String> = config.services.keys().collect();
    names.sort();
    for name in names {
        let service = &config.services[name];
        let keys = root_required_keys(service);
        if keys.is_empty() {
            continue;
        }
        if !system_mode {
            findings.push(Diagnostic {
                severity: "error".into(),
                line: None,
                column: None,
                kind: "requires-system-mode".into(),
                message: format!(
                    "service '{name}' declares {} — root-only, not startable in user mode (SG0705)",
                    keys.join(", ")
                ),
                why: "These keys need a root supervisor: user mode cannot switch users, grant capabilities, or attach cgroups/namespaces.".into(),
                suggestion: "Check against the system runtime with `sysg validate --sys`; start with `sudo sysg --sys start ...` — or remove the root-only keys.".into(),
                doc: format!("{DOCS}/how-it-works/dialog/codes#sg0705"),
            });
            continue;
        }
        if !host.linux {
            let blocked: Vec<&str> =
                keys.iter().copied().filter(|k| linux_only(k)).collect();
            if !blocked.is_empty() {
                findings.push(Diagnostic {
                    severity: "error".into(),
                    line: None,
                    column: None,
                    kind: "linux-only-keys".into(),
                    message: format!(
                        "service '{name}' declares {} — Linux-only, unenforceable on this platform even with --sys",
                        blocked.join(", ")
                    ),
                    why: "Capabilities, cgroups, and namespace isolation are Linux kernel mechanisms; start on this platform refuses them.".into(),
                    suggestion: "Validate and run this manifest on a Linux host, or remove the Linux-only keys.".into(),
                    doc: format!("{DOCS}/kernel-mode/sandboxing"),
                });
            }
        }
        for (kind, exists, account) in [
            ("user", host.user_exists, service.user.clone()),
            ("group", host.group_exists, service.group.clone()),
        ] {
            if let Some(account) = account
                && !exists(&account)
            {
                findings.push(Diagnostic {
                    severity: "warning".into(),
                    line: None,
                    column: None,
                    kind: format!("unknown-{kind}"),
                    message: format!(
                        "service '{name}' declares {kind} '{account}', not found on this host"
                    ),
                    why: "Validation ran on this machine; if it is also the deploy target, start will fail to resolve the account.".into(),
                    suggestion: format!(
                        "Create the {kind} on the target host, or fix the name."
                    ),
                    doc: format!("{DOCS}/kernel-mode/system-mode"),
                });
            }
        }
    }
    findings
}

/// Reads and validates the configuration at `path`, returning a report and the
/// file contents (when readable) so callers can render annotated snippets.
/// `system_mode` selects which runtime the enforceability pass judges against.
pub fn validate(path: &str, system_mode: bool) -> (ValidationReport, Option<String>) {
    let content = match fs::read_to_string(Path::new(path)) {
        Ok(content) => content,
        Err(err) => {
            let diagnostic = Diagnostic {
                severity: "error".into(),
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

    let content = match crate::config::resolve_includes(&content, Path::new(path)) {
        Ok(resolved) => resolved,
        Err(err) => {
            let diagnostic = Diagnostic {
                severity: "error".into(),
                line: None,
                column: None,
                kind: "unresolved-include".into(),
                message: err.to_string(),
                why: format!(
                    "'{path}' could not be assembled from its includes, so there is nothing to validate."
                ),
                suggestion: "Fix the included file or its path, then re-run validate."
                    .into(),
                doc: format!("{DOCS}/how-it-works/commands/validate"),
            };
            return (ValidationReport::failed(path, diagnostic), Some(content));
        }
    };

    if let Err(err) = parse_config_manifest(&content) {
        let diagnostic = classify_yaml(&err);
        return (ValidationReport::failed(path, diagnostic), Some(content));
    }

    match load_config(Some(path)) {
        Ok(config) => {
            let findings = mode_findings(&config, system_mode, &HostFacts::probe());
            let startable = findings.iter().all(|f| f.severity != "error");
            (
                ValidationReport {
                    config: path.to_string(),
                    valid: true,
                    startable,
                    diagnostics: findings,
                },
                Some(content),
            )
        }
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
        severity: "error".into(),
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
            "Give the health_check a `url:` or a `command:` (plus optional interval/attempt_timeout/retries).",
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
        severity: "error".into(),
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
        let (report, content) = validate(&path, false);
        assert!(report.valid);
        assert!(report.diagnostics.is_empty());
        assert!(content.is_some());
    }

    #[test]
    fn missing_version_is_classified() {
        let (_dir, path) = write_config("services:\n  api:\n    command: \"echo ok\"\n");
        let (report, _) = validate(&path, false);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "missing-version");
    }

    #[test]
    fn unsupported_version_is_classified() {
        let (_dir, path) =
            write_config("version: \"9\"\nservices:\n  api:\n    command: \"echo ok\"\n");
        let (report, _) = validate(&path, false);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "unsupported-version");
    }

    #[test]
    fn bad_health_check_is_classified() {
        let (_dir, path) = write_config(
            "version: \"2\"\nservices:\n  api:\n    command: \"echo ok\"\n    deployment:\n      health_check:\n        interval: \"2s\"\n",
        );
        let (report, _) = validate(&path, false);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "invalid-health-check");
    }

    #[test]
    fn unreadable_config_is_reported() {
        let (report, content) = validate("/nonexistent/path/systemg.yaml", false);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "unreadable-config");
        assert!(content.is_none());
    }

    #[test]
    fn unknown_dependency_is_classified() {
        let (_dir, path) = write_config(
            "version: \"2\"\nservices:\n  api:\n    command: \"echo ok\"\n    depends_on: [missing]\n",
        );
        let (report, _) = validate(&path, false);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "unknown-dependency");
    }

    #[test]
    fn dependency_cycle_is_classified() {
        let (_dir, path) = write_config(
            "version: \"2\"\nservices:\n  a:\n    command: \"x\"\n    depends_on: [b]\n  b:\n    command: \"y\"\n    depends_on: [a]\n",
        );
        let (report, _) = validate(&path, false);
        assert!(!report.valid);
        assert_eq!(report.diagnostics[0].kind, "dependency-cycle");
    }

    #[test]
    fn location_is_captured_for_syntax_errors() {
        let (_dir, path) = write_config(
            "version: \"2\"\nservices:\n  api:\n   command: \"x\"\n  bad: [unclosed\n",
        );
        let (report, _) = validate(&path, false);
        assert!(!report.valid);
        assert!(report.diagnostics[0].line.is_some());
    }

    fn fake_host(linux: bool) -> HostFacts {
        HostFacts {
            user_exists: |name| name == "realuser",
            group_exists: |name| name == "realgroup",
            linux,
        }
    }

    const PRIVILEGED: &str = r#"
version: "2"
services:
  db:
    command: "postgres"
    user: "realuser"
    capabilities: ["CAP_NET_BIND_SERVICE"]
"#;

    #[test]
    fn privileged_keys_fail_user_mode_validate() {
        let (_dir, path) = write_config(PRIVILEGED);
        let (report, _) = validate(&path, false);
        assert!(report.valid, "manifest is well-formed");
        assert!(!report.startable, "user mode cannot start it");
        assert_eq!(report.diagnostics[0].kind, "requires-system-mode");
        assert!(report.diagnostics[0].message.contains("SG0705"));
    }

    #[test]
    fn privileged_keys_pass_sys_mode_validate() {
        let (_dir, path) = write_config(PRIVILEGED);
        let (report, _) = validate(&path, true);
        assert!(report.valid);
        if cfg!(target_os = "linux") {
            assert!(report.startable);
        }
    }

    #[test]
    fn plain_manifest_startable_in_both_modes() {
        let plain = "version: \"2\"\nservices:\n  web:\n    command: \"sleep 1\"\n";
        let (_dir, path) = write_config(plain);
        for mode in [false, true] {
            let (report, _) = validate(&path, mode);
            assert!(report.valid && report.startable);
        }
    }

    #[test]
    fn mode_findings_flags_only_effective_requests() {
        let manifest = r#"
version: "2"
services:
  quiet:
    command: "sleep 1"
    capabilities: []
    isolation:
      network: false
"#;
        let config = crate::config::parse_config_manifest(manifest).expect("parse");
        assert!(mode_findings(&config, false, &fake_host(true)).is_empty());
    }

    #[test]
    fn mode_findings_warns_on_unknown_account_in_sys_mode() {
        let manifest = r#"
version: "2"
services:
  db:
    command: "postgres"
    user: "ghostuser"
    group: "realgroup"
"#;
        let config = crate::config::parse_config_manifest(manifest).expect("parse");
        let findings = mode_findings(&config, true, &fake_host(true));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, "warning");
        assert_eq!(findings[0].kind, "unknown-user");
    }

    #[test]
    fn mode_findings_errors_on_linux_only_keys_off_linux() {
        let manifest = r#"
version: "2"
services:
  db:
    command: "postgres"
    user: "realuser"
    capabilities: ["CAP_SYS_NICE"]
"#;
        let config = crate::config::parse_config_manifest(manifest).expect("parse");
        let findings = mode_findings(&config, true, &fake_host(false));
        assert!(
            findings
                .iter()
                .any(|f| f.kind == "linux-only-keys" && f.severity == "error")
        );
    }
}
