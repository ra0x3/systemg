//! Command-line interface for Systemg.
use std::{fmt, str::FromStr};

use clap::{Parser, Subcommand, ValueEnum};
use tracing::level_filters::LevelFilter;

/// Wrapper around `LevelFilter` so clap can parse log levels from either
/// string names ("info", "debug", etc.) or numeric shorthands (0-5).
#[derive(Clone, Copy, Debug)]
pub struct LogLevelArg(LevelFilter);

impl LogLevelArg {
    /// Returns the string representation suitable for `RUST_LOG`.
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

    /// Handles from str.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogKind {
    /// Standard output logs
    Stdout,
    /// Standard error logs
    Stderr,
    /// Supervisor logs
    Supervisor,
}

impl LogKind {
    /// Returns the string representation for file paths and display.
    pub fn as_str(&self) -> &'static str {
        match self {
            LogKind::Stdout => "stdout",
            LogKind::Stderr => "stderr",
            LogKind::Supervisor => "supervisor",
        }
    }
}

impl fmt::Display for LogKind {
    /// Handles fmt.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for LogKind {
    type Err = String;

    /// Handles from str.
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

/// Machine-readable output formats supported by status and inspect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Emit JSON output.
    Json,
    /// Emit XML output.
    Xml,
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

        /// Project id to target.
        #[arg(short = 'p', long)]
        project: Option<String>,

        /// Name for ad-hoc units or child-start requests.
        #[arg(long)]
        name: Option<String>,

        /// Time-to-live in seconds for child-start requests.
        #[arg(long)]
        ttl: Option<u64>,

        /// Parent process ID for child-start requests.
        #[arg(long)]
        parent_pid: Option<u32>,

        /// Explicitly run in child-start mode (requires --parent-pid).
        #[arg(long)]
        child: bool,

        /// Pipe stderr output from supervised processes to stdout.
        ///
        /// When enabled, stderr from all supervised processes will be redirected to
        /// the stdout of the sysg start command with the format: \[service_name:stderr\] \<line\>
        ///
        /// This is useful for:
        /// • Debugging services during development
        /// • Capturing all error output in CI/CD pipelines
        /// • Monitoring service errors in real-time
        ///
        /// Note: This only affects foreground mode. In daemonized mode, stderr is still
        /// written to log files.
        #[arg(long)]
        stderr: bool,

