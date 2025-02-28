use clap::{Parser, Subcommand};

/// Command-line interface for the Rust Process Manager.
///
/// This module defines the structure of the CLI using `clap` and provides
/// commands to start, stop, restart, and check the status of managed services.
#[derive(Parser)]
#[command(name = "rust_process_manager")]
#[command(about = "A lightweight process manager for system services", long_about = None)]
pub struct Cli {
    /// The command to execute.
    #[command(subcommand)]
    pub command: Commands,
}

/// Available commands for the process manager.
#[derive(Subcommand)]
pub enum Commands {
    /// Start the process manager with the given configuration.
    Start {
        /// Path to the configuration file (defaults to `config.yaml`).
        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },

    /// Stop the currently running process manager.
    Stop,

    /// Restart the process manager, optionally specifying a new configuration file.
    Restart {
        /// Path to the configuration file (defaults to `config.yaml`).
        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },

    /// Show the status of currently running services.
    Status,
}

/// Parses command-line arguments and returns a `Cli` struct.
///
/// # Returns
///
/// * `Cli` - Parsed CLI arguments containing the command to execute.
///
/// # Example
///
/// ```no_run
/// use systemg::cli::{parse_args, Commands};
///
/// let args = parse_args();
/// match args.command {
///     Commands::Start { config } => println!("Starting with config: {}", config),
///     Commands::Stop => println!("Stopping process manager"),
///     Commands::Restart { config } => println!("Restarting with config: {}", config),
///     Commands::Status => println!("Showing service status"),
/// }
/// ```
pub fn parse_args() -> Cli {
    Cli::parse()
}
