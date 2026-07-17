//! Resolving `restart`'s selectors into one exhaustive plan, plus a preflight
//! that refuses illegal operations before any side effect.
//!
//! Restart is stop + start + reconcile, so it carries the most failure surface
//! of any command. Two ideas keep it honest:
//!
//! - [`RestartPlan`] — an exhaustive enum of *what* to restart, resolved from
//!   the shared [`Target`](crate::selector::Target).
//! - [`preflight`] — a total check of *whether the world permits it*, run before
//!   the plan is dispatched. It can reject the plan (returning a typed
//!   [`Diagnostic`]) or upgrade a whole-config restart to a supervisor
//!   [`RestartPlan::Recycle`] when the resident daemon's version has drifted.
//!   Nothing is torn down until preflight has passed.

use std::path::PathBuf;

use crate::{
    diag::{Diagnostic, SgCode},
    selector::{ProjectMismatch, Target, resolve_target},
};

/// What a `restart` invocation targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartPlan {
    /// Reconcile and restart everything the config declares.
    Everything {
        /// The resolved config path.
        config: PathBuf,
    },
    /// Restart one project.
    Project {
        /// The resolved config path, so a `-c` reload reaches the supervisor.
        config: PathBuf,
        /// The project id.
        project: String,
    },
    /// Restart one service, optionally qualified by its project.
    Service {
        /// The resolved config path, so a `-c` reload reaches the supervisor and
        /// the service's changed config is applied on the bounce.
        config: PathBuf,
        /// The service name (never carries a `project/` prefix).
        service: String,
        /// The project the service belongs to, when known. `None` resolves from
        /// the resident supervisor (SG0006 on ambiguity).
        project: Option<String>,
    },
    /// Tear the resident supervisor down and re-fork it, because its running
    /// binary version has drifted from this CLI. Only ever reached for a
    /// whole-config restart, via preflight.
    Recycle {
        /// The config the recycled supervisor boots from.
        config: PathBuf,
    },
}

/// Resolves the selectors into a base [`RestartPlan`], before preflight.
///
/// A `-p` flag that disagrees with a `project/service` selector prefix is a
/// mismatch (the caller renders SG0201).
pub fn resolve_plan(
    config: PathBuf,
    service: Option<&str>,
    project: Option<&str>,
) -> Result<RestartPlan, ProjectMismatch> {
    Ok(match resolve_target(service, project)? {
        Target::Everything => RestartPlan::Everything { config },
        Target::Project { project } => RestartPlan::Project { config, project },
        Target::Service { service, project } => RestartPlan::Service {
            config,
            service,
            project,
        },
    })
}

/// A snapshot of the world that preflight inspects. Kept small and explicit so
/// preflight stays a pure decision over known facts.
#[derive(Debug, Clone, Copy)]
pub struct World {
    /// Whether a supervisor is currently running.
    pub supervisor_running: bool,
    /// Whether the resident supervisor's version has drifted from this CLI, so a
    /// whole-config restart must recycle it rather than message it.
    pub version_drifted: bool,
}

/// The outcome of preflight: a plan cleared to dispatch, or a refusal.
#[derive(Debug)]
pub enum Preflight {
    /// The plan passed preflight and may be dispatched.
    Ready(RestartPlan),
    /// The plan is refused; render this diagnostic and do not touch anything.
    Refused(Box<Diagnostic>),
}

/// Checks whether `plan` is legal given `world`, before any side effect.
///
/// A whole-config restart against a version-drifted resident supervisor is
/// upgraded to [`RestartPlan::Recycle`]. Whole-config validation of the manifest
/// itself (SG0301) happens in the reconcile step, which has the parsed config;
/// preflight covers the world-state preconditions.
pub fn preflight(plan: RestartPlan, world: World) -> Preflight {
    if let RestartPlan::Everything { config } = &plan
        && world.supervisor_running
        && world.version_drifted
    {
        return Preflight::Ready(RestartPlan::Recycle {
            config: config.clone(),
        });
    }
    Preflight::Ready(plan)
}

/// Builds the SG0301 diagnostic for a whole-config restart whose new manifest
/// failed validation — the restart is refused and nothing is touched.
pub fn manifest_rejected(reason: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        SgCode::ManifestRejected,
        "the new manifest is invalid; the restart was refused and nothing was changed",
    )
    .note(reason)
    .note("fix the manifest and retry; the running services were left untouched")
    .help_docs()
}

