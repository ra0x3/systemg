use libc::{SIGKILL, getpgrp, killpg};
use nix::{sys::signal, unistd::Pid};
use std::{
    error::Error,
    fs,
    os::unix::io::IntoRawFd,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use systemg::{
    cli::{Cli, Commands, parse_args},
    config::load_config,
    cron::CronStateFile,
    daemon::{Daemon, PidFile, ServiceStateFile},
    ipc::{self, ControlCommand, ControlError, ControlResponse},
    logs::{LogManager, resolve_log_path},
    status::StatusManager,
    supervisor::Supervisor,
};

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args();
    init_logging(&args);

    match args.command {
        Commands::Start {
            config,
            daemonize,
            service,
        } => {
            if daemonize {
                if supervisor_running() {
                    warn!("systemg supervisor already running; aborting duplicate start");
                    return Ok(());
                }

                let config_path = resolve_config_path(&config)?;
                info!("Starting systemg supervisor with config {:?}", config_path);
                start_supervisor_daemon(config_path, service)?;
            } else {
                register_signal_handler()?;
                start_foreground(&config, service)?;
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
                match build_daemon(&config) {
                    Ok(daemon) => {
                        if let Some(service) = service_name.as_deref() {
                            daemon.stop_service(service)?;
                        } else {
                            daemon.stop_services()?;
                        }
                    }
                    Err(err) => {
                        warn!(
                            "No supervisor detected and unable to load config '{}': {}",
                            config, err
                        );
                        if let Ok(Some(hint)) = ipc::read_config_hint() {
                            let hint_str = hint.to_string_lossy();
                            match build_daemon(hint_str.as_ref()) {
                                Ok(daemon) => {
                                    if let Some(service) = service_name.as_deref() {
                                        daemon.stop_service(service)?;
                                    } else {
                                        daemon.stop_services()?;
                                    }
                                    info!(
                                        "stop fallback executed using config hint {:?}",
                                        hint
                                    );
                                }
                                Err(hint_err) => {
                                    warn!(
                                        "Fallback using config hint {:?} failed: {}",
                                        hint, hint_err
                                    );
                                }
                            }
                        }
                    }
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
                start_supervisor_daemon(config_path, None)?;
            } else {
                let daemon = build_daemon(&config)?;
                daemon.restart_services()?;
            }
        }
        Commands::Status {
            config,
            service,
            all,
        } => {
            let pid = Arc::new(Mutex::new(PidFile::load().unwrap_or_default()));
            let state =
                Arc::new(Mutex::new(ServiceStateFile::load().unwrap_or_default()));
            let manager = StatusManager::new(pid, state);
            match service {
                Some(service) => manager.show_status(&service, Some(&config)),
                None => {
                    if all {
                        manager.show_statuses_all()
                    } else {
                        let config_data = load_config(Some(&config))?;
                        manager.show_statuses_filtered(&config_data)
                    }
                }
            }
        }
        Commands::Logs {
            service,
            lines,
            kind,
        } => {
            let pid = Arc::new(Mutex::new(PidFile::load().unwrap_or_default()));
            let manager = LogManager::new(pid.clone());
            match service {
                Some(service) => {
                    info!("Fetching logs for service: {service}");
                    let process_pid = pid.lock().unwrap().pid_for(&service);

                    if let Some(process_pid) = process_pid {
                        manager.show_log(
                            &service,
                            process_pid,
                            lines,
                            kind.as_deref(),
                        )?;
                    } else {
                        let cron_state = CronStateFile::load().unwrap_or_default();
                        let stdout_exists = resolve_log_path(&service, "stdout").exists();
                        let stderr_exists = resolve_log_path(&service, "stderr").exists();

                        if cron_state.jobs().contains_key(&service)
                            || stdout_exists
                            || stderr_exists
                        {
                            manager.show_inactive_log(
                                &service,
                                lines,
                                kind.as_deref(),
                            )?;
                        } else {
                            warn!("Service '{service}' is not currently running");
                        }
                    }
                }
                None => {
                    info!("Fetching logs for all services");
                    manager.show_logs(lines, kind.as_deref())?;
                }
            }
        }
        Commands::Purge => {
            purge_all_state()?;
            println!("All systemg state has been purged");
        }
    }

    Ok(())
}

fn supervisor_log_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    PathBuf::from(format!("{}/.local/share/systemg/supervisor.log", home))
}

