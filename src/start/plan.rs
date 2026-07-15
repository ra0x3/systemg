//! Resolving `start`'s selectors into one exhaustive plan.
//!
//! The `start` command has several orthogonal axes — target a whole config, a
//! project, or a service; run against a resident supervisor or fork one;
//! daemonize or stay in the foreground. Historically these were tangled in one
//! deeply-nested branch. [`StartPlan`] separates *what to start* (resolved here,
//! from the selectors alone) from *how to dispatch it* (decided by the caller
//! from the running/daemonize state). Making the plan an exhaustive enum means
//! the compiler forces every dispatch path to handle every case — no silent
//! fall-through, which was the source of the wrong-target bugs.

use std::path::PathBuf;

pub use crate::selector::{ProjectMismatch, split_selector};
use crate::selector::{Target, resolve_target};

/// What a `start` invocation targets, resolved from its selectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartPlan {
    /// Start everything the config file declares (every project + loose bundle).
    WholeConfig {
        /// The resolved config path.
        config: PathBuf,
    },
    /// Start a single project by id.
    Project {
        /// The resolved config path (used to register the project if resident
        /// resolution is unavailable).
        config: PathBuf,
        /// The project id to target.
        project: String,
    },
    /// Start a single service, optionally qualified by its project.
    Service {
        /// The resolved config path.
        config: PathBuf,
        /// The service name (never carries a `project/` prefix — that is split
        /// into `project`).
        service: String,
        /// The project the service belongs to, when known from `-p` or a
        /// `project/service` selector. `None` means "resolve it from the
        /// resident supervisor", which is where SG0006 ambiguity is enforced.
        project: Option<String>,
    },
    /// Stage an ad-hoc unit written from a bare command; never auto-applied to a
    /// running supervisor.
    StageAdHoc {
        /// Path of the staged unit config.
        config: PathBuf,
    },
}

/// Resolves the selectors into a [`StartPlan`]. `ad_hoc` marks a bare-command
/// invocation; `config` is the already-resolved config path.
///
/// A `-p` flag that disagrees with a `project/service` selector prefix is a
/// mismatch and returns `Err(the two project ids)`; the caller renders SG0201.
pub fn resolve_plan(
    config: PathBuf,
    service: Option<&str>,
    project: Option<&str>,
    ad_hoc: bool,
) -> Result<StartPlan, ProjectMismatch> {
    if ad_hoc {
        return Ok(StartPlan::StageAdHoc { config });
    }

    Ok(match resolve_target(service, project)? {
        Target::Everything => StartPlan::WholeConfig { config },
        Target::Project { project } => StartPlan::Project { config, project },
        Target::Service { service, project } => StartPlan::Service {
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
    fn no_selectors_targets_the_whole_config() {
        assert_eq!(
            resolve_plan(cfg(), None, None, false).unwrap(),
            StartPlan::WholeConfig { config: cfg() }
        );
    }

    #[test]
    fn project_flag_targets_one_project() {
        assert_eq!(
            resolve_plan(cfg(), None, Some("alpha"), false).unwrap(),
            StartPlan::Project {
                config: cfg(),
                project: "alpha".into()
            }
        );
    }

    #[test]
    fn bare_service_leaves_project_for_resident_resolution() {
        assert_eq!(
            resolve_plan(cfg(), Some("worker"), None, false).unwrap(),
            StartPlan::Service {
                config: cfg(),
                service: "worker".into(),
                project: None
            }
        );
    }

    #[test]
    fn qualified_selector_splits_project_and_service() {
        assert_eq!(
            resolve_plan(cfg(), Some("alpha/worker"), None, false).unwrap(),
            StartPlan::Service {
                config: cfg(),
                service: "worker".into(),
                project: Some("alpha".into())
            }
        );
    }

    #[test]
    fn project_flag_and_matching_selector_agree() {
        assert_eq!(
            resolve_plan(cfg(), Some("alpha/worker"), Some("alpha"), false).unwrap(),
            StartPlan::Service {
                config: cfg(),
                service: "worker".into(),
                project: Some("alpha".into())
            }
        );
    }

    #[test]
    fn project_flag_conflicting_with_selector_is_a_mismatch() {
        let err =
            resolve_plan(cfg(), Some("beta/worker"), Some("alpha"), false).unwrap_err();
        assert_eq!(err.flag, "alpha");
        assert_eq!(err.selector, "beta");
    }

    #[test]
    fn ad_hoc_stages_regardless_of_selectors() {
        assert_eq!(
            resolve_plan(cfg(), None, None, true).unwrap(),
            StartPlan::StageAdHoc { config: cfg() }
        );
    }
}
