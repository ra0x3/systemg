//! Reconciling a running project against a new manifest — the defensive heart
//! of a whole-config restart.
//!
//! A reconcile is computed and validated *before* anything is torn down:
//!
//! 1. [`ManifestDiff::compute`] — a pure comparison of the running config
//!    against the new one: which services are **added**, **removed**, or
//!    **changed** (command/settings differ). No side effects.
//! 2. The caller validates the whole new manifest first; a bad manifest refuses
//!    the restart wholesale (SG0301) and touches nothing.
//! 3. The caller applies the diff surgically — only added/removed/changed units
//!    are touched, each gated through the came-up ladder. Any unit that misses
//!    its target is a hard failure (SG0302); unchanged units are never bounced.

use std::collections::BTreeSet;

use crate::config::Config;

/// How much of a project a restart is allowed to touch.
///
/// The default reconciles: a restart applies the manifest delta and adopts
/// unchanged units rather than bouncing them, so adding one service does not
/// take the whole stack down and a completed one-shot is not re-run.
///
/// The cost of that is real and is why [`SgCode::RestartTouchedNothing`] exists:
/// a rebuilt binary at an unchanged path hashes identically, so the delta cannot
/// see it. A reconcile that ends up bouncing nothing therefore FAILS rather than
/// reporting success, and `--all` is the way to say "bounce everything anyway".
///
/// [`SgCode::RestartTouchedNothing`]: crate::diag::SgCode::RestartTouchedNothing
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestartScope {
    /// Bounce every unit the manifest declares — `restart --all`.
    Everything,
    /// Bounce only added and changed units, plus their transitive dependents.
    #[default]
    Changed,
    /// Apply a manifest delta on behalf of a registration (`start`, `AddProject`)
    /// rather than a restart. Selects the same units as [`RestartScope::Changed`],
    /// but bouncing nothing is a legitimate outcome here — the caller never asked
    /// for a restart, so it must not be told one failed.
    Register,
}

impl RestartScope {
    /// Whether a run under this scope that bounced nothing is a failure.
    ///
    /// Only a caller that actually asked for a restart is owed that error.
    pub fn expects_a_bounce(self) -> bool {
        matches!(self, RestartScope::Everything | RestartScope::Changed)
    }
}

/// The set of changes needed to bring a running project to a new manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ManifestDiff {
    /// Services present in the new manifest but not the old — to start.
    pub added: BTreeSet<String>,
    /// Services present in the old manifest but not the new — to stop.
    pub removed: BTreeSet<String>,
    /// Services in both whose configuration differs — to restart.
    pub changed: BTreeSet<String>,
}

impl ManifestDiff {
    /// Computes the diff from the currently-running `old` config to the `new`
    /// one. A service is *changed* when its config hash differs; unchanged
    /// services appear in none of the sets and are left running.
    pub fn compute(old: &Config, new: &Config) -> Self {
        let mut diff = ManifestDiff::default();

        for name in new.services.keys() {
            match old.services.get(name) {
                None => {
                    diff.added.insert(name.clone());
                }
                Some(old_svc) => {
                    let new_svc = &new.services[name];
                    if old_svc.compute_hash() != new_svc.compute_hash() {
                        diff.changed.insert(name.clone());
                    }
                }
            }
        }

        for name in old.services.keys() {
            if !new.services.contains_key(name) {
                diff.removed.insert(name.clone());
            }
        }

        diff
    }

    /// Whether the diff touches nothing — the running set already matches.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::{Config, ProjectConfig, ServiceConfig, Version};

    fn svc(command: &str) -> ServiceConfig {
        ServiceConfig {
            command: command.to_string(),
            ..ServiceConfig::default()
        }
    }

    fn config(services: &[(&str, &str)]) -> Config {
        Config {
            version: Version::V2,
            project: ProjectConfig::default(),
            services: services
                .iter()
                .map(|(name, cmd)| (name.to_string(), svc(cmd)))
                .collect::<HashMap<_, _>>(),
            project_dir: None,
            env: None,
            metrics: Default::default(),
            logs: Default::default(),
            status: Default::default(),
        }
    }

    #[test]
    fn identical_configs_diff_to_nothing() {
        let a = config(&[("web", "run"), ("api", "serve")]);
        let b = config(&[("web", "run"), ("api", "serve")]);
        assert!(ManifestDiff::compute(&a, &b).is_empty());
    }

    #[test]
    fn added_removed_and_changed_are_classified() {
        let old = config(&[("web", "run"), ("api", "serve"), ("gone", "x")]);
        let new = config(&[("web", "run"), ("api", "serve-v2"), ("fresh", "y")]);
        let diff = ManifestDiff::compute(&old, &new);
        assert_eq!(diff.added, BTreeSet::from(["fresh".to_string()]));
        assert_eq!(diff.removed, BTreeSet::from(["gone".to_string()]));
        assert_eq!(diff.changed, BTreeSet::from(["api".to_string()]));
    }

    #[test]
    fn unchanged_service_is_not_in_any_set() {
        let old = config(&[("web", "run")]);
        let new = config(&[("web", "run"), ("worker", "loop")]);
        let diff = ManifestDiff::compute(&old, &new);
        assert!(!diff.changed.contains("web"));
        assert!(!diff.removed.contains("web"));
        assert_eq!(diff.added, BTreeSet::from(["worker".to_string()]));
    }
}
