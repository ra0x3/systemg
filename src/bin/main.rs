use libc::{SIGKILL, getpgrp, killpg};
use tracing::{error, info};

use systemg::{
    cli::{Commands, parse_args},
    config::load_config,
    daemon::Daemon,
    logs::show_logs,
    status::show_status,
};

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
fn main() -> Result<(), Box<dyn std::error::Error>> {
    register_signal_handler();
    tracing_subscriber::fmt::init();
    let args = parse_args();

    match args.command {
        Commands::Start { config, daemonize } => {
            info!("Loading configuration from: {config:?}");
            match load_config(&config) {
                Ok(parsed_config) => {
                    if daemonize {
                        daemonize_systemg()?;
                    }
                    let daemon = Daemon::new(parsed_config);

                    if let Err(e) = daemon.start_services() {
                        error!("Error starting services: {e}");
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    error!("Failed to load config: {e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Stop { service } => {
            info!("Stopping all services...");
            match load_config("systemg.yaml") {
                Ok(parsed_config) => {
                    let mut daemon = Daemon::new(parsed_config);

                    if service.is_none() {
                        if let Err(e) = daemon.stop_services() {
                            error!("Error stopping services: {e}");
                            std::process::exit(1);
                        }
                    } else if let Err(e) = daemon.stop_service(&service.unwrap()) {
                        error!("Error stopping service: {e}");
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    error!("Failed to load config for stopping services: {e}");
                    std::process::exit(1);
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
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    error!("Failed to load config: {e}");
                    std::process::exit(1);
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
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

/// Daemonizes the systemg process.
fn daemonize_systemg() -> std::io::Result<()> {
    if unsafe { libc::fork() } > 0 {
        std::process::exit(0); // Parent exits, child continues
    }

    unsafe {
        libc::setsid(); // Create a new session
    }

    if unsafe { libc::fork() } > 0 {
        std::process::exit(0); // First child exits, second child is fully detached
    }

    unsafe {
        libc::setpgid(0, 0); // Ensure systemg retains control over its process group
    }

    Ok(())
}

/// Registers a signal handler to terminate child processes on `SIGTERM` or `SIGINT`.
fn register_signal_handler() {
    ctrlc::set_handler(move || {
        println!("systemg is shutting down... killing all child processes");

        unsafe {
            let pgid = getpgrp();
            killpg(pgid, SIGKILL); // Kill the entire process group
        }

        std::process::exit(0);
    })
    .expect("Failed to set signal handler");
}
