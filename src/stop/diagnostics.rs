//! Typed diagnostics for the `stop` command.

use crate::diag::{Diagnostic, SgCode};

/// Builds the diagnostic for a stop that targets a service no project declares.
pub fn service_not_found(service: &str) -> Diagnostic {
    Diagnostic::error(
        SgCode::TargetNotFound,
        format!("no managed service named `{service}`"),
    )
    .note("no project the supervisor manages declares this service")
    .help_cmd("list what's running", "sysg status")
    .help_docs()
}

/// Builds the diagnostic for a command that targets a project this supervisor
/// does not manage.
pub fn project_not_found(project: &str) -> Diagnostic {
    Diagnostic::error(
        SgCode::TargetNotFound,
        format!("no managed project named `{project}`"),
    )
    .note("this supervisor is not managing a project with that id")
    .help_cmd("list what's running", "sysg status")
    .help_docs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_not_found_is_typed() {
        let diag = service_not_found("ghost");
        assert_eq!(diag.code, SgCode::TargetNotFound);
        assert!(diag.render(false).contains("ghost"));
    }

    #[test]
    fn project_not_found_is_typed() {
        let diag = project_not_found("ghost");
        assert_eq!(diag.code, SgCode::TargetNotFound);
        assert!(diag.render(false).contains("ghost"));
    }
}
