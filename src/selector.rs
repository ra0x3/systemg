//! The shared command selector: how `-s`/`-p`/`project/service` flags resolve to
//! a target set, identically for `start`, `stop`, and `restart`.
//!
//! Each command's plan (`StartPlan`, `StopPlan`, `RestartPlan`) is built on top
//! of a [`crate::selector::Target`] so the selector semantics — including the `-p`-vs-selector
//! mismatch rule — live in exactly one place and cannot drift between commands.

/// Splits a `project/service` selector into its parts, or `None` if unqualified.
/// A part is only recognised when both sides are non-empty.
pub fn split_selector(selector: &str) -> Option<(&str, &str)> {
    selector
        .split_once('/')
        .filter(|(p, s)| !p.is_empty() && !s.is_empty())
}

/// A `-p` flag disagreeing with a `project/service` selector prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMismatch {
    /// The project named by `-p`.
    pub flag: String,
    /// The project named by the selector prefix.
    pub selector: String,
}

/// What a command's selectors resolve to, before command-specific dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// No `-s`/`-p`: everything the command's config or resident set covers.
    Everything,
    /// `-p <id>`: one project.
    Project {
        /// The project id.
        project: String,
    },
    /// `-s <name>` / `project/name` / `-p <id> -s <name>`: one service.
    Service {
        /// The service name (never carries a `project/` prefix).
        service: String,
        /// The project the service belongs to, when known from `-p` or a
        /// `project/service` selector. `None` means "resolve from the resident
        /// supervisor", where cross-project ambiguity (SG0006) is enforced.
        project: Option<String>,
    },
}

/// Resolves `-s`/`-p` selectors into a [`Target`].
///
/// A `-p` flag that disagrees with a `project/service` selector prefix is a
/// [`ProjectMismatch`] (the caller renders SG0201). This is the single source of
/// truth for selector resolution across every command.
pub fn resolve_target(
    service: Option<&str>,
    project: Option<&str>,
) -> Result<Target, ProjectMismatch> {
    if let Some(selector) = service {
        let (selector_project, service_name) = match split_selector(selector) {
            Some((p, s)) => (Some(p), s),
            None => (None, selector),
        };

        if let (Some(flag), Some(sel)) = (project, selector_project)
            && flag != sel
        {
            return Err(ProjectMismatch {
                flag: flag.to_string(),
                selector: sel.to_string(),
            });
        }

        return Ok(Target::Service {
            service: service_name.to_string(),
            project: project.or(selector_project).map(str::to_string),
        });
    }

    match project {
        Some(project) => Ok(Target::Project {
            project: project.to_string(),
        }),
        None => Ok(Target::Everything),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_resolves_to_everything() {
        assert_eq!(resolve_target(None, None).unwrap(), Target::Everything);
    }

    #[test]
    fn project_flag_resolves_to_project() {
        assert_eq!(
            resolve_target(None, Some("alpha")).unwrap(),
            Target::Project {
                project: "alpha".into()
            }
        );
    }

    #[test]
    fn bare_service_leaves_project_unresolved() {
        assert_eq!(
            resolve_target(Some("worker"), None).unwrap(),
            Target::Service {
                service: "worker".into(),
                project: None
            }
        );
    }

    #[test]
    fn qualified_selector_splits() {
        assert_eq!(
            resolve_target(Some("alpha/worker"), None).unwrap(),
            Target::Service {
                service: "worker".into(),
                project: Some("alpha".into())
            }
        );
    }

    #[test]
    fn flag_and_matching_selector_agree() {
        assert_eq!(
            resolve_target(Some("alpha/worker"), Some("alpha")).unwrap(),
            Target::Service {
                service: "worker".into(),
                project: Some("alpha".into())
            }
        );
    }

    #[test]
    fn flag_conflicting_with_selector_is_a_mismatch() {
        let err = resolve_target(Some("beta/worker"), Some("alpha")).unwrap_err();
        assert_eq!(err.flag, "alpha");
        assert_eq!(err.selector, "beta");
    }
}
