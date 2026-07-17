//! Typed diagnostics for `logs` mode-resolution failures — the checks that used
//! to be bare `eprintln!` + `exit(2)`.

use crate::diag::{Diagnostic, SgCode};

/// Builds the SG0204 diagnostic for mutually-exclusive `logs` mode flags.
pub fn conflicting_modes(modes: &[&str]) -> Diagnostic {
    Diagnostic::error(
        SgCode::ConflictingSelectors,
        format!("{} cannot be combined", modes.join(" and ")),
    )
    .note("each selects a different logs mode; pick one")
    .help_docs()
}

/// Builds the SG0204 diagnostic for `--follow` combined with a non-show mode.
pub fn follow_with_mode(mode: &str) -> Diagnostic {
    Diagnostic::error(
        SgCode::ConflictingSelectors,
        format!("--follow cannot be combined with {mode}"),
    )
    .note("--follow streams live logs; it only applies to the default show mode")
    .help_docs()
}

/// Builds the SG0017 diagnostic for `--prune` with no size or age bound.
pub fn prune_bound_missing() -> Diagnostic {
    Diagnostic::error(
        SgCode::PruneBoundMissing,
        "nothing to prune against: no --max-size or --max-age bound",
    )
    .note("prune trims rotated backups down to a bound; give it at least one")
    .help_cmd("cap total size", "sysg logs --prune --max-size 500MB")
    .help_cmd("drop old backups", "sysg logs --prune --max-age 7d")
    .help_docs()
}

/// Builds the SG0019 diagnostic for a bare `logs` with no target selector.
pub fn target_required() -> Diagnostic {
    Diagnostic::error(
        SgCode::LogsTargetRequired,
        "`sysg logs` needs a target: a service, a project, or --supervisor",
    )
    .note("bare `logs` is refused so it never dumps every project's output at once")
    .help_cmd("one service", "sysg logs -s <service>")
    .help_cmd("a whole project", "sysg logs -p <project>")
    .help_cmd("the supervisor log", "sysg logs --supervisor")
    .help_docs()
}

/// Builds the SG0020 diagnostic for `--supervisor` combined with a selector.
pub fn supervisor_with_selector() -> Diagnostic {
    Diagnostic::error(
        SgCode::LogsSupervisorConflict,
        "--supervisor cannot be combined with a service or project selector",
    )
    .note("the supervisor log is a single stream; it has no -s/-p scope")
    .help_cmd("supervisor log", "sysg logs --supervisor")
    .help_docs()
}

/// Builds the SG0021 diagnostic for `logs -s <service>` (no -p) where the
/// service is not in the loose bundle.
pub fn loose_service_not_found(service: &str) -> Diagnostic {
    Diagnostic::error(
        SgCode::LooseServiceNotFound,
        format!("no loose service named `{service}`"),
    )
    .note(
        "a bare `-s` reads only project-less (loose) services; a service inside a \
         project needs its project named",
    )
    .help_cmd(
        "target its project",
        format!("sysg logs -s {service} -p <project>"),
    )
    .help_docs()
}

/// Builds the SG0204 diagnostic for an unsupported `--format` value.
pub fn unsupported_format(format: &str) -> Diagnostic {
    Diagnostic::error(
        SgCode::ConflictingSelectors,
        format!("`sysg logs` does not support --format {format}"),
    )
    .note("logs are line-oriented; use --format json for machine output")
    .help_docs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflicting_modes_is_sg0204_and_names_them() {
        let diag = conflicting_modes(&["--path", "--purge"]);
        assert_eq!(diag.code, SgCode::ConflictingSelectors);
        assert!(diag.render(false).contains("--path and --purge"));
    }

    #[test]
    fn prune_bound_missing_is_sg0017() {
        let diag = prune_bound_missing();
        assert_eq!(diag.code, SgCode::PruneBoundMissing);
        assert!(diag.render(false).contains("SG0017"));
    }

    #[test]
    fn follow_with_mode_is_sg0204() {
        let diag = follow_with_mode("--path");
        assert_eq!(diag.code, SgCode::ConflictingSelectors);
    }
}
