//! Typed diagnostics for `inspect` failures — replacing the bare
//! `eprintln!` + `exit(2)` the command used to print.

use crate::diag::{Diagnostic, SgCode};

/// Builds the SG0202 diagnostic for a service `inspect` could not find.
pub fn service_not_found(service: &str) -> Diagnostic {
    Diagnostic::error(
        SgCode::TargetNotFound,
        format!("no service named `{service}` to inspect"),
    )
    .note("inspect details one running or configured service; check the name")
    .help_cmd("list services", "sysg status")
    .help_docs()
}

/// Builds a diagnostic for an unparseable `--stream` duration.
pub fn invalid_stream_duration(value: &str, reason: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        SgCode::Catchall,
        format!("`--stream` value `{value}` is not a valid duration"),
    )
    .note(reason)
    .help_cmd("use seconds or a unit", "sysg inspect -s web --stream 5")
    .help_docs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_not_found_is_sg0202() {
        let diag = service_not_found("web");
        assert_eq!(diag.code, SgCode::TargetNotFound);
        assert!(diag.render(false).contains("web"));
    }

    #[test]
    fn invalid_stream_duration_names_the_value() {
        let diag = invalid_stream_duration("xyz", "not a number");
        assert!(diag.render(false).contains("xyz"));
    }
}
