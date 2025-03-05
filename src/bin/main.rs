use std::process;
use systemg::{
    cli::{Commands, parse_args},
    config::load_config,
    daemon::Daemon,
    logs::show_logs,
    status::show_status,
};
use tracing::{error, info};

/// Entry point for the Rust Process Manager.
///
/// This function:
/// - Parses CLI arguments.
/// - Loads the configuration file.
/// - Starts, stops, restarts, checks service status, or retrieves logs.
///
/// # Errors
/// - If the configuration file fails to load, it logs an error and exits.
/// - If any daemon operation (start, stop, restart) fails, it logs the error and exits.
fn main() {
    tracing_subscriber::fmt::init();
    let args = parse_args();

    match args.command {
        Commands::Start { config } => {
            info!("Loading configuration from: {config:?}");
            match load_config(&config) {
                Ok(parsed_config) => {
                    let daemon = Daemon::new(parsed_config);

                    if let Err(e) = daemon.start_services() {
                        error!("Error starting services: {e}");
                        process::exit(1);
                    }
                }
                Err(e) => {
                    error!("Failed to load config: {e}");
                    process::exit(1);
                }
            }
        }

        Commands::Stop => {
            info!("Stopping all services...");
            match load_config("systemg.yaml") {
                Ok(parsed_config) => {
                    let mut daemon = Daemon::new(parsed_config);

                    if let Err(e) = daemon.stop_services() {
                        error!("Error stopping services: {e}");
                        process::exit(1);
                    }
                }
                Err(e) => {
                    error!("Failed to load config for stopping services: {e}");
                    process::exit(1);
                }
            }
        }

        Commands::Restart { config } => {
            info!("Restarting services using config: {config:?}");
            match load_config(&config) {
                Ok(parsed_config) => {
                    let mut daemon = Daemon::new(parsed_config);

                    if let Err(e) = daemon.restart_services() {
                        error!("Error restarting services: {e}");
                        process::exit(1);
                    }
                }
                Err(e) => {
                    error!("Failed to load config: {e}");
                    process::exit(1);
                }
            }
        }

        Commands::Status { service } => {
            info!("Checking service status...");
            show_status(service.as_deref());
        }

        Commands::Logs { service, lines } => {
            info!("Fetching logs for service: {service}");
            if let Err(e) = show_logs(&service, lines) {
                error!("Error reading logs: {e}");
                process::exit(1);
            }
        }
    }
}
