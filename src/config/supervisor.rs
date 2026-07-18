//! Supervisor-level configuration — distinct from any project's manifest.
//!
//! The supervisor is impartial infrastructure; it owns no project. What it DOES
//! own is a small set of supervisor-wide defaults a user can tune, persisted as
//! `supervisor.xml` in the state directory (alongside `pid.xml`/`state.xml`).
//! Today that is the default log-rotation caps applied to every service that
//! does not override them. The file is created with sensible defaults on first
//! supervisor start if absent, so the supervisor is zero-config by default.

use std::path::PathBuf;

use quick_xml::{de::from_str as xml_from_str, se::to_string as xml_to_string};
use serde::{Deserialize, Serialize};

use super::{LOGS_DEFAULT_MAX_BYTES, LOGS_DEFAULT_MAX_FILES};
use crate::runtime;

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

/// The supervisor's own configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename = "supervisor")]
pub struct SupervisorConfig {
    /// Default log-rotation caps for all services (overridable per service).
    #[serde(default)]
    pub logs: SupervisorLogDefaults,
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
                Ok(config) => config,
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
        let xml =
            xml_to_string(self).map_err(|err| std::io::Error::other(err.to_string()))?;
        runtime::write_private_file(&path, xml)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_hardcoded_log_caps() {
        let cfg = SupervisorConfig::default();
        assert_eq!(cfg.logs.max_bytes, LOGS_DEFAULT_MAX_BYTES);
        assert_eq!(cfg.logs.max_files, LOGS_DEFAULT_MAX_FILES);
    }

    #[test]
    fn roundtrips_through_xml() {
        let cfg = SupervisorConfig {
            logs: SupervisorLogDefaults {
                max_bytes: 42,
                max_files: 7,
            },
        };
        let xml = xml_to_string(&cfg).unwrap();
        let back: SupervisorConfig = xml_from_str(&xml).unwrap();
        assert_eq!(back.logs.max_bytes, 42);
        assert_eq!(back.logs.max_files, 7);
    }
}
