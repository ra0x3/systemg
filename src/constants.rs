//! Constants and configuration values for the systemg daemon.
//!
//! This module centralizes all magic numbers, strings, and configuration values
//! used throughout the daemon to improve maintainability and clarity.

use std::{cmp::Ordering, str::FromStr, time::Duration};

// ============================================================================
// Lock Management and Ordering
// ============================================================================

/// Typed lock abstraction for enforcing proper lock acquisition order in the daemon.
///
/// This enum ensures that locks are always acquired in a consistent order to prevent
/// deadlocks. The ordering is enforced through the `Ord` trait implementation.
///
/// # Lock Acquisition Rules
///
/// Locks MUST be acquired in ascending order of their discriminant values:
/// 1. `Processes` - Child process management
/// 2. `PidFile` - Process ID persistence
/// 3. `StateFile` - Service state persistence
/// 4. `RestartCounts` - Restart attempt tracking
/// 5. `ManualStopFlags` - Manual stop flag tracking
/// 6. `RestartSuppressed` - Restart suppression flags
///
/// # Example
/// ```ignore
/// // Correct: Acquiring in order
/// let _proc_lock = daemon.lock(DaemonLock::Processes)?;
/// let _pid_lock = daemon.lock(DaemonLock::PidFile)?;
///
/// // Incorrect: Would cause deadlock potential
/// // let _pid_lock = daemon.lock(DaemonLock::PidFile)?;
/// // let _proc_lock = daemon.lock(DaemonLock::Processes)?; // WRONG!
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DaemonLock {
    /// Lock for the shared map of running service processes.
    /// Priority: 1 (must be acquired first)
    Processes = 1,

    /// Lock for the PID file containing service to process ID mappings.
    /// Priority: 2
    PidFile = 2,

    /// Lock for the service state file containing status and metadata.
    /// Priority: 3
    StateFile = 3,

    /// Lock for tracking restart attempt counts per service.
    /// Priority: 4
    RestartCounts = 4,

    /// Lock for tracking which services were manually stopped.
    /// Priority: 5
    ManualStopFlags = 5,

    /// Lock for tracking services with suppressed restarts.
    /// Priority: 6 (must be acquired last)
    RestartSuppressed = 6,
}

impl DaemonLock {
    /// Returns the numeric priority of this lock type.
    /// Lower numbers must be acquired before higher numbers.
    pub const fn priority(&self) -> u8 {
        *self as u8
    }

    /// Returns a human-readable name for this lock type.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Processes => "processes",
            Self::PidFile => "pid_file",
            Self::StateFile => "state_file",
            Self::RestartCounts => "restart_counts",
            Self::ManualStopFlags => "manual_stop_flags",
            Self::RestartSuppressed => "restart_suppressed",
        }
    }

    /// Checks if acquiring `other` after `self` would violate lock ordering.
    /// Returns `true` if the acquisition order is valid.
    pub const fn can_acquire_after(&self, other: &Self) -> bool {
        self.priority() > other.priority()
    }
}

impl PartialOrd for DaemonLock {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DaemonLock {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority().cmp(&other.priority())
    }
}

// ============================================================================
// File System Constants
// ============================================================================

/// Name of the PID file stored in the state directory.
/// Contains mappings of service names to process IDs.
pub const PID_FILE_NAME: &str = "pid.json";

/// Lock file suffix for PID file to ensure exclusive access.
pub const PID_LOCK_SUFFIX: &str = ".lock";

/// Name of the service state file stored in the state directory.
/// Contains the current state and metadata for all managed services.
pub const STATE_FILE_NAME: &str = "state.json";

// ============================================================================
// Shell Execution Constants
// ============================================================================

/// Default shell used for executing service commands and hooks.
pub const DEFAULT_SHELL: &str = "sh";

/// Shell argument flag for executing command strings.
pub const SHELL_COMMAND_FLAG: &str = "-c";

// ============================================================================
// Process Management Timing
// ============================================================================

/// Number of checks to perform when waiting for a process to become ready.
/// Used in conjunction with PROCESS_CHECK_INTERVAL.
pub const PROCESS_READY_CHECKS: usize = 10;

