//! Resolving `inspect`'s selector into one plan.
//!
//! Inspect is single-mode — it details exactly one service — so its plan is
//! lean: a resolved service target plus the runtime-collection flag. The point
//! of the plan is to reject up front the selectors inspect cannot serve (a whole
//! project, or nothing) rather than discover them mid-fetch.
//!
//! - [`InspectPlan`] — the one service to inspect, its project, and `live`.
//! - [`resolve_plan`] — resolves `-s`/`-p` into the plan, or a typed error.

use crate::selector::{ProjectMismatch, Target, resolve_target};

/// The single service an `inspect` invocation targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectPlan {
    /// The service to inspect (never carries a `project/` prefix).
    pub service: String,
    /// The project the service belongs to, when known from `-p` or a
    /// `project/service` selector. `None` resolves from the resident supervisor.
    pub project: Option<String>,
    /// Force immediate runtime collection instead of the configured snapshot.
    pub live: bool,
}

/// Why an `inspect` invocation could not resolve to a plan.
#[derive(Debug, PartialEq, Eq)]
pub enum InspectPlanError {
    /// The selector did not name a single service (inspect requires one).
    NotAService,
    /// The `-p` flag disagreed with a `project/service` selector prefix.
    Mismatch(ProjectMismatch),
}

/// Resolves the selector into an [`InspectPlan`].
///
/// Inspect details one service, so `Target::Everything` (no service) and
/// `Target::Project` (a whole project) are rejected — there is nothing single to
/// inspect. Only a `Target::Service` resolves.
pub fn resolve_plan(
    service: &str,
    project: Option<&str>,
    live: bool,
) -> Result<InspectPlan, InspectPlanError> {
    match resolve_target(Some(service), project).map_err(InspectPlanError::Mismatch)? {
        Target::Service { service, project } => Ok(InspectPlan {
            service,
            project,
            live,
        }),
        Target::Everything | Target::Project { .. } => Err(InspectPlanError::NotAService),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_resolves() {
        assert_eq!(
            resolve_plan("web", None, false).unwrap(),
            InspectPlan {
                service: "web".into(),
                project: None,
                live: false
            }
        );
    }

    #[test]
    fn project_qualified_service_resolves() {
        assert_eq!(
            resolve_plan("web", Some("demo"), true).unwrap(),
            InspectPlan {
                service: "web".into(),
                project: Some("demo".into()),
                live: true
            }
        );
    }

    #[test]
    fn prefixed_selector_carries_its_project() {
        assert_eq!(
            resolve_plan("demo/web", None, false).unwrap(),
            InspectPlan {
                service: "web".into(),
                project: Some("demo".into()),
                live: false
            }
        );
    }

    #[test]
    fn p_flag_conflicting_with_prefix_is_a_mismatch() {
        assert!(matches!(
            resolve_plan("other/web", Some("demo"), false),
            Err(InspectPlanError::Mismatch(_))
        ));
    }
}
