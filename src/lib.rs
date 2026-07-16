//! Lightweight process manager for Unix services.

#![warn(unused_crate_dependencies)]
#[cfg(test)]
use assert_cmd as _;
use crossterm as _;
use ctrlc as _;
#[cfg(target_os = "linux")]
use openssl_sys as _;
#[cfg(test)]
use predicates as _;
use strum as _;
#[cfg(test)]
use tempfile as _;
use terminal_size as _;

/// ASCII charting.
pub mod charting;

/// CLI parsing.
pub mod cli;

/// Config loading.
pub mod config;

/// Configuration validation and diagnostics.
pub mod validate;

/// Constants.
pub mod constants;

/// rustc-style diagnostics: structured, colored, actionable failure reports.
pub mod diag;

/// Metrics.
pub mod metrics;

/// Cron scheduler.
pub mod cron;

/// Process daemon.
pub mod daemon;

/// IPC with supervisor.
pub mod ipc;

/// Current-operation tracking for the control plane.
pub mod opslot;

/// Reconciles supervisor bookkeeping against procfs and port ownership.
pub mod reconcile;

/// Errors.
pub mod error;

/// Log streaming.
pub mod logs;

/// Status tracking.
pub mod status;

/// Supervisor daemon.
pub mod supervisor;

/// Dynamic spawn management.
pub mod spawn;

/// Test utils.
#[doc(hidden)]
pub mod test_utils;

/// Runtime paths and modes.
pub mod runtime;

/// Per-project on-disk state layout.
pub mod state_store;

/// The shared command selector (`-s`/`-p`) resolution.
pub mod selector;

/// The `start` command, rebuilt from first principles.
pub mod start;

/// The `stop` command, rebuilt from first principles.
pub mod stop;

/// The `restart` command, rebuilt from first principles.
pub mod restart;

/// The `purge` command, rebuilt from first principles.
pub mod purge;

/// The `logs` command's plan layer, rebuilt from first principles.
pub mod logs_cmd;

/// The `inspect` command's plan layer, rebuilt from first principles.
pub mod inspect;

/// Privilege dropping.
pub mod privilege;
