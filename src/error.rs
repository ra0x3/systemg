use thiserror::Error;

/// Defines all possible errors that can occur in the process manager.
#[derive(Debug, Error)]
pub enum ProcessManagerError {
    /// Error reading or accessing a configuration file.
    #[error("Failed to read config file: {0}")]
    ConfigReadError(#[from] std::io::Error),

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
        source: std::io::Error,
    },

    /// Error stopping a service process.
    #[error("Failed to stop service '{service}': {source}")]
    ServiceStopError {
        /// The service name that failed to stop.
        service: String,
        /// The underlying error that occurred.
        #[source]
        source: std::io::Error,
    },

    /// Error executing a lifecycle hook (e.g., on_start, on_error).
    #[error("Failed to execute hook for service '{service}': {source}")]
    HookExecutionError {
        /// The service name whose hook execution failed.
        service: String,
        /// The underlying error that occurred.
        #[source]
        source: std::io::Error,
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

    /// Error for PID file.
    #[error("PID file error: {0}")]
    PidFileError(#[from] PidFileError),
}

impl<T> From<std::sync::PoisonError<T>> for ProcessManagerError {
    fn from(err: std::sync::PoisonError<T>) -> Self {
        ProcessManagerError::MutexPoisonError(err.to_string())
    }
}

#[derive(Debug, Error)]
pub enum PidFileError {
    #[error("Failed to read PID file: {0}")]
    ReadError(#[from] std::io::Error),

    #[error("Failed to parse PID file: {0}")]
    ParseError(#[from] serde_json::Error),

    #[error("Service not found in PID file")]
    ServiceNotFound,
}