/// Builds the SG0303 diagnostic for a supervisor recycle that was refused
/// because the replacement config failed to validate. The old supervisor is
/// left running — a bad config never costs you the working stack.
pub fn recycle_refused(
    config: &std::path::Path,
    reason: impl Into<String>,
) -> Diagnostic {
    Diagnostic::error(
        SgCode::SupervisorRecycleFailed,
        "refused to recycle the supervisor: the replacement config is invalid",
    )
    .note(reason)
    .note(format!(
        "the existing supervisor was left running; {} was not applied",
        config.display()
    ))
    .help_docs()
}

/// Builds the SG0303 diagnostic for a recycle that stopped the old supervisor
/// but could not start the new one — the box is now unsupervised. The help
/// carries the exact command to bring supervision back.
pub fn recycle_failed(config: &std::path::Path, reason: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        SgCode::SupervisorRecycleFailed,
        "supervisor recycle failed: the old daemon was stopped but the new one did not start",
    )
    .note(reason)
    .note("the box is currently unsupervised")
    .help_cmd(
        "recover",
        format!("sysg start --daemonize --config {}", config.display()),
    )
    .help_docs()
}

/// Builds the SG0302 diagnostic for a reconcile that ran but left one or more
/// units short of their manifest target.
pub fn reconcile_incomplete(failed: &[String]) -> Diagnostic {
    Diagnostic::error(
        SgCode::ReconcileIncomplete,
        "the restart did not bring every unit to its target state",
    )
    .note(format!(
        "units that did not reach their target: {}",
        failed.join(", ")
    ))
    .help_cmd("see what's running", "sysg status")
    .help_docs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PathBuf {
        PathBuf::from("/x/systemg.yaml")
    }

    #[test]
    fn recycle_refused_is_sg0303_and_names_the_untouched_stack() {
        let diag = recycle_refused(std::path::Path::new("/x/stack.yaml"), "bad yaml");
        assert_eq!(diag.code, SgCode::SupervisorRecycleFailed);
        assert!(diag.notes.iter().any(|n| n.contains("bad yaml")));
        assert!(diag.notes.iter().any(|n| n.contains("left running")));
    }

    #[test]
    fn recycle_failed_carries_the_recovery_command() {
        let diag = recycle_failed(std::path::Path::new("/x/stack.yaml"), "no port");
        assert_eq!(diag.code, SgCode::SupervisorRecycleFailed);
        assert!(diag.notes.iter().any(|n| n.contains("unsupervised")));
        let help = format!("{diag}");
        assert!(help.contains("sysg start --daemonize --config /x/stack.yaml"));
    }

    #[test]
    fn no_selectors_targets_everything() {
        assert_eq!(
            resolve_plan(cfg(), None, None).unwrap(),
            RestartPlan::Everything { config: cfg() }
        );
    }

    #[test]
    fn project_and_service_selectors_resolve() {
        assert_eq!(
            resolve_plan(cfg(), None, Some("alpha")).unwrap(),
            RestartPlan::Project {
                config: cfg(),
                project: "alpha".into()
            }
        );
        assert_eq!(
            resolve_plan(cfg(), Some("alpha/worker"), None).unwrap(),
            RestartPlan::Service {
                config: cfg(),
                service: "worker".into(),
                project: Some("alpha".into())
            }
        );
    }

    #[test]
    fn mismatch_is_reported() {
        let err = resolve_plan(cfg(), Some("beta/worker"), Some("alpha")).unwrap_err();
        assert_eq!(err.flag, "alpha");
    }

    #[test]
    fn preflight_upgrades_drifted_whole_config_to_recycle() {
        let world = World {
            supervisor_running: true,
            version_drifted: true,
        };
        match preflight(RestartPlan::Everything { config: cfg() }, world) {
            Preflight::Ready(RestartPlan::Recycle { config }) => {
                assert_eq!(config, cfg())
            }
            other => panic!("expected recycle, got {other:?}"),
        }
    }

    #[test]
    fn preflight_leaves_a_matched_whole_config_alone() {
        let world = World {
            supervisor_running: true,
            version_drifted: false,
        };
        match preflight(RestartPlan::Everything { config: cfg() }, world) {
            Preflight::Ready(RestartPlan::Everything { .. }) => {}
            other => panic!("expected everything, got {other:?}"),
        }
    }

    #[test]
    fn preflight_never_recycles_a_targeted_restart() {
        let world = World {
            supervisor_running: true,
            version_drifted: true,
        };
        match preflight(
            RestartPlan::Project {
                config: cfg(),
                project: "alpha".into(),
            },
            world,
        ) {
            Preflight::Ready(RestartPlan::Project { .. }) => {}
            other => panic!("targeted restart must not recycle, got {other:?}"),
        }
    }
}
