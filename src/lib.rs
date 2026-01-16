//! Lightweight process manager for Unix services.

#![warn(unused_crate_dependencies)]
// These dependencies are only used in the binary (src/bin/main.rs)
// Test dependencies are only used in test code
#[cfg(test)]
use assert_cmd as _;
use ctrlc as _;
// OpenSSL is only needed for static linking on Linux
#[cfg(target_os = "linux")]
use openssl_sys as _;
#[cfg(test)]
use predicates as _;
use strum as _;
#[cfg(test)]
use tempfile as _;
use tracing_subscriber as _;

/// Gnuplot charting.
pub mod charting;

/// CLI parsing.
pub mod cli;

/// Config loading.
pub mod config;

/// Constants.
pub mod constants;

/// Metrics.
pub mod metrics;

/// Cron scheduler.
pub mod cron;

/// Process daemon.
pub mod daemon;

/// IPC with supervisor.
pub mod ipc;

/// Errors.
pub mod error;

/// Log streaming.
pub mod logs;

/// Status tracking.
pub mod status;

/// Supervisor daemon.
pub mod supervisor;

/// Test utils.
#[doc(hidden)]
pub mod test_utils;

/// Runtime paths and modes.
pub mod runtime;

/// Privilege dropping.
pub mod privilege;
