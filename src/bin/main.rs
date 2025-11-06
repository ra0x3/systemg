use libc::{SIGKILL, getpgrp, killpg};
use nix::{sys::signal, unistd::Pid};
use std::{
    error::Error,
    os::unix::io::IntoRawFd,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use systemg::{
    cli::{Cli, Commands, parse_args},
    config::load_config,
    daemon::{Daemon, PidFile},
    ipc::{self, ControlCommand, ControlError, ControlResponse},
    logs::LogManager,
    status::StatusManager,
    supervisor::Supervisor,
};

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args();
    init_logging(&args);

    match args.command {
        Commands::Start {
            config, daemonize, ..
        } => {
            if daemonize {
                if supervisor_running() {
                    warn!("systemg supervisor already running; aborting duplicate start");
                    return Ok(());
                }

                let config_path = resolve_config_path(&config)?;
                info!("Starting systemg supervisor with config {:?}", config_path);
                start_supervisor_daemon(config_path)?;
            } else {
                register_signal_handler()?;
                start_foreground(&config)?;
            }
        }
        Commands::Stop { service, config } => {
            let service_name = service.clone();
            if supervisor_running() {
                let command = if let Some(name) = service_name.clone() {
                    ControlCommand::Stop {
                        service: Some(name),
                    }
                } else {
                    ControlCommand::Shutdown
                };
                send_control_command(command)?;
            } else {
                let daemon = build_daemon(&config)?;
                if let Some(service) = service_name {
                    daemon.stop_service(&service)?;
                } else {
                    daemon.stop_services()?;
                }
            }
        }
        Commands::Restart {
            config,
            service,
            daemonize,
        } => {
            if supervisor_running() {
                let config_override = if config.is_empty() {
                    None
                } else {
                    Some(resolve_config_path(&config)?.display().to_string())
                };

                let command = ControlCommand::Restart {
                    config: config_override,
                    service,
                };
                send_control_command(command)?;
            } else if daemonize {
                let config_path = resolve_config_path(&config)?;
                start_supervisor_daemon(config_path)?;
            } else {
                let daemon = build_daemon(&config)?;
                daemon.restart_services()?;
            }
        }
        Commands::Status { service } => {
            let pid = Arc::new(Mutex::new(PidFile::load().unwrap_or_default()));
            let manager = StatusManager::new(pid);
            match service {
                Some(service) => manager.show_status(&service),
                None => manager.show_statuses(),
            }
        }
        Commands::Logs { service, lines } => {
            let pid = Arc::new(Mutex::new(PidFile::load().unwrap_or_default()));
            let manager = LogManager::new(pid.clone());
            match service {
                Some(service) => {
                    info!("Fetching logs for service: {service}");
                    if let Some(process_pid) = pid.lock().unwrap().pid_for(&service) {
                        manager.show_log(&service, process_pid, lines)?;
                    } else {
                        warn!("Service '{service}' is not currently running");
                    }
                }
                None => {
                    info!("Fetching logs for all services");
                    manager.show_logs(lines)?;
                }
            }
        }
    }

    Ok(())
}

fn init_logging(args: &Cli) {
    let filter = if let Some(level) = args.log_level {
        EnvFilter::new(level.as_str())
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };

    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

fn start_foreground(config_path: &str) -> Result<(), Box<dyn Error>> {
    let daemon = build_daemon(config_path)?;
    daemon.start_services_blocking()?;
    Ok(())
}

fn start_supervisor_daemon(config_path: PathBuf) -> Result<(), Box<dyn Error>> {
    daemonize_systemg()?;

    let mut supervisor = Supervisor::new(config_path, false)?;
    if let Err(err) = supervisor.run() {
        error!("Supervisor exited with error: {err}");
    }

    Ok(())
}

fn build_daemon(config_path: &str) -> Result<Daemon, Box<dyn Error>> {
    let config = load_config(Some(config_path))?;
    let daemon = Daemon::from_config(config, false)?;
    Ok(daemon)
}

fn resolve_config_path(path: &str) -> Result<PathBuf, Box<dyn Error>> {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return Ok(candidate);
    }

    let resolved = std::env::current_dir()?.join(&candidate);
    Ok(resolved.canonicalize().unwrap_or(resolved))
}

fn supervisor_running() -> bool {
    match ipc::read_supervisor_pid() {
        Ok(Some(pid)) => {
            let target = Pid::from_raw(pid);
            match signal::kill(target, None) {
                Ok(_) => true,
                Err(err) => {
                    if err == nix::Error::from(nix::errno::Errno::ESRCH) {
                        let _ = ipc::cleanup_runtime();
                        false
                    } else {
                        warn!("Failed to query supervisor pid {pid}: {err}");
                        false
                    }
                }
            }
        }
        Ok(None) | Err(_) => false,
    }
}

fn send_control_command(command: ControlCommand) -> Result<(), Box<dyn Error>> {
    match ipc::send_command(&command) {
        Ok(ControlResponse::Message(message)) => {
            println!("{message}");
            Ok(())
        }
        Ok(ControlResponse::Ok) => Ok(()),
        Ok(ControlResponse::Error(message)) => Err(ControlError::Server(message).into()),
        Err(ControlError::NotAvailable) => {
            warn!("No running systemg supervisor found; skipping command");
            let _ = ipc::cleanup_runtime();
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

fn daemonize_systemg() -> std::io::Result<()> {
    if unsafe { libc::fork() } > 0 {
        std::process::exit(0);
    }

    unsafe {
        libc::setsid();
    }

    if unsafe { libc::fork() } > 0 {
        std::process::exit(0);
    }

    unsafe {
        libc::setpgid(0, 0);
    }

    std::env::set_current_dir("/")?;
    let devnull = std::fs::File::open("/dev/null")?;
    let fd = devnull.into_raw_fd();
    unsafe {
        let _ = libc::dup2(fd, libc::STDIN_FILENO);
        let _ = libc::dup2(fd, libc::STDOUT_FILENO);
        let _ = libc::dup2(fd, libc::STDERR_FILENO);
        libc::close(fd);
    }

    Ok(())
}

fn register_signal_handler() -> Result<(), Box<dyn Error>> {
    ctrlc::set_handler(move || {
        println!("systemg is shutting down... killing child process group");

        unsafe {
            let pgid = getpgrp();
            killpg(pgid, SIGKILL);
        }

        std::process::exit(0);
    })?;

    Ok(())
}
