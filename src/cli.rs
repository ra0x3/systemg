//! Command-line interface for Systemg.
use clap::{Parser, Subcommand};
use std::str::FromStr;
use tracing::level_filters::LevelFilter;

/// Wrapper around `LevelFilter` so clap can parse log levels from either
/// string names ("info", "debug", etc.) or numeric shorthands (0-5).
#[derive(Clone, Copy, Debug)]
pub struct LogLevelArg(LevelFilter);

impl LogLevelArg {
    /// String representation suitable for `RUST_LOG`.
    pub fn as_str(&self) -> &'static str {
        match self.0 {
            LevelFilter::OFF => "off",
            LevelFilter::ERROR => "error",
            LevelFilter::WARN => "warn",
            LevelFilter::INFO => "info",
            LevelFilter::DEBUG => "debug",
            LevelFilter::TRACE => "trace",
        }
    }
}

impl FromStr for LogLevelArg {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("log level cannot be empty".into());
        }

        if let Ok(number) = trimmed.parse::<u8>() {
            let level = match number {
                0 => LevelFilter::OFF,
                1 => LevelFilter::ERROR,
                2 => LevelFilter::WARN,
                3 => LevelFilter::INFO,
                4 => LevelFilter::DEBUG,
                5 => LevelFilter::TRACE,
                _ => {
                    return Err(format!(
                        "unsupported log level number '{number}' (expected 0-5)"
                    ));
                }
            };

            return Ok(LogLevelArg(level));
        }

        let lowercase = trimmed.to_ascii_lowercase();
        let level = match lowercase.as_str() {
            "off" => Some(LevelFilter::OFF),
            "error" | "err" => Some(LevelFilter::ERROR),
            "warn" | "warning" => Some(LevelFilter::WARN),
            "info" | "information" => Some(LevelFilter::INFO),
            "debug" => Some(LevelFilter::DEBUG),
            "trace" => Some(LevelFilter::TRACE),
            _ => None,
        }
        .ok_or_else(|| format!("invalid log level '{trimmed}'"))?;

        Ok(LogLevelArg(level))
    }
}

/// Command-line interface for Systemg.
#[derive(Parser)]
#[command(name = "systemg", version, author)]
#[command(about = "A lightweight process manager for system services", long_about = None)]
pub struct Cli {
    /// Override the logging verbosity for this invocation only.
    #[arg(long, value_name = "LEVEL", global = true)]
    pub log_level: Option<LogLevelArg>,

    /// The command to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// Available commands for systemg.
#[derive(Subcommand)]
pub enum Commands {
    /// Start the process manager with the given configuration.
    Start {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// Whether to daemonize systemg.
        #[arg(long)]
        daemonize: bool,

        /// Optionally start only the named service.
        #[arg(short, long)]
        service: Option<String>,
    },

    /// Stop the currently running process manager.
    Stop {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// Name of service to stop (optional).
        #[arg(short, long)]
        service: Option<String>,
    },

    /// Restart the process manager, optionally specifying a new configuration file.
    Restart {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// Optionally restart only the named service.
        #[arg(short, long)]
        service: Option<String>,

        /// Start the supervisor before restarting if it isn't already running.
        #[arg(long)]
        daemonize: bool,
    },

    /// Show the status of currently running services.
    Status {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// Optionally specify a service name to check its status.
        #[arg(short, long)]
        service: Option<String>,

        /// Show all services including orphaned state (services not in current config).
        #[arg(long)]
        all: bool,
    },

    /// Show logs for a specific service.
    Logs {
        /// The name of the service whose logs should be displayed (optional).
        service: Option<String>,

        /// Number of lines to show (default: 50).
        #[arg(short, long, default_value = "50")]
        lines: usize,

        /// Kind of logs to show: stdout, stderr, or supervisor (default: all).
        #[arg(short = 'k', long)]
        kind: Option<String>,
    },

    /// Purge all systemg state and runtime files.
    Purge,
}

/// Parses command-line arguments and returns a `Cli` struct.
pub fn parse_args() -> Cli {
    Cli::parse()
}
