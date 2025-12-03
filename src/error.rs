//! Error handling for systemg.
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

    /// Error when a dependency has failed during startup.
    #[error("Service '{service}' cannot start because dependency '{dependency}' failed")]
    DependencyFailed {
        /// The service that cannot be started.
        service: String,
        /// The dependency that failed.
        dependency: String,
    },

    /// Error when a dependency reference is undefined in the configuration.
    #[error("Service '{service}' declares unknown dependency '{dependency}'")]
    UnknownDependency {
        /// The service with an invalid dependency reference.
        service: String,
        /// The missing dependency name.
        dependency: String,
    },

    /// Error when dependency graph contains a cycle.
    #[error("Detected dependency cycle: {cycle}")]
    DependencyCycle {
        /// Human-readable cycle description (e.g. `a -> b -> a`).
        cycle: String,
    },

    /// Error for poisoned mutex.
    #[error("Mutex is poisoned: {0}")]
    MutexPoisonError(String),

    /// Error for PID file.
    #[error("PID file error: {0}")]
    PidFileError(#[from] PidFileError),

    /// Error for service state file.
    #[error("Service state error: {0}")]
    ServiceStateError(#[from] ServiceStateError),

    /// Error for logs manager.
    #[error("Service not found in PID file")]
    ErrNo(#[from] nix::errno::Errno),

    /// Error when services fail to remain running after a restart completes.
    #[error("Service(s) failed to remain running after restart: {services:?}")]
    ServicesNotRunning {
        /// List of services that were expected to be running but were not.
        services: Vec<String>,
    },
}

/// Implement the `From` trait to convert a `std::sync::PoisonError` into a `ProcessManagerError`.
impl<T> From<std::sync::PoisonError<T>> for ProcessManagerError {
    /// Converts a `std::sync::PoisonError` into a `ProcessManagerError`.
    fn from(err: std::sync::PoisonError<T>) -> Self {
        ProcessManagerError::MutexPoisonError(err.to_string())
    }
}

/// Error type for PID file operations.
#[derive(Debug, Error)]
pub enum PidFileError {
    /// Error writing to a PID file.
    #[error("Failed to read PID file: {0}")]
    ReadError(#[from] std::io::Error),

    /// Error writing to a PID file.
    #[error("Failed to parse PID file: {0}")]
    ParseError(#[from] serde_json::Error),

    /// Error writing to a PID file.
    #[error("Service not found in PID file")]
    ServiceNotFound,
}

/// Error type for persistent service state file operations.
#[derive(Debug, Error)]
pub enum ServiceStateError {
    /// Error reading the state file from disk.
    #[error("Failed to read service state file: {0}")]
    ReadError(#[from] std::io::Error),

    /// Error parsing JSON contents of the state file.
    #[error("Failed to parse service state file: {0}")]
    ParseError(#[from] serde_json::Error),

    /// Attempted to update or remove a non-existent service entry.
    #[error("Service not found in state file")]
    ServiceNotFound,
}

/// Error type for logs manager operations.
#[derive(Debug, Error)]
pub enum LogsManagerError {
    /// Error parsing YAML configuration.
    #[error("Service '{0}' not found in PID file")]
    ServiceNotFound(String),

    /// Error executing the tail command for logs.
    #[error("Log file unavailable for PID {0}")]
    LogUnavailable(u32),

    /// Error while tailing logs, such as command failure.
    #[error("Log tailing failed: {0}")]
    LogProcessError(#[from] std::io::Error),

    /// Error when the tail command exits with a non-zero status.
    #[error("Log tail command exited with status {0:?}")]
    TailCommandFailed(Option<i32>),

    /// Error when the log file is unavailable.
    #[error("Unsupported platform for log tailing")]
    UnsupportedPlatform,
}
