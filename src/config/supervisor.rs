//! Supervisor-level configuration — distinct from any project's manifest.
//!
//! The supervisor is impartial infrastructure; it owns no project. What it DOES
//! own is a small set of supervisor-wide defaults a user can tune, persisted as
//! `supervisor.xml` in the state directory (alongside `pid.xml`/`state.xml`).
//! Today that is the default log-rotation caps applied to every service that
//! does not override them. The file is created with sensible defaults on first
//! supervisor start if absent, so the supervisor is zero-config by default.

use std::{path::PathBuf, time::Duration};

use quick_xml::de::from_str as xml_from_str;
use serde::{Deserialize, Serialize};

use super::{LOGS_DEFAULT_MAX_BYTES, LOGS_DEFAULT_MAX_FILES};
use crate::{
    constants::{PRE_START_TIMEOUT, SERVICE_START_STABILITY, STOP_VERIFY_TIMEOUT},
    runtime, xml,
};

/// File name of the supervisor config in the state directory.
pub const SUPERVISOR_CONFIG_FILE: &str = "supervisor.xml";

/// Default log-rotation caps for services that do not set their own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorLogDefaults {
    /// Maximum active log-file size (bytes) before a service log rotates.
    pub max_bytes: u64,
    /// Number of rotated files retained per service log.
    pub max_files: usize,
}

impl Default for SupervisorLogDefaults {
    fn default() -> Self {
        Self {
            max_bytes: LOGS_DEFAULT_MAX_BYTES,
            max_files: LOGS_DEFAULT_MAX_FILES,
        }
    }
}

/// Operator-controlled lifecycle timeouts shared by supervised projects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorTimeouts {
    /// Default maximum runtime for a deployment pre-start command.
    pub pre_start_secs: u64,
    /// Survival window for services without an explicit health check.
    pub startup_stability_ms: u64,
    /// Maximum wait for a terminated process to disappear.
    pub stop_verify_secs: u64,
}

impl Default for SupervisorTimeouts {
    /// Returns the built-in lifecycle timeout policy.
    fn default() -> Self {
        Self {
            pre_start_secs: PRE_START_TIMEOUT.as_secs(),
            startup_stability_ms: SERVICE_START_STABILITY.as_millis() as u64,
            stop_verify_secs: STOP_VERIFY_TIMEOUT.as_secs(),
        }
    }
}

impl SupervisorTimeouts {
    /// Returns the configured pre-start timeout.
    pub fn pre_start_timeout(&self) -> Duration {
        Duration::from_secs(self.pre_start_secs.max(1))
    }

    /// Returns the configured no-health-check startup stability window.
    pub fn startup_stability(&self) -> Duration {
        Duration::from_millis(self.startup_stability_ms)
    }

    /// Returns the configured stop verification timeout.
    pub fn stop_verify_timeout(&self) -> Duration {
        Duration::from_secs(self.stop_verify_secs.max(1))
    }
}

/// The supervisor's own configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename = "supervisor")]
pub struct SupervisorConfig {
    /// Default log-rotation caps for all services (overridable per service).
    #[serde(default)]
    pub logs: SupervisorLogDefaults,
    /// Lifecycle timeout defaults applied to every managed project.
    #[serde(default)]
    pub timeouts: SupervisorTimeouts,
}

impl SupervisorConfig {
    /// The on-disk path of the supervisor config in the current state dir.
    pub fn path() -> PathBuf {
        runtime::state_dir().join(SUPERVISOR_CONFIG_FILE)
    }

    /// Loads the supervisor config, creating it with defaults if it does not yet
    /// exist. A malformed file falls back to defaults (never fatal — the
    /// supervisor must still boot) but is not overwritten, so a user can fix it.
    pub fn load_or_create() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => match xml_from_str::<Self>(&contents) {
                Ok(config) => {
                    if xml::is_compact_nested(&contents)
                        && let Err(write_err) = config.write()
                    {
                        tracing::warn!(
                            "could not normalize supervisor config {}: {write_err}",
                            path.display()
                        );
                    }
                    config
                }
                Err(err) => {
                    tracing::warn!(
                        "supervisor config {} is invalid ({err}); using defaults",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let config = Self::default();
                if let Err(write_err) = config.write() {
                    tracing::warn!(
                        "could not write default supervisor config {}: {write_err}",
                        path.display()
                    );
                }
                config
            }
            Err(err) => {
                tracing::warn!(
                    "could not read supervisor config {} ({err}); using defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Writes the config to its on-disk path (owner-only).
    pub fn write(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            runtime::create_private_dir(parent)?;
        }
        let output = xml::to_string(self).map_err(std::io::Error::other)?;
        runtime::write_private_file(&path, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Verifies defaults preserve the historical built-in policy.
    fn defaults_match_the_hardcoded_log_caps() {
        let cfg = SupervisorConfig::default();
        assert_eq!(cfg.logs.max_bytes, LOGS_DEFAULT_MAX_BYTES);
        assert_eq!(cfg.logs.max_files, LOGS_DEFAULT_MAX_FILES);
        assert_eq!(cfg.timeouts.pre_start_timeout(), PRE_START_TIMEOUT);
        assert_eq!(cfg.timeouts.startup_stability(), SERVICE_START_STABILITY);
        assert_eq!(cfg.timeouts.stop_verify_timeout(), STOP_VERIFY_TIMEOUT);
    }

    #[test]
    /// Verifies every supervisor setting survives XML serialization.
    fn roundtrips_through_xml() {
        let cfg = SupervisorConfig {
            logs: SupervisorLogDefaults {
                max_bytes: 42,
                max_files: 7,
            },
            timeouts: SupervisorTimeouts {
                pre_start_secs: 8,
                startup_stability_ms: 90,
                stop_verify_secs: 10,
            },
        };
        let output = xml::to_string(&cfg).unwrap();
        let back: SupervisorConfig = xml_from_str(&output).unwrap();
        assert_eq!(back.logs.max_bytes, 42);
        assert_eq!(back.logs.max_files, 7);
        assert_eq!(back.timeouts.pre_start_secs, 8);
        assert_eq!(back.timeouts.startup_stability_ms, 90);
        assert_eq!(back.timeouts.stop_verify_secs, 10);
    }

    #[test]
    /// Verifies legacy configs receive defaults for newly added timeouts.
    fn compact_legacy_config_receives_timeout_defaults() {
        let config: SupervisorConfig =
            xml_from_str("<supervisor><logs><max_bytes>42</max_bytes><max_files>7</max_files></logs></supervisor>")
                .unwrap();

        assert_eq!(config.timeouts.pre_start_timeout(), PRE_START_TIMEOUT);
        assert_eq!(config.timeouts.startup_stability(), SERVICE_START_STABILITY);
        assert_eq!(config.timeouts.stop_verify_timeout(), STOP_VERIFY_TIMEOUT);
    }
}
