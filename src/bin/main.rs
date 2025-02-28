use std::process;
use systemg::cli::{Commands, parse_args};
use systemg::config::load_config;
use systemg::daemon::Daemon;
use tracing::{error, info};

/// Entry point for the Rust Process Manager.
///
/// This function:
/// - Parses CLI arguments.
/// - Loads the configuration file.
/// - Starts, stops, restarts, or checks service status based on user input.
///
/// # Errors
/// - If the configuration file fails to load, it logs an error and exits.
/// - If any daemon operation (start, stop, restart) fails, it logs the error and exits.
fn main() {
    tracing_subscriber::fmt::init();
    let args = parse_args();

    match args.command {
        Commands::Start { config } => {
            info!("Starting services using config: {}", config);
            match load_config(&config) {
                Ok(parsed_config) => {
                    let mut daemon = Daemon::default();
                    if let Err(e) = daemon.start_services(&parsed_config) {
                        error!("Error starting services: {}", e);
                        process::exit(1);
                    }
                }
                Err(e) => {
                    error!("Failed to load config: {}", e);
                    process::exit(1);
                }
            }
        }

        Commands::Stop => {
            info!("Stopping all services...");
            let mut daemon = Daemon::default();
            if let Err(e) = daemon.stop_services() {
                error!("Error stopping services: {}", e);
                process::exit(1);
            }
        }

        Commands::Restart { config } => {
            info!("Restarting services using config: {}", config);
            match load_config(&config) {
                Ok(parsed_config) => {
                    let mut daemon = Daemon::default();
                    if let Err(e) = daemon.restart_services(&parsed_config) {
                        error!("Error restarting services: {}", e);
                        process::exit(1);
                    }
                }
                Err(e) => {
                    error!("Failed to load config: {}", e);
                    process::exit(1);
                }
            }
        }

        Commands::Status => {
            info!("Checking service status...");
            let daemon = Daemon::default();
            daemon.status();
        }
    }
}
