//! Resolving `status`'s selectors into one exhaustive plan.
//!
//! Like the other command plans, this separates *what to report on* (resolved
//! from the selectors alone) from *how to fetch and render it*. `status` is
//! read-only, so its plan carries no side effect — but the same exhaustive enum
//! keeps selector handling honest and shares the one selector resolver, so a
//! `-p`/`-s` combination can never silently mean something different here than
//! it does for start, stop, or restart.

use std::path::PathBuf;

use crate::selector::{ProjectMismatch, Target, resolve_target};

/// What a `status` invocation reports on, resolved from its selectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusPlan {
    /// Report on everything the resolved config declares.
    Everything {
        /// The resolved config path (scopes which projects are shown).
        config: PathBuf,
    },
    /// Report on a single project.
    Project {
        /// The resolved config path.
        config: PathBuf,
        /// The project id to report on.
        project: String,
    },
    /// Report on a single service, optionally qualified by its project.
    Service {
        /// The resolved config path.
        config: PathBuf,
        /// The service name (never carries a `project/` prefix).
        service: String,
        /// The project the service belongs to, when known from `-p` or a
        /// `project/service` selector. `None` means "match across all projects".
        project: Option<String>,
    },
}

impl StatusPlan {
    /// The config path every plan variant carries.
    pub fn config(&self) -> &PathBuf {
        match self {
            StatusPlan::Everything { config }
            | StatusPlan::Project { config, .. }
            | StatusPlan::Service { config, .. } => config,
        }
    }
}

/// Resolves the selectors into a [`StatusPlan`]. `config` is the already-resolved
/// config path.
///
/// A `-p` flag that disagrees with a `project/service` selector prefix is a
/// mismatch and returns `Err`; the caller renders SG0201.
pub fn resolve_plan(
    config: PathBuf,
    service: Option<&str>,
    project: Option<&str>,
) -> Result<StatusPlan, ProjectMismatch> {
    Ok(match resolve_target(service, project)? {
        Target::Everything => StatusPlan::Everything { config },
        Target::Project { project } => StatusPlan::Project { config, project },
        Target::Service { service, project } => StatusPlan::Service {
            config,
            service,
            project,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PathBuf {
        PathBuf::from("/x/systemg.yaml")
    }

    #[test]
    fn no_selectors_reports_everything() {
        assert_eq!(
            resolve_plan(cfg(), None, None).unwrap(),
            StatusPlan::Everything { config: cfg() }
        );
    }

    #[test]
    fn project_flag_reports_one_project() {
        assert_eq!(
            resolve_plan(cfg(), None, Some("alpha")).unwrap(),
            StatusPlan::Project {
                config: cfg(),
                project: "alpha".into()
            }
        );
    }

    #[test]
    fn bare_service_matches_across_projects() {
        assert_eq!(
            resolve_plan(cfg(), Some("worker"), None).unwrap(),
            StatusPlan::Service {
                config: cfg(),
                service: "worker".into(),
                project: None
            }
        );
    }

    #[test]
    fn qualified_selector_splits_project_and_service() {
        assert_eq!(
            resolve_plan(cfg(), Some("alpha/worker"), None).unwrap(),
            StatusPlan::Service {
                config: cfg(),
                service: "worker".into(),
                project: Some("alpha".into())
            }
        );
    }

    #[test]
    fn project_flag_conflicting_with_selector_is_a_mismatch() {
        let err = resolve_plan(cfg(), Some("beta/worker"), Some("alpha")).unwrap_err();
        assert_eq!(err.flag, "alpha");
        assert_eq!(err.selector, "beta");
    }
}