fn purge_all_state() -> Result<(), Box<dyn Error>> {
    let home = std::env::var("HOME").expect("HOME not set");
    let runtime_dir = PathBuf::from(format!("{}/.local/share/systemg", home));

    if runtime_dir.exists() {
        info!("Removing systemg runtime directory: {:?}", runtime_dir);
        fs::remove_dir_all(&runtime_dir)?;
        info!("Successfully purged all systemg state");
    } else {
        info!("No systemg runtime directory found at {:?}", runtime_dir);
    }

    Ok(())
}

fn init_logging(args: &Cli) {
    let filter = if let Some(level) = args.log_level {
        EnvFilter::new(level.as_str())
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };

    // Ensure the log directory exists
    let log_path = supervisor_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    // Open log file in append mode
    let file = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(file) => file,
        Err(e) => {
            eprintln!("Failed to open supervisor log file {:?}: {}", log_path, e);
            // Fall back to stdout if we can't open the log file
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
            return;
        }
    };

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(move || file.try_clone().unwrap())
        .with_ansi(false)
        .try_init();
}

fn start_foreground(
    config_path: &str,
    service: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let resolved_path = resolve_config_path(config_path)?;
    let mut supervisor = Supervisor::new(resolved_path, false, service)?;
    supervisor.run()?;
    Ok(())
}

fn start_supervisor_daemon(
    config_path: PathBuf,
    service: Option<String>,
) -> Result<(), Box<dyn Error>> {
    daemonize_systemg()?;

    let mut supervisor = Supervisor::new(config_path, false, service)?;
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
    // Check PID first (more reliable than socket existence)
    match ipc::read_supervisor_pid() {
        Ok(Some(pid)) => {
            let target = Pid::from_raw(pid);
            match signal::kill(target, None) {
                Ok(_) => {
                    // Process is alive
                    true
                }
                Err(err) => {
                    if err == nix::Error::from(nix::errno::Errno::ESRCH) {
                        // Process is dead - clean up stale artifacts
                        let _ = ipc::cleanup_runtime();
                        false
                    } else {
                        warn!("Failed to query supervisor pid {pid}: {err}");
                        false
                    }
                }
            }
        }
        Ok(None) | Err(_) => {
            // No PID file - check if stale socket exists and clean it up
            if let Ok(path) = ipc::socket_path()
                && path.exists()
            {
                warn!("Found stale socket without PID file, cleaning up");
                let _ = ipc::cleanup_runtime();
            }
            false
        }
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
        println!("systemg is shutting down... terminating child services");

        let mut service_pids: Vec<(String, libc::pid_t)> = Vec::new();
        if let Ok(pid_file) = PidFile::load() {
            for (service, pid) in pid_file.services() {
                service_pids.push((service.clone(), *pid as libc::pid_t));
            }
        }

        for (service, pgid) in &service_pids {
            unsafe {
                if libc::killpg(*pgid, libc::SIGTERM) == -1 {
                    let err = std::io::Error::last_os_error();
                    match err.raw_os_error() {
                        Some(code) if code == libc::ESRCH => {}
                        Some(code) if code == libc::EPERM => {
                            let _ = libc::kill(*pgid, libc::SIGTERM);
                        }
                        _ => eprintln!(
                            "systemg: failed to send SIGTERM to '{service}' (pgid {pgid}): {err}"
                        ),
                    }
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(150));

        for (service, pgid) in &service_pids {
            unsafe {
                if libc::killpg(*pgid, libc::SIGKILL) == -1 {
                    let err = std::io::Error::last_os_error();
                    if !matches!(err.raw_os_error(), Some(code) if code == libc::ESRCH) {
                        eprintln!(
                            "systemg: failed to send SIGKILL to '{service}' (pgid {pgid}): {err}"
                        );
                    }
                }
            }
        }

        unsafe {
            let pgid = getpgrp();
            killpg(pgid, SIGKILL);
        }

        std::process::exit(0);
    })?;

    Ok(())
}
