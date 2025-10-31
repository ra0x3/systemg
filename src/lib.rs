//! Systemg is a lightweight process management tool used to quickly run, monitor, and
//! manage background services on Unix-like operating systems. It provides a simple
//! CLI interface to start, stop, and check the status of services, along with logging
//! capabilities and lifecycle hooks for service management.

/// CLI interface.
pub mod cli;

/// Configuration management.
pub mod config;

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
