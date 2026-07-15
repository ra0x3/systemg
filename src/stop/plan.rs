//! Resolving `stop`'s selectors into one exhaustive plan.
//!
//! Like [`StartPlan`](crate::start::StartPlan), this separates *what to stop*
//! (resolved from the selectors alone) from *how to dispatch it*. The exhaustive
//! enum makes the compiler cover every dispatch case, so no combination of flags
//! silently does the wrong thing.

use std::path::PathBuf;

use crate::start::plan::{ProjectMismatch, split_selector};

/// What a `stop` invocation targets, resolved from its selectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopPlan {
    /// Stop every service the running supervisor manages, leaving the
    /// supervisor itself up.
    Everything {
        /// The resolved config path (for project-context resolution).
        config: PathBuf,
    },
    /// Stop every service in one project.
    Project {
        /// The project id to stop.
        project: String,
    },
    /// Stop one service, optionally qualified by its project.
    Service {
        /// The service name (never carries a `project/` prefix).
        service: String,
        /// The project the service belongs to, when known from `-p` or a
        /// `project/service` selector. `None` means "resolve from the resident
        /// supervisor", where SG0006 ambiguity is enforced.
        project: Option<String>,
    },
    /// Shut the whole supervisor down (and with it every service).
    Supervisor,
}

/// A `--supervisor` flag combined with a unit selector, which is contradictory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorSelectorConflict;

/// Why a set of stop selectors could not be resolved into a plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopPlanError {
    /// `-p` disagreed with a `project/service` selector prefix.
    Mismatch(ProjectMismatch),
    /// `--supervisor` was combined with a `-s`/`-p` selector.
    SupervisorWithSelector,
}

/// Resolves the selectors into a [`StopPlan`]. `config` is the already-resolved
/// config path (used only by [`StopPlan::Everything`]).
///
/// `--supervisor` is exclusive: combining it with a `-s`/`-p` selector is a
/// conflict, since you cannot both shut the supervisor down and target one unit.
pub fn resolve_plan(
    config: PathBuf,
    service: Option<&str>,
    project: Option<&str>,
    supervisor: bool,
) -> Result<StopPlan, StopPlanError> {
    if supervisor {
        if service.is_some() || project.is_some() {
            return Err(StopPlanError::SupervisorWithSelector);
        }
        return Ok(StopPlan::Supervisor);
    }

    if let Some(selector) = service {
        let (selector_project, service_name) = match split_selector(selector) {
            Some((p, s)) => (Some(p), s),
            None => (None, selector),
        };

        if let (Some(flag), Some(sel)) = (project, selector_project)
            && flag != sel
        {
            return Err(StopPlanError::Mismatch(ProjectMismatch {
                flag: flag.to_string(),
                selector: sel.to_string(),
            }));
        }

        return Ok(StopPlan::Service {
            service: service_name.to_string(),
            project: project.or(selector_project).map(str::to_string),
        });
    }

    match project {
        Some(project) => Ok(StopPlan::Project {
            project: project.to_string(),
        }),
        None => Ok(StopPlan::Everything { config }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PathBuf {
        PathBuf::from("/x/systemg.yaml")
    }

    #[test]
    fn no_selectors_stops_everything() {
        assert_eq!(
            resolve_plan(cfg(), None, None, false).unwrap(),
            StopPlan::Everything { config: cfg() }
        );
    }

    #[test]
    fn supervisor_flag_targets_the_supervisor() {
        assert_eq!(
            resolve_plan(cfg(), None, None, true).unwrap(),
            StopPlan::Supervisor
        );
    }

    #[test]
    fn supervisor_with_a_selector_is_a_conflict() {
        assert_eq!(
            resolve_plan(cfg(), Some("web"), None, true).unwrap_err(),
            StopPlanError::SupervisorWithSelector
        );
        assert_eq!(
            resolve_plan(cfg(), None, Some("alpha"), true).unwrap_err(),
            StopPlanError::SupervisorWithSelector
        );
    }

    #[test]
    fn project_flag_stops_one_project() {
        assert_eq!(
            resolve_plan(cfg(), None, Some("alpha"), false).unwrap(),
            StopPlan::Project {
                project: "alpha".into()
            }
        );
    }

    #[test]
    fn bare_service_leaves_project_for_resident_resolution() {
        assert_eq!(
            resolve_plan(cfg(), Some("worker"), None, false).unwrap(),
            StopPlan::Service {
                service: "worker".into(),
                project: None
            }
        );
    }

    #[test]
    fn qualified_selector_splits_project_and_service() {
        assert_eq!(
            resolve_plan(cfg(), Some("alpha/worker"), None, false).unwrap(),
            StopPlan::Service {
                service: "worker".into(),
                project: Some("alpha".into())
            }
        );
    }

    #[test]
    fn project_flag_conflicting_with_selector_is_a_mismatch() {
        let err =
            resolve_plan(cfg(), Some("beta/worker"), Some("alpha"), false).unwrap_err();
        assert_eq!(
            err,
            StopPlanError::Mismatch(ProjectMismatch {
                flag: "alpha".into(),
                selector: "beta".into()
            })
        );
    }
}