/// Interval between process readiness checks.
pub const PROCESS_CHECK_INTERVAL: Duration = Duration::from_millis(100);

/// Maximum time to wait for a service to start before timing out.
/// Applied during service initialization and health checks.
pub const SERVICE_START_TIMEOUT: Duration = Duration::from_secs(5);

/// Polling interval when waiting for service state changes.
pub const SERVICE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Number of attempts to verify a service is running after restart.
pub const POST_RESTART_VERIFY_ATTEMPTS: usize = 2;

/// Delay between post-restart verification attempts.
pub const POST_RESTART_VERIFY_DELAY: Duration = Duration::from_millis(200);

// ============================================================================
// Logging and Output Constants
// ============================================================================

/// Maximum number of log lines to display in status output.
/// Prevents overwhelming the terminal with excessive log data.
pub const MAX_STATUS_LOG_LINES: usize = 50;

/// Buffer size for log output streams (stdout/stderr).
pub const LOG_BUFFER_SIZE: usize = 8192;

// ============================================================================
// Hook Execution Constants
// ============================================================================

/// Format string for hook labels combining stage and outcome.
/// Example: "pre_start.pending", "post_start.success"
pub const HOOK_LABEL_FORMAT: &str = "{}.{}";

// ============================================================================
// Linux-specific Constants
// ============================================================================

// ============================================================================
// Error Messages and Formats
// ============================================================================

/// Error message for malformed environment file lines.
pub const ENV_FILE_MALFORMED_MSG: &str =
    "Ignoring malformed line in env file for '{}': {}";

/// Error message for environment file read failures.
pub const ENV_FILE_READ_ERROR_MSG: &str = "Failed to read env file for '{}': {}";

/// Error message for hook timeout parsing failures.
pub const HOOK_TIMEOUT_PARSE_ERROR_MSG: &str =
    "Invalid timeout '{}' for hook {} on '{}': {}";

/// Error message for insufficient process signal permissions.
pub const INSUFFICIENT_SIGNAL_PERMISSIONS_MSG: &str =
    "Insufficient permissions to signal process group {} for '{}'";

/// Error message for process tree termination failures.
pub const PROCESS_TREE_TERM_FAILURE_MSG: &str =
    "Failed to terminate process tree rooted at PID {} for '{}'";

// ============================================================================
// Service Management Constants
// ============================================================================

/// Deployment strategies for service restarts.
///
/// This enum provides type-safe handling of deployment strategies, ensuring
/// that only valid strategies can be used throughout the codebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentStrategy {
    /// Rolling deployment: Start new instance before stopping old one.
    /// Useful for zero-downtime deployments where port availability is managed.
    Rolling,

    /// Immediate deployment: Stop old instance then start new one.
    /// Traditional restart approach with potential brief downtime.
    Immediate,
}

impl DeploymentStrategy {
    /// Convert the deployment strategy to its string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rolling => "rolling",
            Self::Immediate => "immediate",
        }
    }
}

impl FromStr for DeploymentStrategy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "rolling" => Ok(Self::Rolling),
            "immediate" => Ok(Self::Immediate),
            _ => Err(format!("Unknown deployment strategy: {}", s)),
        }
    }
}

impl Default for DeploymentStrategy {
    fn default() -> Self {
        Self::Immediate
    }
}

/// Default deployment strategy when not specified in configuration.
pub const DEFAULT_DEPLOYMENT_STRATEGY: &str = "immediate";

/// Rolling deployment strategy identifier.
pub const ROLLING_DEPLOYMENT: &str = "rolling";

/// Immediate deployment strategy identifier.
pub const IMMEDIATE_DEPLOYMENT: &str = "immediate";

/// Message logged when skipping cron-managed services during bulk operations.
pub const SKIP_CRON_SERVICE_MSG: &str = "Skipping cron-managed service '{}' during bulk start; scheduled execution will launch it";

/// Message logged when skipping cron services during restart.
pub const SKIP_CRON_RESTART_MSG: &str = "Skipping cron-managed service '{}' during restart; scheduled execution will launch it";
