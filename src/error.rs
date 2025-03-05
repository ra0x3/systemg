use std::io;
use thiserror::Error;

/// Defines all possible errors that can occur in the process manager.
#[derive(Debug, Error)]
pub enum ProcessManagerError {
    /// Error reading or accessing a configuration file.
    #[error("Failed to read config file: {0}")]
    ConfigReadError(#[from] io::Error),

    /// Error parsing YAML configuration.
    #[error("Invalid YAML format: {0}")]
    ConfigParseError(#[from] serde_yaml::Error),

    /// Error spawning a service process.
    #[error("Failed to start service '{service}': {source}")]
    ServiceStartError {
        /// The service name that failed to start.
        service: String,
        /// The underlying error that occurred.
        #[source]
        source: io::Error,
    },

    /// Error stopping a service process.
    #[error("Failed to stop service '{service}': {source}")]
    ServiceStopError {
        /// The service name that failed to stop.
        service: String,
        /// The underlying error that occurred.
        #[source]
        source: io::Error,
    },

    /// Error executing a lifecycle hook (e.g., on_start, on_error).
    #[error("Failed to execute hook for service '{service}': {source}")]
    HookExecutionError {
        /// The service name whose hook execution failed.
        service: String,
        /// The underlying error that occurred.
        #[source]
        source: io::Error,
    },

    /// Error when a required dependency service is not running.
    #[error(
        "Service '{service}' is waiting for an unavailable dependency: '{dependency}'"
    )]
    DependencyError {
        /// The service that is waiting.
        service: String,
        /// The missing dependency.
        dependency: String,
    },

    /// Error for poisoned mutex.
    #[error("Mutex is poisoned: {0}")]
    MutexPoisonError(String),
}

impl<T> From<std::sync::PoisonError<T>> for ProcessManagerError {
    fn from(err: std::sync::PoisonError<T>) -> Self {
        ProcessManagerError::MutexPoisonError(err.to_string())
    }
}
