//! Command-line interface for Systemg.
use std::{fmt, str::FromStr};

use clap::{Parser, Subcommand};
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

/// Type of logs to display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogKind {
    /// Standard output logs
    Stdout,
    /// Standard error logs
    #[default]
    Stderr,
    /// Supervisor logs
    Supervisor,
}

impl LogKind {
    /// String representation for file paths and display.
    pub fn as_str(&self) -> &'static str {
        match self {
            LogKind::Stdout => "stdout",
            LogKind::Stderr => "stderr",
            LogKind::Supervisor => "supervisor",
        }
    }
}

impl fmt::Display for LogKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for LogKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "stdout" => Ok(LogKind::Stdout),
            "stderr" => Ok(LogKind::Stderr),
            "supervisor" => Ok(LogKind::Supervisor),
            _ => Err(format!(
                "invalid log kind '{}', must be one of: stdout, stderr, supervisor",
                s
            )),
        }
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

    /// Opt into privileged system mode. Requires running as root.
    #[arg(long = "sys", global = true)]
    pub sys: bool,

    /// Drop privileges after performing privileged setup.
    #[arg(long = "drop-privileges", global = true)]
    pub drop_privileges: bool,

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

        /// Emit machine-readable JSON output instead of a table.
        #[arg(long)]
        json: bool,

        /// Disable ANSI colors in output.
        #[arg(long = "no-color")]
        no_color: bool,

        /// Show full command lines in the status table.
        #[arg(long = "full-cmd")]
        full_cmd: bool,

        /// Continuously refresh output at the provided interval (e.g., "5", "1s", "2m").
        #[arg(long, value_name = "DURATION")]
        stream: Option<String>,
    },

    /// Inspect a single service or cron unit in detail.
    Inspect {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// Name or hash of the unit to inspect.
        unit: String,

        /// Emit machine-readable JSON output instead of a report.
        #[arg(long)]
        json: bool,

        /// Disable ANSI colors in output.
        #[arg(long = "no-color")]
        no_color: bool,

        /// Continuously refresh output and use a rolling metrics window (e.g., "5", "1s", "2m").
        #[arg(long, value_name = "DURATION")]
        stream: Option<String>,
    },

    /// Show logs for a specific service.
    Logs {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// The name of the service whose logs should be displayed (optional).
        #[arg(short, long)]
        service: Option<String>,

        /// Number of lines to show (default: 50).
        #[arg(short, long, default_value = "50")]
        lines: usize,

        /// Kind of logs to show: stdout, stderr, or supervisor (default: stdout).
        #[arg(short = 'k', long, default_value_t = LogKind::default())]
        kind: LogKind,

        /// Continuously refresh output at the provided interval (e.g., "5", "1s", "2m").
        #[arg(long, value_name = "DURATION")]
        stream: Option<String>,
    },

    /// Purge all systemg state and runtime files.
    Purge,

    /// Spawn a dynamic child process from a parent service.
    Spawn {
        /// Name for the spawned child process.
        #[arg(long)]
        name: String,

        /// Time-to-live in seconds (optional).
        #[arg(long)]
        ttl: Option<u64>,

        /// Parent process ID (defaults to caller's parent PID if not specified).
        #[arg(long)]
        parent_pid: Option<u32>,

        /// Override the logging verbosity for the spawned process.
        #[arg(long, value_name = "LEVEL")]
        log_level: Option<LogLevelArg>,

        /// Command and arguments to execute.
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
}

/// Parses command-line arguments and returns a `Cli` struct.
pub fn parse_args() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_accepts_stream() {
        let cli = Cli::try_parse_from(["sysg", "status", "--stream", "5"]).unwrap();
        match cli.command {
            Commands::Status { stream, .. } => assert_eq!(stream.as_deref(), Some("5")),
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn inspect_accepts_stream() {
        let cli =
            Cli::try_parse_from(["sysg", "inspect", "demo", "--stream", "2m"]).unwrap();
        match cli.command {
            Commands::Inspect { stream, .. } => {
                assert_eq!(stream.as_deref(), Some("2m"))
            }
            _ => panic!("expected inspect command"),
        }
    }

    #[test]
    fn logs_accepts_stream() {
        let cli =
            Cli::try_parse_from(["sysg", "logs", "--service", "demo", "--stream", "1s"])
                .unwrap();
        match cli.command {
            Commands::Logs { stream, .. } => {
                assert_eq!(stream.as_deref(), Some("1s"))
            }
            _ => panic!("expected logs command"),
        }
    }

    #[test]
    fn status_rejects_watch() {
        assert!(Cli::try_parse_from(["sysg", "status", "--watch", "5"]).is_err());
    }

    #[test]
    fn inspect_rejects_window() {
        assert!(
            Cli::try_parse_from(["sysg", "inspect", "demo", "--window", "5s"]).is_err()
        );
    }
}
