//! Systemg is a lightweight process management tool used to quickly run, monitor, and
//! manage background services on Unix-like operating systems. It provides a simple
//! CLI interface to start, stop, and check the status of services, along with logging
//! capabilities and lifecycle hooks for service management.

#![warn(unused_crate_dependencies)]
// These dependencies are only used in the binary (src/bin/main.rs)
use ctrlc as _;
use strum as _;
use tracing_subscriber as _;

// OpenSSL is only needed for static linking on Linux
#[cfg(target_os = "linux")]
use openssl_sys as _;

// Test dependencies are only used in test code
#[cfg(test)]
use assert_cmd as _;
#[cfg(test)]
use predicates as _;
#[cfg(test)]
use tempfile as _;

/// CLI interface.
pub mod cli;

/// Configuration management.
pub mod config;

/// Cron scheduling for services.
pub mod cron;

/// Daemon process management.
pub mod daemon;

/// IPC helpers for communicating with the resident supervisor.
pub mod ipc;

/// Error handling.
pub mod error;

/// Logs management.
pub mod logs;

/// Status manager.
pub mod status;

/// Supervisor runtime that powers daemonised deployments.
pub mod supervisor;

/// Test utilities for synchronizing environment variable modifications.
#[doc(hidden)]
pub mod test_utils;

/// Runtime context helpers for managing directories and privilege mode.
pub mod runtime;

/// Privilege management and resource handling utilities.
pub mod privilege;