        /// Ad-hoc command and arguments to supervise without a manifest.
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },

    /// Stop the currently running process manager.
    Stop {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// Name of service to stop (optional).
        #[arg(short, long)]
        service: Option<String>,

        /// Project id to target.
        #[arg(short = 'p', long)]
        project: Option<String>,

        /// Shut down the resident supervisor and all registered projects.
        #[arg(long)]
        supervisor: bool,
    },

    /// Restart the process manager, optionally specifying a new configuration file.
    Restart {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// Optionally restart only the named service.
        #[arg(short, long)]
        service: Option<String>,

        /// Project id to target.
        #[arg(short = 'p', long)]
        project: Option<String>,

        /// Start the supervisor before restarting if it isn't already running.
        #[arg(long)]
        daemonize: bool,
    },

    /// Show the status of currently running services.
    Status {
        /// Optional configuration file used to scope status output.
        #[arg(short, long)]
        config: Option<String>,

        /// Optionally specify a service name to check its status.
        #[arg(short, long)]
        service: Option<String>,

        /// Project id to filter status by.
        #[arg(short = 'p', long)]
        project: Option<String>,

        /// Show all services including orphaned state (services not in current config).
        #[arg(long)]
        all: bool,

        /// Emit machine-readable output in the requested format.
        #[arg(
            long,
            value_enum,
            value_name = "FORMAT",
            num_args = 0..=1,
            default_missing_value = "json"
        )]
        format: Option<OutputFormat>,

        /// Disable ANSI colors in output.
        #[arg(long = "no-color")]
        no_color: bool,

        /// Show full command lines in the status table.
        #[arg(long = "full-cmd")]
        full_cmd: bool,

        /// Force immediate runtime collection instead of the configured snapshot mode.
        #[arg(long)]
        live: bool,

        /// Continuously refresh output at the provided interval (e.g., "5", "1s", "2m").
        #[arg(long, value_name = "DURATION")]
        stream: Option<String>,
    },

    /// Inspect a single service or cron unit in detail.
    Inspect {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// Name of the service to inspect.
        #[arg(short, long)]
        service: String,

        /// Project id containing the inspected service.
        #[arg(short = 'p', long)]
        project: Option<String>,

        /// Emit machine-readable output in the requested format.
        #[arg(
            long,
            value_enum,
            value_name = "FORMAT",
            num_args = 0..=1,
            default_missing_value = "json"
        )]
        format: Option<OutputFormat>,

        /// Disable ANSI colors in output.
        #[arg(long = "no-color")]
        no_color: bool,

        /// Force immediate runtime collection instead of the configured snapshot mode.
        #[arg(long)]
        live: bool,

        /// Continuously refresh output and use a rolling metrics window (e.g., "5", "1s", "2m").
        #[arg(long, value_name = "DURATION")]
        stream: Option<String>,
    },

    /// Tail stored service output logs.
    Logs {
        /// Path to the configuration file (defaults to `systemg.yaml`).
        #[arg(short, long, default_value = "systemg.yaml")]
        config: String,

        /// Purge log files instead of displaying them.
        #[arg(long)]
        purge: bool,

        /// Prune rotated log backups instead of displaying logs.
        #[arg(long)]
        prune: bool,

        /// When pruning, cap total rotated-backup size (e.g. "500MB", "2g").
        #[arg(long, value_name = "SIZE")]
        max_size: Option<String>,

        /// When pruning, remove rotated backups older than this (e.g. "7d", "12h").
        #[arg(long, value_name = "AGE")]
        max_age: Option<String>,

        /// The name of the service whose logs should be displayed (optional).
        #[arg(short, long)]
        service: Option<String>,

        /// Project id to filter logs by.
        #[arg(short = 'p', long)]
        project: Option<String>,

        /// Number of lines to show (default: 50).
        #[arg(short, long, default_value = "50")]
        lines: usize,

        /// Kind of logs to show: stdout, stderr, or supervisor. Defaults to stdout+stderr.
        #[arg(short = 'k', long)]
        kind: Option<LogKind>,

        /// Continuously refresh output at the provided interval (e.g., "5", "1s", "2m").
        #[arg(long, value_name = "DURATION")]
        stream: Option<String>,
    },

    /// Purge all systemg state and runtime files.
    Purge,

    /// DEPRECATED: Spawn a dynamic child process from a parent service.
    #[command(hide = true)]
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
    fn status_accepts_live() {
        let cli = Cli::try_parse_from(["sysg", "status", "--live"]).unwrap();
        match cli.command {
            Commands::Status { live, .. } => assert!(live),
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn status_accepts_project_filter() {
        let cli = Cli::try_parse_from(["sysg", "status", "-p", "arbitration"]).unwrap();
        match cli.command {
            Commands::Status { project, .. } => {
                assert_eq!(project.as_deref(), Some("arbitration"));
            }
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn status_config_is_optional() {
        let cli = Cli::try_parse_from(["sysg", "status"]).unwrap();
        match cli.command {
            Commands::Status { config, .. } => assert!(config.is_none()),
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn status_accepts_explicit_config_scope() {
        let cli = Cli::try_parse_from(["sysg", "status", "-c", "foo.yaml"]).unwrap();
        match cli.command {
            Commands::Status { config, .. } => {
                assert_eq!(config.as_deref(), Some("foo.yaml"));
            }
            _ => panic!("expected status command"),
        }
    }

    #[test]
    fn stop_accepts_supervisor_shutdown_flag() {
        let cli = Cli::try_parse_from(["sysg", "stop", "--supervisor"]).unwrap();
        match cli.command {
            Commands::Stop { supervisor, .. } => assert!(supervisor),
            _ => panic!("expected stop command"),
        }
    }

    #[test]
    fn stop_accepts_project_filter_with_log_level() {
        let cli = Cli::try_parse_from([
            "sysg",
            "stop",
            "-p",
            "gamecast-dev",
            "--log-level",
            "DEBUG",
        ])
        .unwrap();
        assert_eq!(
            cli.log_level.as_ref().map(LogLevelArg::as_str),
            Some("debug")
        );
        match cli.command {
            Commands::Stop {
                project, service, ..
            } => {
                assert_eq!(project.as_deref(), Some("gamecast-dev"));
                assert!(service.is_none());
            }
            _ => panic!("expected stop command"),
        }
    }

    #[test]
    fn inspect_accepts_stream() {
        let cli = Cli::try_parse_from([
            "sysg",
            "inspect",
            "--service",
            "demo",
            "--stream",
            "2m",
        ])
        .unwrap();
        match cli.command {
            Commands::Inspect { stream, .. } => {
                assert_eq!(stream.as_deref(), Some("2m"))
            }
            _ => panic!("expected inspect command"),
        }
    }

    #[test]
    fn inspect_accepts_live() {
        let cli = Cli::try_parse_from(["sysg", "inspect", "--service", "demo", "--live"])
            .unwrap();
        match cli.command {
            Commands::Inspect { live, .. } => assert!(live),
            _ => panic!("expected inspect command"),
        }
    }

    #[test]
    fn logs_accepts_project_filter() {
        let cli =
            Cli::try_parse_from(["sysg", "logs", "--project", "arbitration"]).unwrap();
        match cli.command {
            Commands::Logs { project, .. } => {
                assert_eq!(project.as_deref(), Some("arbitration"));
            }
            _ => panic!("expected logs command"),
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
    fn logs_accepts_purge_for_service() {
        let cli = Cli::try_parse_from(["sysg", "logs", "--service", "demo", "--purge"])
            .unwrap();
        match cli.command {
            Commands::Logs { purge, service, .. } => {
                assert!(purge);
                assert_eq!(service.as_deref(), Some("demo"));
            }
            _ => panic!("expected logs command"),
        }
    }

    #[test]
    fn logs_accepts_purge_without_service() {
        let cli = Cli::try_parse_from(["sysg", "logs", "--purge"]).unwrap();
        match cli.command {
            Commands::Logs { purge, service, .. } => {
                assert!(purge);
                assert_eq!(service, None);
            }
            _ => panic!("expected logs command"),
        }
    }

    #[test]
    fn status_rejects_watch() {
        assert!(Cli::try_parse_from(["sysg", "status", "--watch", "5"]).is_err());
    }

    #[test]
    fn start_accepts_trailing_command() {
        let cli =
            Cli::try_parse_from(["sysg", "start", "--daemonize", "sleep", "5"]).unwrap();
        match cli.command {
            Commands::Start {
                daemonize, command, ..
            } => {
                assert!(daemonize);
                assert_eq!(command, vec!["sleep".to_string(), "5".to_string()]);
            }
            _ => panic!("expected start command"),
        }
    }

    #[test]
    fn start_accepts_child_mode_flags() {
        let cli = Cli::try_parse_from([
            "sysg",
            "start",
            "--child",
            "--parent-pid",
            "4242",
            "--name",
            "worker-1",
            "--ttl",
            "30",
            "python",
            "worker.py",
        ])
        .unwrap();
        match cli.command {
            Commands::Start {
                child,
                parent_pid,
                name,
                ttl,
                command,
                ..
            } => {
                assert!(child);
                assert_eq!(parent_pid, Some(4242));
                assert_eq!(name.as_deref(), Some("worker-1"));
                assert_eq!(ttl, Some(30));
                assert_eq!(command, vec!["python".to_string(), "worker.py".to_string()]);
            }
            _ => panic!("expected start command"),
        }
    }

    #[test]
    fn inspect_rejects_window() {
        assert!(
            Cli::try_parse_from([
                "sysg",
                "inspect",
                "--service",
                "demo",
                "--window",
                "5s",
            ])
            .is_err()
        );
    }
}
