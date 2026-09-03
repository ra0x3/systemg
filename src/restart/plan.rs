//! Resolving `restart`'s selectors into one exhaustive plan, plus a preflight
//! that refuses illegal operations before any side effect.
//!
//! Restart is stop + start + reconcile, so it carries the most failure surface
//! of any command. Two ideas keep it honest:
//!
//! - [`RestartPlan`] — an exhaustive enum of *what* to restart, resolved from
//!   the shared [`crate::selector::Target`].
//! - [`preflight`] — a total check of *whether the world permits it*, run before
//!   the plan is dispatched. It can reject the plan (returning a typed
//!   [`Diagnostic`]) or upgrade a whole-config restart to a supervisor
//!   [`RestartPlan::Recycle`] when the resident daemon's version has drifted.
//!   Nothing is torn down until preflight has passed.

use std::path::PathBuf;

use crate::{
    config::ServiceConfig,
    diag::{Diagnostic, SgCode},
    selector::{ProjectMismatch, Target, resolve_target},
};

/// Extracts the TCP port a service is expected to bind, from its health-check
/// URL or a `PORT` entry in its environment.
///
/// This decides a restart STRATEGY, never a kill. A rolling restart runs the
/// replacement before retiring the outgoing instance, which two processes
/// cannot do on one port — so a unit that declares `rolling` alongside a fixed
/// port is asking for something the kernel will not allow, and is restarted
/// immediately instead.
///
/// Deliberately NOT used to decide ownership of a port: sysg is a generic
/// process composer, and a health-check URL says where to probe, not that the
/// unit owns that port or may kill whoever holds it. Being wrong here costs a
/// slower restart; being wrong about a kill costs somebody else's process.
pub fn service_port(service: &ServiceConfig) -> Option<u16> {
    if let Some(port) = service
        .deployment
        .as_ref()
        .and_then(|deployment| deployment.health_check.as_ref())
        .and_then(|health| health.url.as_deref())
        .and_then(port_from_url)
    {
        return Some(port);
    }
    port_from_env(service)
}

/// Parses the port out of an `http://host:port/...` style URL.
fn port_from_url(url: &str) -> Option<u16> {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    host_port.rsplit(':').next().and_then(|p| p.parse().ok())
}

/// Reads a `PORT` value from a service's declared environment.
fn port_from_env(service: &ServiceConfig) -> Option<u16> {
    service
        .env
        .as_ref()
        .and_then(|env| env.vars.as_ref())
        .and_then(|vars| vars.get("PORT"))
        .and_then(|value| value.parse().ok())
}

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

/// Builds the SG0007 diagnostic for a recycle refused because a service manager
/// owns the running supervisor. Recycling stops the supervisor and starts a
/// replacement of its own, which the manager neither started nor tracks: the
/// two then race for the runtime and whichever wins is unsupervised.
pub fn recycle_manager_owned(unit_hint: &str) -> Diagnostic {
    Diagnostic::error(
        SgCode::SupervisorRestartConflict,
        "refused to recycle a supervisor a service manager owns",
    )
    .note("this supervisor was started by systemd or launchd, which restarts it and owns its lifetime")
    .note("stopping it here would leave the manager to start a second one while this command starts its own")
    .help_cmd("restart it through the manager", unit_hint)
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
///
/// `failed` is `None` when the restart failed for a reason that belongs to no
/// particular unit. The diagnostic then says so rather than naming every
/// targeted unit, which would report healthy services as failures.
pub fn reconcile_incomplete(
    failed: Option<&[String]>,
    cause: Option<&str>,
) -> Diagnostic {
    let note = match failed {
        Some(failed) => format!(
            "units that did not reach their target: {}",
            failed.join(", ")
        ),
        None => "the failure could not be attributed to a specific unit".to_string(),
    };
    let diag = Diagnostic::error(
        SgCode::ReconcileIncomplete,
        "the restart did not bring every unit to its target state",
    )
    .note(note);
    let diag = match cause {
        Some(cause) => diag.note(format!("cause: {cause}")),
        None => diag,
    };
    diag.help_cmd("see what's running", "sysg status")
        .help_docs()
}

/// Builds the SG0304 diagnostic for a restart that finished without bouncing a
/// single unit.
///
/// The dangerous case this exists for: a deploy ships a new binary, edits only
/// a cron unit in the manifest, and a `--delta` restart finds nothing else
/// changed. Every long-running unit keeps its old process and the caller sees
/// exit 0. Naming the units that were considered — and why each was passed
/// over — is the difference between a silent stale deploy and a loud one.
pub fn restart_touched_nothing(project: &str, considered: &[String]) -> Diagnostic {
    let note = if considered.is_empty() {
        "the restart targeted no units at all".to_string()
    } else {
        format!(
            "units considered but not bounced (cron-managed, skipped, or unchanged): {}",
            considered.join(", ")
        )
    };
    Diagnostic::error(
        SgCode::RestartTouchedNothing,
        format!("restart of project '{project}' bounced no units; every process it left running is the one that was already running"),
    )
    .note(note)
    .note("a redeployed binary at an unchanged path is invisible to a manifest diff")
    .help_cmd("bounce every unit anyway", "sysg restart")
    .help_cmd("bounce one unit outright", "sysg restart -s <service>")
    .help_docs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PathBuf {
        PathBuf::from("/x/systemg.yaml")
    }

    #[test]
    /// A fixed port is what forces a rolling restart down to immediate.
    fn parses_port_from_health_url() {
        assert_eq!(port_from_url("http://127.0.0.1:8100/health"), Some(8100));
        assert_eq!(port_from_url("https://api.example.com:443/x"), Some(443));
        // No port in the authority means nothing to collide on, so a rolling
        // restart is left alone.
        assert_eq!(port_from_url("http://localhost/health"), None);
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
    fn sg0302_names_only_the_units_that_actually_failed() {
        let failed = ["gamecast_draftkings_ingest".to_string()];
        let diag = reconcile_incomplete(Some(&failed), Some("timed out"));
        assert_eq!(diag.code, SgCode::ReconcileIncomplete);

        let rendered = format!("{diag}");
        assert!(rendered.contains("gamecast_draftkings_ingest"));
        assert!(
            !rendered.contains("gamecast_api"),
            "healthy units must never be named as failures"
        );
        assert!(rendered.contains("timed out"));
    }

    #[test]
    fn sg0302_says_indeterminate_rather_than_naming_every_unit() {
        let diag = reconcile_incomplete(None, Some("monitor thread failed to spawn"));
        assert_eq!(diag.code, SgCode::ReconcileIncomplete);

        let rendered = format!("{diag}");
        assert!(rendered.contains("could not be attributed"));
        assert!(rendered.contains("monitor thread failed to spawn"));
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
