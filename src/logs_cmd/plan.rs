//! Resolving `logs`'s many flags into one exhaustive mode plan.
//!
//! `logs` is really five commands wearing one name — show, follow, print-path,
//! purge, and prune. The flags that pick a mode are mutually exclusive, so the
//! honest model is an enum resolved once, up front:
//!
//! - [`LogsPlan`] — which mode this invocation is, with its resolved selector.
//! - [`resolve_plan`] — maps the mode flags + `-s`/`-p` selectors into exactly
//!   one plan, or a typed error for an illegal combination.
//!
//! The heavy log I/O (tailing, streaming, rotation) stays in [`crate::logs`];
//! this module only decides *what* to do.

use crate::selector::{ProjectMismatch, Target, resolve_target};

/// Which mode a `logs` invocation runs in, with its resolved selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogsPlan {
    /// Show (and optionally follow) a target's logs.
    Show {
        /// The resolved selector.
        target: Target,
        /// Whether to follow the stream rather than print a snapshot.
        follow: bool,
    },
    /// Print the on-disk log path(s) for a target and exit.
    Path {
        /// The resolved selector.
        target: Target,
        /// List rotated backups too.
        all: bool,
    },
    /// Clear a target's captured log files.
    Purge {
        /// The resolved selector.
        target: Target,
    },
    /// Trim rotated log backups against a size and/or age bound.
    Prune {
        /// Cap total rotated-backup size, e.g. "500MB".
        max_size: Option<String>,
        /// Remove rotated backups older than this, e.g. "7d".
        max_age: Option<String>,
    },
}

/// The mode flags a `logs` invocation set, before selector resolution.
#[derive(Debug, Clone, Copy, Default)]
pub struct Modes {
    /// `--path`: print on-disk paths.
    pub path: bool,
    /// `--purge`: clear captured logs.
    pub purge: bool,
    /// `--prune`: trim rotated backups.
    pub prune: bool,
    /// `--follow`: stream rather than snapshot (only meaningful for show).
    pub follow: bool,
}

/// Why a `logs` invocation could not resolve to a single plan.
#[derive(Debug, PartialEq, Eq)]
pub enum LogsPlanError {
    /// More than one mutually-exclusive mode flag was set.
    ConflictingModes {
        /// The mode flags that clashed, for the message.
        modes: Vec<&'static str>,
    },
    /// A mode flag was combined with `--follow`, which only makes sense for show.
    FollowWithMode {
        /// The mode `--follow` was illegally combined with.
        mode: &'static str,
    },
    /// `--prune` ran without a `--max-size` or `--max-age` bound.
    PruneBoundMissing,
    /// The `-p` flag disagreed with a `project/service` selector prefix.
    Mismatch(ProjectMismatch),
}

/// Resolves the mode flags and selectors into exactly one [`LogsPlan`].
///
/// The mode flags (`--path`/`--purge`/`--prune`) are mutually exclusive and none
/// combines with `--follow`. Prune ignores selectors (it works on rotated
/// backups directory-wide) but requires at least one bound. Everything else is a
/// show, which is the only mode `--follow` applies to.
pub fn resolve_plan(
    modes: Modes,
    service: Option<&str>,
    project: Option<&str>,
    max_size: Option<String>,
    max_age: Option<String>,
) -> Result<LogsPlan, LogsPlanError> {
    let mut set = Vec::new();
    if modes.path {
        set.push("--path");
    }
    if modes.purge {
        set.push("--purge");
    }
    if modes.prune {
        set.push("--prune");
    }
    if set.len() > 1 {
        return Err(LogsPlanError::ConflictingModes { modes: set });
    }

    if modes.follow
        && let Some(mode) = set.first()
    {
        return Err(LogsPlanError::FollowWithMode { mode });
    }

    if modes.prune {
        if max_size.is_none() && max_age.is_none() {
            return Err(LogsPlanError::PruneBoundMissing);
        }
        return Ok(LogsPlan::Prune { max_size, max_age });
    }

    let target = resolve_target(service, project).map_err(LogsPlanError::Mismatch)?;

    if modes.path {
        return Ok(LogsPlan::Path { target, all: false });
    }
    if modes.purge {
        return Ok(LogsPlan::Purge { target });
    }
    Ok(LogsPlan::Show {
        target,
        follow: modes.follow,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(path: bool, purge: bool, prune: bool, follow: bool) -> Modes {
        Modes {
            path,
            purge,
            prune,
            follow,
        }
    }

    #[test]
    fn no_flags_is_a_show() {
        assert_eq!(
            resolve_plan(modes(false, false, false, false), None, None, None, None)
                .unwrap(),
            LogsPlan::Show {
                target: Target::Everything,
                follow: false
            }
        );
    }

    #[test]
    fn follow_is_a_following_show() {
        assert_eq!(
            resolve_plan(
                modes(false, false, false, true),
                Some("web"),
                None,
                None,
                None
            )
            .unwrap(),
            LogsPlan::Show {
                target: Target::Service {
                    service: "web".into(),
                    project: None
                },
                follow: true
            }
        );
    }

    #[test]
    fn two_mode_flags_conflict() {
        assert!(matches!(
            resolve_plan(modes(true, true, false, false), None, None, None, None),
            Err(LogsPlanError::ConflictingModes { .. })
        ));
    }

    #[test]
    fn follow_with_a_mode_is_rejected() {
        assert!(matches!(
            resolve_plan(modes(true, false, false, true), None, None, None, None),
            Err(LogsPlanError::FollowWithMode { mode: "--path" })
        ));
    }

    #[test]
    fn prune_without_bounds_is_rejected() {
        assert_eq!(
            resolve_plan(modes(false, false, true, false), None, None, None, None),
            Err(LogsPlanError::PruneBoundMissing)
        );
    }

    #[test]
    fn prune_with_a_bound_resolves() {
        assert_eq!(
            resolve_plan(
                modes(false, false, true, false),
                None,
                None,
                Some("500MB".into()),
                None
            )
            .unwrap(),
            LogsPlan::Prune {
                max_size: Some("500MB".into()),
                max_age: None
            }
        );
    }

    #[test]
    fn purge_resolves_with_selector() {
        assert_eq!(
            resolve_plan(
                modes(false, true, false, false),
                None,
                Some("demo"),
                None,
                None
            )
            .unwrap(),
            LogsPlan::Purge {
                target: Target::Project {
                    project: "demo".into()
                }
            }
        );
    }
}
