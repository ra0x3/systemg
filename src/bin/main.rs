use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fs, io,
    io::Write,
    os::unix::io::IntoRawFd,
    path::{Path, PathBuf},
    process,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime},
};

use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal,
};
use libc::{SIGKILL, SIGTERM, getppid};
use nix::{
    sys::signal,
    unistd::{Pid, Uid},
};
use sha2::{Digest, Sha256};
use sysinfo::{Pid as SysPid, ProcessRefreshKind, ProcessesToUpdate, System, Users};
use systemg::{
    charting::{self, ChartConfig, parse_stream_duration},
    cli::{Cli, Commands, parse_args},
    config::load_config,
    cron::{CronExecutionStatus, CronStateFile},
    daemon::{Daemon, PidFile, ServiceLifecycleStatus},
    ipc::{self, ControlCommand, ControlError, ControlResponse, InspectPayload},
    logs::{LogManager, resolve_log_path},
    metrics::MetricSample,
    runtime::{self, RuntimeMode},
    spawn::{SpawnedChild, SpawnedChildKind, SpawnedExit},
    status::{
        CronUnitStatus, ExitMetadata, OverallHealth, ProcessState, SpawnedProcessNode,
        StatusSnapshot, UnitHealth, UnitKind, UnitMetricsSummary, UnitStatus, UptimeInfo,
        collect_disk_snapshot, compute_overall_health, format_elapsed,
    },
    supervisor::Supervisor,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const UNIT_CONFIG_MAX_FILES: usize = 200;
const UNIT_CONFIG_MAX_AGE_DAYS: u64 = 30;
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
const INSPECT_CRON_HISTORY_LIMIT: usize = 10;

/// Runs the `sysg` command-line entrypoint.
fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args();
    let euid = Uid::effective();
    let drop_privileges_effective =
        args.drop_privileges && drop_privileges_applies_to_command(&args.command);

    let runtime_mode = if args.sys {
        if !euid.is_root() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "--sys requires root privileges",
            )
            .into());
        }
        RuntimeMode::System
    } else {
        RuntimeMode::User
    };

    runtime::init(runtime_mode);
    runtime::set_drop_privileges(drop_privileges_effective);
    runtime::capture_socket_activation();

    let use_file_logging = matches!(
        &args.command,
        Commands::Start {
            daemonize: true,
            ..
        } | Commands::Restart {
            daemonize: true,
            ..
        }
    );
    init_logging(&args, use_file_logging);

    if args.drop_privileges && !euid.is_root() {
        warn!("--drop-privileges has no effect when not running as root");
    } else if args.drop_privileges && !drop_privileges_effective {
        warn!(
            "--drop-privileges only applies when spawning child services during start/restart; this command will ignore it"
        );
    }

    if euid.is_root() && runtime_mode == RuntimeMode::User {
        warn!("Running as root without --sys; state will be stored in userspace paths");
        if system_mode_state_detected() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Detected system-mode state at /var/lib/systemg while running as root without --sys. Re-run with --sys to avoid targeting the wrong runtime.",
            )
            .into());
        }
    }

    match args.command {
        Commands::Start {
            config,
            daemonize,
            service,
            name,
            ttl,
            parent_pid,
            child,
            stderr,
            command,
        } => {
            if let Some(child_start) = resolve_child_start(
                child,
                parent_pid,
                ttl,
                name.clone(),
                &command,
                args.log_level.map(|level| level.as_str().to_string()),
            )? {
                run_child_start(child_start)?;
                return Ok(());
            }

            let start_target =
                resolve_start_target(&config, service, name.as_deref(), command)?;

            if daemonize {
                if supervisor_running() {
                    if args.drop_privileges {
                        warn!(
                            "--drop-privileges is managed by the running supervisor and has no effect for this start request"
                        );
                    }

                    if start_target.ad_hoc {
                        info!(
                            "Staged unit config at {:?}. Running supervisor was left unchanged.",
                            start_target.config_path
                        );
                        println!(
                            "Unit staged at {}. Run `sysg restart --daemonize --config {}` to apply it.",
                            start_target.config_path.display(),
                            start_target.config_path.display()
                        );
                    } else if let Some(service_name) = start_target.service {
                        let command = ControlCommand::Start {
                            service: Some(service_name.clone()),
                        };
                        send_control_command(command)?;
                        info!(
                            "Service '{service_name}' start command sent to supervisor"
                        );
                    } else {
                        warn!(
                            "systemg supervisor already running; aborting duplicate start"
                        );
                    }
                    return Ok(());
                }

                info!(
                    "Starting systemg supervisor with config {:?}",
                    start_target.config_path
                );
                start_supervisor_daemon(
                    start_target.config_path,
                    start_target.service,
                    stderr,
                )?;
            } else {
                register_signal_handler()?;
                start_foreground(start_target.config_path, start_target.service, stderr)?;
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
                if args.drop_privileges {
                    warn!(
                        "--drop-privileges is managed by the running supervisor and has no effect for this restart request"
                    );
                }
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
                start_supervisor_daemon(config_path, None, false)?;
            } else {
                warn!(
                    "No running supervisor detected; executing restart in local one-shot mode. \
Use --daemonize in deployment scripts to ensure daemonized supervision is restored if detection fails."
                );
                let daemon = build_daemon(&config)?;
                daemon.restart_services()?;
            }
        }
        Commands::Status {
            config,
            service,
            all,
            json,
            no_color,
            full_cmd,
            stream,
        } => {
            let mut effective_config = config.clone();
            if load_config(Some(&config)).is_err()
                && let Ok(Some(hint)) = ipc::read_config_hint()
            {
                effective_config = hint.to_string_lossy().to_string();
            }

            let render_opts = StatusRenderOptions {
                json,
                no_color,
                full_cmd,
                include_orphans: all,
                service_filter: service.as_deref(),
            };

            if let Some(stream_interval) = stream {
                let stream_seconds = match parse_stream_duration(&stream_interval) {
                    Ok(seconds) => seconds,
                    Err(err) => {
                        eprintln!(
                            "Invalid stream duration '{}': {}",
                            stream_interval, err
                        );
                        process::exit(1);
                    }
                };
                let sleep_interval = Duration::from_secs(stream_seconds);
                loop {
                    match fetch_status_snapshot(&effective_config) {
                        Ok(snapshot) => {
                            if let Err(e) = render_status(
                                &snapshot,
                                &render_opts,
                                true,
                                &effective_config,
                            ) {
                                eprintln!("Error rendering status: {}", e);
                                thread::sleep(sleep_interval);
                                continue;
                            }
                        }
                        Err(_) => {
                            print!("\x1B[2J\x1B[H");
                            println!(
                                "{}Warn: Supervisor has been shut down{}",
                                YELLOW, RESET
                            );
                            println!("\nWaiting for supervisor to restart...");
                            println!("Press Ctrl+C to exit stream mode.");
                        }
                    }
                    thread::sleep(sleep_interval);
                }
            } else {
                let snapshot = fetch_status_snapshot(&effective_config)?;

                let health =
                    render_status(&snapshot, &render_opts, false, &effective_config)?;

                let exit_code = match health {
                    OverallHealth::Healthy => 0,
                    OverallHealth::Degraded => 1,
                    OverallHealth::Failing => 2,
                };
                process::exit(exit_code);
            }
        }
        Commands::Inspect {
            config,
            service,
            json,
            no_color,
            stream,
        } => {
            let mut effective_config = config.clone();
            if load_config(Some(&config)).is_err()
                && let Ok(Some(hint)) = ipc::read_config_hint()
            {
                effective_config = hint.to_string_lossy().to_string();
            }

            let stream_seconds = match stream.as_deref() {
                Some(value) => match parse_stream_duration(value) {
                    Ok(seconds) => seconds,
                    Err(err) => {
                        eprintln!("Invalid stream duration '{}': {}", value, err);
                        process::exit(1);
                    }
                },
                None => 5,
            };

            let samples_limit = if stream_seconds < 3600 {
                stream_seconds as usize
            } else {
                720
            };

            let render_opts = InspectRenderOptions {
                json,
                no_color,
                window_seconds: stream_seconds,
                window_desc: format!("last {}s", stream_seconds),
                samples_limit,
            };

            if stream.is_some() {
                let sleep_interval = Duration::from_secs(stream_seconds);
                loop {
                    let payload =
                        fetch_inspect(&effective_config, &service, samples_limit)?;
                    if payload.unit.is_none() {
                        eprintln!("Service '{service}' not found.");
                        process::exit(2);
                    }
                    print!("\x1B[2J\x1B[H");
                    let _ = io::stdout().flush();
                    let _ = render_inspect(&payload, &render_opts)?;
                    thread::sleep(sleep_interval);
                }
            } else {
                let payload = fetch_inspect(&effective_config, &service, samples_limit)?;
                if payload.unit.is_none() {
                    eprintln!("Service '{service}' not found.");
                    process::exit(2);
                }

                let health = render_inspect(&payload, &render_opts)?;
                let exit_code = match health {
                    OverallHealth::Healthy => 0,
                    OverallHealth::Degraded => 1,
                    OverallHealth::Failing => 2,
                };
                process::exit(exit_code);
            }
        }
        Commands::Logs {
            config,
            clear,
            service,
            lines,
            kind,
            stream,
        } => {
            let effective_config = match load_config(Some(&config)) {
                Ok(_) => config.clone(),
                Err(_) => {
                    if let Ok(Some(hint)) = ipc::read_config_hint() {
                        hint.to_string_lossy().to_string()
                    } else {
                        config.clone()
                    }
                }
            };

            let pid = Arc::new(Mutex::new(PidFile::load().unwrap_or_default()));
            let manager = LogManager::new(pid.clone());

            if clear {
                match service.as_deref() {
                    Some(service_name) => {
                        info!("Clearing logs for service: {service_name}");
                        manager.clear_service_logs(service_name)?;
                    }
                    None => {
                        info!("Clearing logs for all services");
                        manager.clear_all_logs()?;
                    }
                }
                return Ok(());
            }

            let stream_logs_via_supervisor =
                |follow: bool| -> Result<(), Box<dyn Error>> {
                    let command = ControlCommand::Logs {
                        service: service.clone(),
                        lines,
                        kind: kind.as_str().to_string(),
                        follow,
                    };
                    ipc::stream_command_output(&command, io::stdout())
                        .map_err(|err| Box::new(err) as Box<dyn Error>)
                };

            let render_logs_once = |snapshot_mode: bool| -> Result<(), Box<dyn Error>> {
                let snapshot = fetch_status_snapshot(&effective_config)?;

                match service.as_ref() {
                    Some(service_name) => {
                        info!("Fetching logs for service: {service_name}");
                        render_service_logs_from_snapshot(
                            &manager,
                            &snapshot,
                            service_name,
                            lines,
                            kind.as_str(),
                            snapshot_mode,
                        )?;
                    }
                    None => {
                        info!("Fetching logs for all services");
                        render_all_logs_from_snapshot(
                            &manager,
                            &snapshot,
                            lines,
                            kind.as_str(),
                            snapshot_mode,
                        )?;
                    }
                }
                Ok(())
            };

            if let Some(stream_interval) = stream {
                let stream_seconds = match parse_stream_duration(&stream_interval) {
                    Ok(seconds) => seconds,
                    Err(err) => {
                        eprintln!(
                            "Invalid stream duration '{}': {}",
                            stream_interval, err
                        );
                        process::exit(1);
                    }
                };
                let sleep_interval = Duration::from_secs(stream_seconds);
                loop {
                    print!("\x1B[2J\x1B[H");
                    let _ = io::stdout().flush();
                    match stream_logs_via_supervisor(false) {
                        Ok(()) => {}
                        Err(err) => match err.downcast_ref::<ControlError>() {
                            Some(ControlError::NotAvailable) => render_logs_once(true)?,
                            _ => return Err(err),
                        },
                    }
                    thread::sleep(sleep_interval);
                }
            } else {
                match stream_logs_via_supervisor(true) {
                    Ok(()) => {}
                    Err(err) => match err.downcast_ref::<ControlError>() {
                        Some(ControlError::NotAvailable) => render_logs_once(false)?,
                        _ => return Err(err),
                    },
                }
            }
        }
        Commands::Purge => {
            purge_all_state()?;
            println!("All systemg state has been purged");
        }
        Commands::Spawn {
            name,
            ttl,
            parent_pid,
            log_level,
            command,
        } => {
            eprintln!(
                "Warning: `sysg spawn` is deprecated. Use `sysg start --parent-pid <pid> --name <name> [--ttl <seconds>] -- <command...>`."
            );
            let child_start = ChildStartRequest {
                parent_pid: parent_pid.unwrap_or_else(|| unsafe { getppid() } as u32),
                name,
                command,
                ttl,
                log_level: log_level.map(|level| level.as_str().to_string()),
            };
            run_child_start(child_start)?;
        }
    }

    Ok(())
}

/// Handles drop privileges applies to command.
fn drop_privileges_applies_to_command(command: &Commands) -> bool {
    matches!(command, Commands::Start { .. } | Commands::Restart { .. })
}

/// Handles system mode state detected.
fn system_mode_state_detected() -> bool {
    let state_dir = PathBuf::from("/var/lib/systemg");
    state_dir.join("sysg.pid").exists() || state_dir.join("control.sock").exists()
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use systemg::{spawn::SpawnedChild, status::SpawnedProcessNode};

    use super::*;

    #[test]
    fn visit_spawn_tree_renders_nested_children() {
        let nodes = vec![SpawnedProcessNode::new(
            SpawnedChild {
                name: "team_lead".into(),
                pid: 200,
                parent_pid: 100,
                command: "team lead".into(),
                started_at: SystemTime::now(),
                ttl: None,
                depth: 1,
                cpu_percent: None,
                rss_bytes: None,
                last_exit: None,
                user: None,
                kind: SpawnedChildKind::Spawned,
            },
            vec![
                SpawnedProcessNode::new(
                    SpawnedChild {
                        name: "core_infra_dev".into(),
                        pid: 300,
                        parent_pid: 200,
                        command: "core".into(),
                        started_at: SystemTime::now(),
                        ttl: None,
                        depth: 2,
                        cpu_percent: None,
                        rss_bytes: None,
                        last_exit: None,
                        user: None,
                        kind: SpawnedChildKind::Spawned,
                    },
                    vec![SpawnedProcessNode::new(
                        SpawnedChild {
                            name: "infra_helper".into(),
                            pid: 400,
                            parent_pid: 300,
                            command: "infra helper".into(),
                            started_at: SystemTime::now(),
                            ttl: None,
                            depth: 3,
                            cpu_percent: None,
                            rss_bytes: None,
                            last_exit: None,
                            user: None,
                            kind: SpawnedChildKind::Peripheral,
                        },
                        Vec::new(),
                    )],
                ),
                SpawnedProcessNode::new(
                    SpawnedChild {
                        name: "ui_dev".into(),
                        pid: 301,
                        parent_pid: 200,
                        command: "ui".into(),
                        started_at: SystemTime::now(),
                        ttl: None,
                        depth: 2,
                        cpu_percent: None,
                        rss_bytes: None,
                        last_exit: None,
                        user: None,
                        kind: SpawnedChildKind::Spawned,
                    },
                    Vec::new(),
                ),
            ],
        )];

        let mut rendered = Vec::new();
        visit_spawn_tree(&nodes, "", &mut |child, prefix, _| {
            rendered.push(format!("{}{}", prefix, child.name));
        });

        assert_eq!(
            rendered,
            vec![
                "└─ team_lead".to_string(),
                "   ├─ core_infra_dev".to_string(),
                "   │  └─ infra_helper".to_string(),
                "   └─ ui_dev".to_string(),
            ],
        );
    }

    #[test]
    fn status_rows_render_service_kind_and_spawn_user() {
        let columns = vec![
            Column {
                title: "UNIT",
                width: 48,
                align: Alignment::Left,
            },
            Column {
                title: "KIND",
                width: 6,
                align: Alignment::Center,
            },
            Column {
                title: "STATE",
                width: 7,
                align: Alignment::Left,
            },
            Column {
                title: "USER",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "PID",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "CPU",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "RSS",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "UPTIME",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "CMD",
                width: 64,
                align: Alignment::Left,
            },
            Column {
                title: "LAST_EXIT",
                width: 10,
                align: Alignment::Left,
            },
            Column {
                title: "HEALTH",
                width: 8,
                align: Alignment::Left,
            },
        ];

        let unit = UnitStatus {
            name: "orchestrator".to_string(),
            hash: "abc123".to_string(),
            kind: UnitKind::Service,
            lifecycle: Some(ServiceLifecycleStatus::Running),
            health: UnitHealth::Healthy,
            process: Some(systemg::status::ProcessRuntime {
                pid: 1234,
                state: ProcessState::Running,
                user: Some("rashad".to_string()),
            }),
            uptime: None,
            last_exit: None,
            cron: None,
            metrics: None,
            command: None,
            runtime_command: None,
            spawned_children: vec![],
        };
        let unit_row = format_unit_row(&unit, &columns, true);
        assert!(unit_row.contains("srvc"));
        assert!(unit_row.contains("rashad"));

        let child = SpawnedChild {
            name: "agent-owner".to_string(),
            pid: 2222,
            parent_pid: 1234,
            command: "python main.py".to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth: 1,
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
            user: Some("rashad".to_string()),
            kind: SpawnedChildKind::Spawned,
        };
        let child_row = format_spawned_child_row(&child, &columns, true, "└─ ");
        assert!(child_row.contains("spwn"));
        assert!(child_row.contains("rashad"));
    }

    #[test]
    fn spawned_rows_darken_by_depth() {
        let columns = vec![
            Column {
                title: "UNIT",
                width: 48,
                align: Alignment::Left,
            },
            Column {
                title: "KIND",
                width: 6,
                align: Alignment::Center,
            },
            Column {
                title: "STATE",
                width: 7,
                align: Alignment::Left,
            },
            Column {
                title: "USER",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "PID",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "CPU",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "RSS",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "UPTIME",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "CMD",
                width: 64,
                align: Alignment::Left,
            },
            Column {
                title: "LAST_EXIT",
                width: 10,
                align: Alignment::Left,
            },
            Column {
                title: "HEALTH",
                width: 8,
                align: Alignment::Left,
            },
        ];

        let child = SpawnedChild {
            name: "/opt/homebrew/bin/claude".to_string(),
            pid: 62751,
            parent_pid: 59769,
            command: "/opt/homebrew/bin/claude --dangerously-skip-permissions"
                .to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth: 4,
            cpu_percent: Some(0.0),
            rss_bytes: Some(123_456_789),
            last_exit: None,
            user: Some("rashad".to_string()),
            kind: SpawnedChildKind::Peripheral,
        };

        let row = format_spawned_child_row(&child, &columns, false, "└─ ");
        assert!(row.contains(&format!("{DARK_GRAY}└─ /opt/homebrew/bin/claude{RESET}")));
        assert!(row.contains(&format!("{DARK_GRAY}rashad{RESET}")));
        assert!(row.contains(&format!("{DARK_GRAY}62751{RESET}")));
        assert!(row.contains(&format!("{DARK_GRAY}0.0%{RESET}")));
        assert!(row.contains(&format!("{DARK_GRAY}117.7MB{RESET}")));
        assert!(row.contains(&format!(
            "{DARK_GRAY}/opt/homebrew/bin/claude --dangerously-skip-permissions{RESET}"
        )));
        assert!(row.contains(&format!("{DARK_GRAY}-{RESET}")));
        assert!(row.contains(&format!("{DARK_GRAY}peri{RESET}")));
        assert!(row.contains(&format!("{DARK_GRAY}Running{RESET}")));
        assert!(row.contains(&format!("{DARK_GRAY}Healthy{RESET}")));
    }

    #[test]
    fn deeper_spawn_rows_use_darker_shades() {
        let columns = vec![
            Column {
                title: "UNIT",
                width: 32,
                align: Alignment::Left,
            },
            Column {
                title: "KIND",
                width: 6,
                align: Alignment::Center,
            },
            Column {
                title: "STATE",
                width: 7,
                align: Alignment::Left,
            },
            Column {
                title: "USER",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "PID",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "CPU",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "RSS",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "UPTIME",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "CMD",
                width: 24,
                align: Alignment::Left,
            },
            Column {
                title: "LAST_EXIT",
                width: 10,
                align: Alignment::Left,
            },
            Column {
                title: "HEALTH",
                width: 8,
                align: Alignment::Left,
            },
        ];

        let mut shallow = SpawnedChild {
            name: "worker".to_string(),
            pid: 10,
            parent_pid: 1,
            command: "worker".to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth: 1,
            cpu_percent: None,
            rss_bytes: None,
            last_exit: None,
            user: Some("ubuntu".to_string()),
            kind: SpawnedChildKind::Spawned,
        };
        let shallow_row = format_spawned_child_row(&shallow, &columns, false, "└─ ");
        shallow.depth = 4;
        let deep_row = format_spawned_child_row(&shallow, &columns, false, "└─ ");

        assert!(shallow_row.contains(&format!("{DIM_WHITE}└─ worker{RESET}")));
        assert!(deep_row.contains(&format!("{DARK_GRAY}└─ worker{RESET}")));
    }

    #[test]
    fn peripheral_row_keeps_fixed_visible_width_when_cmd_is_truncated() {
        let columns = vec![
            Column {
                title: "UNIT",
                width: 24,
                align: Alignment::Left,
            },
            Column {
                title: "KIND",
                width: 6,
                align: Alignment::Center,
            },
            Column {
                title: "STATE",
                width: 7,
                align: Alignment::Left,
            },
            Column {
                title: "USER",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "PID",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "CPU",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "RSS",
                width: 8,
                align: Alignment::Right,
            },
            Column {
                title: "UPTIME",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "CMD",
                width: 16,
                align: Alignment::Left,
            },
            Column {
                title: "LAST_EXIT",
                width: 10,
                align: Alignment::Left,
            },
            Column {
                title: "HEALTH",
                width: 8,
                align: Alignment::Left,
            },
        ];

        let child = SpawnedChild {
            name: "/opt/homebrew/bin/claude".to_string(),
            pid: 73903,
            parent_pid: 73896,
            command: "/opt/homebrew/bin/claude --dangerously-skip-permissions --long-long-long-arg".to_string(),
            started_at: SystemTime::now(),
            ttl: None,
            depth: 4,
            cpu_percent: Some(0.0),
            rss_bytes: Some(253_100_000_000),
            last_exit: None,
            user: Some("rashad".to_string()),
            kind: SpawnedChildKind::Peripheral,
        };

        let row = format_spawned_child_row(&child, &columns, false, "└─ ");
        assert_eq!(visible_length(&row), total_inner_width(&columns) + 2);
    }

    #[test]
    fn truncate_unit_name_prefers_path_suffix() {
        let value = "/Users/rashad/dev/repos/systemg/examples/orchestrator/orchestrator-ui/node_modules/@esbuild/darwin-arm64/bin/esbuild";
        let truncated = truncate_unit_name(value, 24);
        assert_eq!(visible_length(&truncated), 24);
        assert!(truncated.starts_with("..."));
        assert!(truncated.ends_with("/bin/esbuild"));
    }

    #[test]
    fn truncate_nested_unit_label_keeps_tree_prefix() {
        let prefix = "   │  └─ ";
        let name = "/Users/rashad/dev/repos/systemg/examples/orchestrator/orchestrator-ui/node_modules/@esbuild/darwin-arm64/bin/esbuild";
        let width = 32;
        let label = truncate_nested_unit_label(prefix, name, width);
        assert_eq!(visible_length(&label), width);
        assert!(label.starts_with(prefix));
        assert!(label.ends_with("/bin/esbuild"));
    }

    #[test]
    fn truncate_nested_unit_label_truncates_prefix_if_no_room_for_name() {
        let prefix = "   │  └─ ";
        let label = truncate_nested_unit_label(prefix, "child", 6);
        assert_eq!(label, "   ...");
    }

    #[test]
    fn wrap_plain_text_splits_long_tokens() {
        let wrapped =
            wrap_plain_text("alpha beta-super-long-token-without-spaces omega", 10);
        assert!(wrapped.len() > 2);
        assert!(wrapped.iter().all(|line| visible_length(line) <= 10));
    }

    #[test]
    fn format_command_value_lines_wraps_and_colors_value_gray() {
        let lines = format_command_value_lines(
            "Configured",
            "sh -c veryveryveryveryverylongvalue --flag",
            28,
            false,
        );
        assert!(lines.len() > 1);
        assert!(lines[0].contains(&format!("{WHITE}Configured{RESET}: ")));
        assert!(lines.iter().all(|line| line.contains(GRAY)));
    }

    #[test]
    fn format_cpu_time_from_ticks_formats_centiseconds() {
        let rendered = format_cpu_time_from_ticks(1234);
        assert!(rendered.contains(':'));
        assert!(rendered.contains('.'));
    }

    #[test]
    fn sanitize_table_cell_collapses_control_characters() {
        let sanitized = sanitize_table_cell("foo\tbar\nbaz\rqux");
        assert_eq!(sanitized, "foo bar baz qux");
    }

    #[test]
    fn format_inspect_elapsed_omits_ago_suffix() {
        assert_eq!(format_inspect_elapsed(30), "30 secs");
        assert_eq!(format_inspect_elapsed(5 * 60), "5 mins");
    }

    #[test]
    fn format_row_sanitizes_multiline_cells() {
        let columns = vec![
            Column {
                title: "A",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "B",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "C",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "D",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "E",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "F",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "G",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "H",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "I",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "J",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "K",
                width: 8,
                align: Alignment::Left,
            },
        ];
        let values = [
            "ok".to_string(),
            "ok".to_string(),
            "ok".to_string(),
            "ok".to_string(),
            "ok".to_string(),
            "ok".to_string(),
            "ok".to_string(),
            "ok".to_string(),
            "cmd line one\nline two".to_string(),
            "ok".to_string(),
            "ok".to_string(),
        ];

        let row = format_row(&values, &columns);
        assert!(!row.contains('\n'));
        assert_eq!(visible_length(&row), total_inner_width(&columns) + 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_proc_stat_line_extracts_priority_and_cpu_ticks() {
        let sample = "1234 (bash) S 1000 1234 1234 0 -1 4194560 290 0 0 0 210 35 0 0 20 0 1 0 12345 123456789 1024 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let parsed = parse_proc_stat_line(sample).expect("parse stat line");
        assert_eq!(parsed.ppid, Some(1000));
        assert_eq!(parsed.priority, Some(20));
        assert_eq!(parsed.nice, Some(0));
        assert_eq!(parsed.cpu_ticks, Some(245));
    }

    #[test]
    fn drop_privileges_is_spawn_only() {
        assert!(drop_privileges_applies_to_command(&Commands::Start {
            config: "systemg.yaml".to_string(),
            daemonize: false,
            service: None,
            name: None,
            ttl: None,
            parent_pid: None,
            child: false,
            stderr: false,
            command: vec![],
        }));
        assert!(drop_privileges_applies_to_command(&Commands::Restart {
            config: "systemg.yaml".to_string(),
            service: None,
            daemonize: false,
        }));
        assert!(!drop_privileges_applies_to_command(&Commands::Status {
            config: "systemg.yaml".to_string(),
            service: None,
            all: false,
            json: false,
            no_color: false,
            full_cmd: false,
            stream: None,
        }));
    }

    #[test]
    fn target_table_width_uses_seventy_five_percent_of_terminal_width() {
        assert_eq!(target_table_width(1), 1);
        assert_eq!(target_table_width(2), 1);
        assert_eq!(target_table_width(3), 2);
        assert_eq!(target_table_width(80), 60);
        assert_eq!(target_table_width(120), 90);
        assert_eq!(target_table_width(200), 150);
    }

    #[test]
    fn status_widths_fit_terminal_width() {
        let mut widths = [30, 4, 7, 8, 7, 6, 8, 10, 30, 20, 8];
        shrink_status_widths_to_fit(&mut widths, 120);
        assert!(status_row_width(&widths) <= 120);
    }

    #[test]
    fn status_widths_fit_target_table_width() {
        let mut widths = [30, 4, 7, 8, 7, 6, 8, 10, 30, 20, 8];
        let target_width = target_table_width(120);
        shrink_status_widths_to_fit(&mut widths, target_width);
        assert!(status_row_width(&widths) <= target_width);
    }

    #[test]
    fn child_mode_requires_command() {
        let result = resolve_child_start(
            true,
            Some(1234),
            None,
            Some("worker".to_string()),
            &[],
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn child_mode_infers_from_parent_pid() {
        let result = resolve_child_start(
            false,
            Some(1234),
            Some(60),
            Some("worker".to_string()),
            &["sleep".to_string(), "1".to_string()],
            Some("debug".to_string()),
        )
        .expect("resolve child start")
        .expect("child mode should be inferred");

        assert_eq!(result.parent_pid, 1234);
        assert_eq!(result.name, "worker");
        assert_eq!(result.ttl, Some(60));
        assert_eq!(result.command, vec!["sleep".to_string(), "1".to_string()]);
        assert_eq!(result.log_level.as_deref(), Some("debug"));
    }

    #[test]
    fn status_width_shrink_priority_preserves_critical_columns() {
        let mut widths = [30, 4, 7, 8, 7, 6, 8, 10, 30, 20, 8];
        let original = widths;
        shrink_status_widths_to_fit(&mut widths, 120);

        assert_eq!(widths[STATUS_COL_PID], original[STATUS_COL_PID]);
        assert_eq!(widths[STATUS_COL_CPU], original[STATUS_COL_CPU]);
        assert_eq!(widths[STATUS_COL_RSS], original[STATUS_COL_RSS]);
        assert!(widths[STATUS_COL_UNIT] <= original[STATUS_COL_UNIT]);
        assert!(widths[STATUS_COL_CMD] <= original[STATUS_COL_CMD]);
    }

    #[test]
    fn status_widths_do_not_expand_when_terminal_is_wide() {
        let unit = UnitStatus {
            name: "app".to_string(),
            hash: "abc".to_string(),
            kind: UnitKind::Service,
            lifecycle: Some(ServiceLifecycleStatus::Running),
            health: UnitHealth::Healthy,
            process: None,
            uptime: None,
            last_exit: None,
            cron: None,
            metrics: None,
            command: Some("sh hello-world.sh".to_string()),
            runtime_command: None,
            spawned_children: vec![],
        };
        let widths = compute_status_preferred_widths(&[unit], true);
        let mut fitted = widths;
        shrink_status_widths_to_fit(&mut fitted, 240);
        assert_eq!(fitted, widths);
    }

    #[test]
    fn status_widths_balance_unit_and_cmd_columns() {
        let mut widths = [8, 4, 7, 8, 7, 6, 8, 10, 90, 20, 8];
        shrink_status_widths_to_fit(&mut widths, 120);

        let diff = widths[STATUS_COL_UNIT].abs_diff(widths[STATUS_COL_CMD]);
        assert!(
            diff <= STATUS_UNIT_CMD_MAX_DIFF,
            "expected UNIT/CMD widths to stay close, got UNIT={} CMD={}",
            widths[STATUS_COL_UNIT],
            widths[STATUS_COL_CMD]
        );
    }

    #[test]
    fn inspect_process_widths_fit_terminal_width() {
        let rows = vec![InspectProcessRow {
            tree_label: "└─ very-long-process-name-with-depth".to_string(),
            is_root: true,
            depth: 0,
            pid: 12345,
            ppid: Some(1234),
            user: "engineer".to_string(),
            pri: Some(20),
            nice: Some(0),
            virt_bytes: 5_240_000_000,
            res_bytes: 250_000_000,
            shared_bytes: Some(64_000_000),
            state: "R".to_string(),
            cpu_percent: 67.3,
            mem_percent: 2.1,
            cpu_time: "15:42.11".to_string(),
            command: "sh very-long-command --with many args and values".to_string(),
        }];
        let mut widths = compute_inspect_process_preferred_widths(&rows);
        shrink_inspect_process_widths_to_fit(&mut widths, 120);
        assert!(inspect_process_row_width(&widths) <= 120);
    }

    #[test]
    fn inspect_process_widths_fit_target_table_width() {
        let rows = vec![InspectProcessRow {
            tree_label: "└─ very-long-process-name-with-depth".to_string(),
            is_root: true,
            depth: 0,
            pid: 12345,
            ppid: Some(1234),
            user: "engineer".to_string(),
            pri: Some(20),
            nice: Some(0),
            virt_bytes: 5_240_000_000,
            res_bytes: 250_000_000,
            shared_bytes: Some(64_000_000),
            state: "R".to_string(),
            cpu_percent: 67.3,
            mem_percent: 2.1,
            cpu_time: "15:42.11".to_string(),
            command: "sh very-long-command --with many args and values".to_string(),
        }];
        let mut widths = compute_inspect_process_preferred_widths(&rows);
        let target_width = target_table_width(120);
        shrink_inspect_process_widths_to_fit(&mut widths, target_width);
        assert!(inspect_process_row_width(&widths) <= target_width);
    }

    #[test]
    fn inspect_process_shrink_priority_prefers_proc_and_cmd() {
        let mut widths = [30, 7, 7, 8, 4, 4, 9, 9, 9, 1, 6, 6, 9, 30];
        let original = widths;
        shrink_inspect_process_widths_to_fit(&mut widths, 120);

        assert_eq!(widths[INSPECT_COL_PID], original[INSPECT_COL_PID]);
        assert_eq!(widths[INSPECT_COL_CPU], original[INSPECT_COL_CPU]);
        assert_eq!(widths[INSPECT_COL_MEM], original[INSPECT_COL_MEM]);
        assert!(widths[INSPECT_COL_PROC] <= original[INSPECT_COL_PROC]);
        assert!(widths[INSPECT_COL_CMD] <= original[INSPECT_COL_CMD]);
    }

    #[test]
    fn inspect_process_widths_balance_proc_and_cmd_columns() {
        let mut widths = [8, 7, 7, 8, 4, 4, 9, 9, 9, 1, 6, 6, 9, 90];
        shrink_inspect_process_widths_to_fit(&mut widths, 120);

        let diff = widths[INSPECT_COL_PROC].abs_diff(widths[INSPECT_COL_CMD]);
        assert!(
            diff <= INSPECT_PROC_CMD_MAX_DIFF,
            "expected PROC/CMD widths to stay close, got PROC={} CMD={}",
            widths[INSPECT_COL_PROC],
            widths[INSPECT_COL_CMD]
        );
    }

    #[test]
    fn inspect_process_descendant_rows_darken_by_depth() {
        let mut user_colors = HashMap::new();
        let user_color = "\x1b[38;5;39m";
        user_colors.insert("ubuntu".to_string(), user_color);

        let root = InspectProcessRow {
            tree_label: "sh".to_string(),
            is_root: true,
            depth: 0,
            pid: 1,
            ppid: None,
            user: "ubuntu".to_string(),
            pri: Some(20),
            nice: Some(0),
            virt_bytes: 10_000,
            res_bytes: 5_000,
            shared_bytes: Some(1_000),
            state: "S".to_string(),
            cpu_percent: 0.0,
            mem_percent: 0.0,
            cpu_time: "00:00.00".to_string(),
            command: "sh -c run".to_string(),
        };
        let child = InspectProcessRow {
            tree_label: "└─ worker".to_string(),
            is_root: false,
            depth: 2,
            pid: 2,
            ppid: Some(1),
            user: "ubuntu".to_string(),
            pri: Some(20),
            nice: Some(0),
            virt_bytes: 10_000,
            res_bytes: 5_000,
            shared_bytes: Some(1_000),
            state: "S".to_string(),
            cpu_percent: 0.0,
            mem_percent: 0.0,
            cpu_time: "00:00.00".to_string(),
            command: "python worker.py".to_string(),
        };

        let root_values = inspect_process_row_values(&root, &user_colors, false);
        let child_values = inspect_process_row_values(&child, &user_colors, false);

        assert!(!root_values[INSPECT_COL_PROC].contains(GRAY));
        assert!(!root_values[INSPECT_COL_CMD].contains(GRAY));
        assert!(root_values[INSPECT_COL_USER].contains(user_color));
        assert!(root_values[INSPECT_COL_VIRT].contains(GREEN));
        assert!(child_values[INSPECT_COL_PROC].contains(GRAY));
        assert!(child_values[INSPECT_COL_CMD].contains(GRAY));
        assert!(child_values[INSPECT_COL_USER].contains(GRAY));
        assert!(child_values[INSPECT_COL_VIRT].contains(GRAY));
    }

    #[test]
    fn inspect_cron_widths_fit_target_table_width() {
        let rows = vec![InspectCronRunRow {
            run: "10".to_string(),
            time: "2026-03-10 14:03:00".to_string(),
            user: "postgres".to_string(),
            pid: "12345".to_string(),
            command: "sh scripts/migrate-provider-data.sh --delete".to_string(),
        }];
        let mut widths = compute_inspect_cron_preferred_widths(&rows);
        let target_width = target_table_width(120);
        shrink_inspect_cron_widths_to_fit(&mut widths, target_width);
        assert!(inspect_cron_row_width(&widths) <= target_width);
    }

    #[test]
    fn inspect_cron_width_shrink_prioritizes_command_column() {
        let rows = vec![InspectCronRunRow {
            run: "10".to_string(),
            time: "2026-03-10 14:03:00".to_string(),
            user: "postgres".to_string(),
            pid: "12345".to_string(),
            command: "sh scripts/migrate-provider-data.sh --delete --sink rds --force"
                .to_string(),
        }];
        let mut widths = compute_inspect_cron_preferred_widths(&rows);
        let original_cmd = widths[4];
        shrink_inspect_cron_widths_to_fit(&mut widths, 60);
        assert!(widths[4] < original_cmd);
        assert!(widths[2] >= INSPECT_CRON_SOFT_MIN_WIDTHS[2]);
    }

    #[test]
    fn test_format_uptime_short() {
        assert_eq!(format_uptime_short("30 secs ago"), "< 1m");
        assert_eq!(format_uptime_short("5 mins ago"), "5m");
        assert_eq!(format_uptime_short("90 mins ago"), "1h");
        assert_eq!(format_uptime_short("3 hours ago"), "3h");
        assert_eq!(format_uptime_short("25 hours ago"), "1d");
        assert_eq!(format_uptime_short("4 days ago"), "4d");
        assert_eq!(format_uptime_short("2 weeks ago"), "2w");
    }

    #[test]
    fn test_format_relative_time_short() {
        use chrono::Duration as ChronoDuration;

        let now = Utc::now();
        let thirty_secs_ago = now - ChronoDuration::seconds(30);
        let five_mins_ago = now - ChronoDuration::minutes(5);
        let two_hours_ago = now - ChronoDuration::hours(2);
        let three_days_ago = now - ChronoDuration::days(3);

        assert_eq!(format_relative_time_short(thirty_secs_ago), "<1m");
        assert_eq!(format_relative_time_short(five_mins_ago), "5m");
        assert_eq!(format_relative_time_short(two_hours_ago), "2h");
        assert_eq!(format_relative_time_short(three_days_ago), "3d");
    }

    #[test]
    fn test_format_last_exit_human_readable() {
        let exit_zero = Some(ExitMetadata {
            exit_code: Some(0),
            signal: None,
        });
        let exit_one = Some(ExitMetadata {
            exit_code: Some(1),
            signal: None,
        });
        let signal_kill = Some(ExitMetadata {
            exit_code: None,
            signal: Some(9),
        });

        assert_eq!(format_last_exit(exit_zero.as_ref(), None), "exit 0");
        assert_eq!(format_last_exit(exit_one.as_ref(), None), "exit 1");
        assert_eq!(format_last_exit(signal_kill.as_ref(), None), "exit ?");
        assert_eq!(format_last_exit(None, None), "-");
    }

    #[test]
    fn test_last_exit_color_uses_exit_code() {
        let success = ExitMetadata {
            exit_code: Some(0),
            signal: None,
        };
        let failure = ExitMetadata {
            exit_code: Some(2),
            signal: None,
        };
        let signaled = ExitMetadata {
            exit_code: None,
            signal: Some(9),
        };

        assert_eq!(last_exit_color(Some(&success), None), Some(GREEN_BOLD));
        assert_eq!(last_exit_color(Some(&failure), None), Some(RED_BOLD));
        assert_eq!(last_exit_color(Some(&signaled), None), Some(RED_BOLD));
        assert_eq!(last_exit_color(None, None), None);
    }

    #[test]
    fn prune_unit_configs_respects_max_files() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let units_dir = temp.path();
        let now = SystemTime::now();

        for idx in 0..3 {
            let path = units_dir.join(format!("unit-{idx}.yaml"));
            fs::write(path, "version: \"1\"\nservices: {}\n").expect("write unit file");
            std::thread::sleep(Duration::from_millis(10));
        }

        prune_unit_configs_with_limits(
            units_dir,
            now + Duration::from_secs(1),
            2,
            Duration::from_secs(60),
        )
        .expect("prune configs");

        let yaml_count = fs::read_dir(units_dir)
            .expect("read units dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|ext| ext.to_str()) == Some("yaml")
            })
            .count();
        assert_eq!(yaml_count, 2);
    }

    #[test]
    fn prune_unit_configs_respects_max_age() {
        let temp = tempfile::tempdir().expect("create tempdir");
        let units_dir = temp.path();
        let now = SystemTime::now();

        let path = units_dir.join("old.yaml");
        fs::write(&path, "version: \"1\"\nservices: {}\n").expect("write unit file");

        prune_unit_configs_with_limits(
            units_dir,
            now + Duration::from_secs(5),
            10,
            Duration::from_secs(1),
        )
        .expect("prune configs");

        assert!(!path.exists(), "file older than max age should be pruned");
    }
}

include!("sysg/ui.rs");

/// Handles purge all state.
fn purge_all_state() -> Result<(), Box<dyn Error>> {
    stop_supervisors();

    let runtime_dir = runtime::state_dir();

    if runtime_dir.exists() {
        info!("Removing systemg runtime directory: {:?}", runtime_dir);
        fs::remove_dir_all(&runtime_dir)?;
        info!("Successfully purged all systemg state");
    } else {
        info!("No systemg runtime directory found at {:?}", runtime_dir);
    }

    Ok(())
}

/// Stops supervisors.
fn stop_supervisors() {
    let candidates = gather_supervisor_pids();

    if candidates.is_empty() {
        return;
    }

    let mut survivors = HashSet::new();
    let mut fallback_targets = HashSet::new();

    if supervisor_running() {
        match send_control_command(ControlCommand::Shutdown) {
            Ok(_) => {
                for pid in &candidates {
                    if !wait_for_supervisor_exit(*pid, Duration::from_secs(3)) {
                        fallback_targets.insert(*pid);
                    }
                }
            }
            Err(err) => {
                if let Some(control_err) = err.downcast_ref::<ControlError>() {
                    match control_err {
                        ControlError::NotAvailable => warn!(
                            "Supervisor IPC unavailable during purge; falling back to signal-based shutdown"
                        ),
                        other => warn!("Failed to request supervisor shutdown: {other}"),
                    }
                } else {
                    warn!("Failed to request supervisor shutdown: {err}");
                }
                fallback_targets.extend(&candidates);
            }
        }
    } else {
        fallback_targets.extend(&candidates);
    }

    survivors.extend(fallback_targets);

    if survivors.is_empty() {
        return;
    }

    for pid in gather_supervisor_pids() {
        survivors.insert(pid);
    }

    for pid in survivors {
        force_kill(pid);
    }
}

/// Gathers supervisor pids.
fn gather_supervisor_pids() -> HashSet<libc::pid_t> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );

    let mut set = HashSet::new();

    if let Ok(Some(pid)) = ipc::read_supervisor_pid() {
        set.insert(pid);
    }

    let current_pid = process::id();

    for (pid, process) in system.processes() {
        if pid.as_u32() == current_pid {
            continue;
        }

        if is_supervisor(process) {
            set.insert(pid.as_u32() as libc::pid_t);
        }
    }

    set
}

/// Returns whether supervisor.
fn is_supervisor(process: &sysinfo::Process) -> bool {
    let cmd = process.cmd();
    if cmd.is_empty() {
        return false;
    }

    let binary = cmd
        .first()
        .map(|arg| arg.to_string_lossy())
        .unwrap_or_default();

    if !(binary.ends_with("sysg") || binary.contains("/sysg")) {
        return false;
    }

    let mut has_start = false;
    let mut has_daemonize = false;

    for arg in cmd {
        let value = arg.to_string_lossy();
        if value == "start" {
            has_start = true;
        } else if value == "--daemonize" {
            has_daemonize = true;
        }
    }

    has_start && has_daemonize
}

/// Waits for for supervisor exit.
fn wait_for_supervisor_exit(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let target = Pid::from_raw(pid);

    while Instant::now() < deadline {
        match signal::kill(target, None) {
            Ok(_) => {
                if process_exited(pid) {
                    return true;
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                if err == nix::Error::from(nix::errno::Errno::ESRCH) {
                    return true;
                }
                if process_exited(pid) {
                    return true;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    false
}

/// Forcefully terminates kill.
fn force_kill(pid: libc::pid_t) {
    if wait_for_supervisor_exit(pid, Duration::from_millis(100)) {
        return;
    }

    let pgid = unsafe { libc::getpgid(pid) };

    if pgid >= 0 && pgid == pid {
        unsafe { libc::killpg(pgid, SIGTERM) };
    } else {
        unsafe { libc::kill(pid, SIGTERM) };
    }

    if wait_for_supervisor_exit(pid, Duration::from_secs(2)) {
        return;
    }

    if pgid >= 0 && pgid == pid {
        unsafe { libc::killpg(pgid, SIGKILL) };
    } else {
        unsafe { libc::kill(pid, SIGKILL) };
    }

    let _ = wait_for_supervisor_exit(pid, Duration::from_secs(2));
}

/// Returns whether the tracked root PID is still alive.
fn tracked_service_alive(pid: libc::pid_t) -> bool {
    let target = Pid::from_raw(pid);
    match signal::kill(target, None) {
        Ok(_) => true,
        Err(err) => err != nix::Error::from(nix::errno::Errno::ESRCH),
    }
}

/// Waits for a tracked service root PID to exit.
fn wait_for_tracked_service_exit(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !tracked_service_alive(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    !tracked_service_alive(pid)
}

/// Sends a signal to a tracked service using its process group when available and falling back
/// to the root PID when group signaling is not possible.
fn signal_tracked_service(
    service: &str,
    pid: libc::pid_t,
    pgid: Option<libc::pid_t>,
    signal: libc::c_int,
) {
    let mut delivered = false;

    if let Some(group_id) = pgid {
        unsafe {
            if libc::killpg(group_id, signal) == 0 {
                delivered = true;
            } else {
                let err = std::io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(code) if code == libc::ESRCH => {
                        delivered = true;
                    }
                    Some(code) if code == libc::EPERM => {}
                    _ => eprintln!(
                        "systemg: failed to send signal {signal} to '{service}' (pgid {group_id}): {err}"
                    ),
                }
            }
        }
    }

    if delivered {
        return;
    }

    unsafe {
        if libc::kill(pid, signal) == -1 {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(code) if code == libc::ESRCH => {}
                _ => eprintln!(
                    "systemg: failed to send signal {signal} to '{service}' (pid {pid}): {err}"
                ),
            }
        }
    }
}

/// Terminates tracked services from the pid file during foreground Ctrl-C shutdown.
fn terminate_tracked_services_on_shutdown() {
    let mut tracked: Vec<(String, libc::pid_t, Option<libc::pid_t>)> = Vec::new();
    if let Ok(pid_file) = PidFile::load() {
        for (service, pid) in pid_file.services() {
            tracked.push((
                service.clone(),
                *pid as libc::pid_t,
                pid_file.pgid_for(service).map(|group| group as libc::pid_t),
            ));
        }
    }

    for (service, pid, pgid) in &tracked {
        signal_tracked_service(service, *pid, *pgid, libc::SIGTERM);
    }

    for (service, pid, pgid) in &tracked {
        if wait_for_tracked_service_exit(*pid, Duration::from_secs(2)) {
            continue;
        }
        signal_tracked_service(service, *pid, *pgid, libc::SIGKILL);
    }

    for (service, pid, _) in &tracked {
        if !wait_for_tracked_service_exit(*pid, Duration::from_secs(2)) {
            eprintln!(
                "systemg: service '{service}' (pid {pid}) survived foreground shutdown"
            );
        }
    }
}

/// Handles process exited.
fn process_exited(pid: libc::pid_t) -> bool {
    let proc_root = PathBuf::from(format!("/proc/{pid}"));
    if !proc_root.exists() {
        return true;
    }

    match read_proc_state(pid) {
        Some('Z') | Some('X') => true,
        Some(_) => false,
        None => false,
    }
}

/// Reads proc state.
fn read_proc_state(pid: libc::pid_t) -> Option<char> {
    let stat_path = PathBuf::from(format!("/proc/{pid}/stat"));
    let contents = fs::read_to_string(stat_path).ok()?;
    let mut parts = contents.split_whitespace();
    parts.next()?;
    let mut name_part = parts.next()?;
    if !name_part.ends_with(')') {
        for part in parts.by_ref() {
            name_part = part;
            if name_part.ends_with(')') {
                break;
            }
        }
    }

    parts.next()?.chars().next()
}

/// Initializes logging.
fn init_logging(args: &Cli, use_file: bool) {
    let filter = if let Some(level) = args.log_level {
        EnvFilter::new(level.as_str())
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };

    if use_file {
        let log_dir = runtime::log_dir();
        if let Err(err) = fs::create_dir_all(&log_dir) {
            eprintln!("Failed to create log directory {:?}: {}", log_dir, err);
        }
        let log_path = log_dir.join("supervisor.log");

        let file = match fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(file) => file,
            Err(e) => {
                eprintln!("Failed to open supervisor log file {:?}: {}", log_path, e);
                let _ = tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_writer(std::io::stderr)
                    .try_init();
                return;
            }
        };

        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(move || file.try_clone().unwrap())
            .with_ansi(false)
            .try_init();
    } else {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .try_init();
    }
}

/// Starts foreground.
fn start_foreground(
    config_path: PathBuf,
    service: Option<String>,
    pipe_stderr: bool,
) -> Result<(), Box<dyn Error>> {
    let mut supervisor = Supervisor::new(config_path, false, service)?;
    supervisor.set_pipe_stderr(pipe_stderr);
    supervisor.run()?;
    Ok(())
}

/// Starts supervisor daemon.
fn start_supervisor_daemon(
    config_path: PathBuf,
    service: Option<String>,
    pipe_stderr: bool,
) -> Result<(), Box<dyn Error>> {
    daemonize_systemg()?;

    let mut supervisor = Supervisor::new(config_path, false, service)?;
    supervisor.set_pipe_stderr(pipe_stderr);
    if let Err(err) = supervisor.run() {
        error!("Supervisor exited with error: {err}");
    }

    Ok(())
}

/// Builds daemon.
fn build_daemon(config_path: &str) -> Result<Daemon, Box<dyn Error>> {
    let config = load_config(Some(config_path))?;
    let daemon = Daemon::from_config(config, false)?;
    Ok(daemon)
}

/// Represents start target.
struct StartTarget {
    config_path: PathBuf,
    service: Option<String>,
    ad_hoc: bool,
}

/// Represents child start request.
struct ChildStartRequest {
    parent_pid: u32,
    name: String,
    command: Vec<String>,
    ttl: Option<u64>,
    log_level: Option<String>,
}

/// Resolves child start.
fn resolve_child_start(
    child: bool,
    parent_pid: Option<u32>,
    ttl: Option<u64>,
    name: Option<String>,
    command: &[String],
    log_level: Option<String>,
) -> Result<Option<ChildStartRequest>, Box<dyn Error>> {
    let child_mode = child || parent_pid.is_some() || ttl.is_some();
    if !child_mode {
        return Ok(None);
    }

    if command.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "child start mode requires a command; use `sysg start --parent-pid <pid> --name <name> -- <command...>`",
        )
        .into());
    }

    if child && parent_pid.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--child requires --parent-pid",
        )
        .into());
    }

    let parent_pid = parent_pid.unwrap_or_else(|| unsafe { getppid() } as u32);
    let name = name.unwrap_or_else(|| default_child_name(command));
    Ok(Some(ChildStartRequest {
        parent_pid,
        name: sanitize_service_name(&name),
        command: command.to_vec(),
        ttl,
        log_level,
    }))
}

/// Runs child start.
fn run_child_start(request: ChildStartRequest) -> Result<(), Box<dyn Error>> {
    let spawn_cmd = ControlCommand::Spawn {
        parent_pid: request.parent_pid,
        name: request.name,
        command: request.command,
        ttl: request.ttl,
        log_level: request.log_level,
    };

    match ipc::send_command(&spawn_cmd) {
        Ok(ControlResponse::Spawned { pid }) => {
            println!("{}", pid);
            Ok(())
        }
        Ok(ControlResponse::Error(msg)) => {
            Err(io::Error::other(format!("Failed to start child process: {msg}")).into())
        }
        Ok(_) => Err(io::Error::other("Unexpected response from supervisor").into()),
        Err(err) => Err(io::Error::other(format!(
            "Failed to communicate with supervisor: {err}"
        ))
        .into()),
    }
}

/// Resolves start target.
fn resolve_start_target(
    config: &str,
    service: Option<String>,
    requested_name: Option<&str>,
    command: Vec<String>,
) -> Result<StartTarget, Box<dyn Error>> {
    if command.is_empty() {
        if requested_name.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--name requires a unit command or child mode",
            )
            .into());
        }
        return Ok(StartTarget {
            config_path: resolve_config_path(config)?,
            service,
            ad_hoc: false,
        });
    }

    if service.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--service cannot be used with unit commands; use --name for units",
        )
        .into());
    }

    let config_path = write_ad_hoc_config(&command, requested_name)?;
    Ok(StartTarget {
        config_path,
        service: None,
        ad_hoc: true,
    })
}

/// Writes ad hoc config.
fn write_ad_hoc_config(
    command: &[String],
    requested_name: Option<&str>,
) -> Result<PathBuf, Box<dyn Error>> {
    let service_name = requested_name
        .map(sanitize_service_name)
        .unwrap_or_else(|| {
            let base = command
                .first()
                .map(|value| sanitize_service_name(value))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "unit".to_string());
            let hash = short_command_hash(command);
            format!("{base}-{hash}")
        });

    let shell_command = render_shell_command(command);
    let hash = short_command_hash(command);
    let units_dir = runtime::state_dir().join("units");
    fs::create_dir_all(&units_dir)?;

    let config_path = units_dir.join(format!("{service_name}-{hash}.yaml"));
    let yaml = format!(
        "version: \"1\"\nservices:\n  {name}:\n    command: {command}\n",
        name = service_name,
        command = yaml_single_quoted(&shell_command)
    );
    fs::write(&config_path, yaml)?;
    if let Err(err) = prune_unit_configs(&units_dir) {
        warn!("Failed to prune unit configs in {:?}: {err}", units_dir);
    }
    Ok(config_path)
}

/// Prunes unit configs.
fn prune_unit_configs(units_dir: &Path) -> io::Result<()> {
    let max_age = Duration::from_secs(UNIT_CONFIG_MAX_AGE_DAYS * SECONDS_PER_DAY);
    prune_unit_configs_with_limits(
        units_dir,
        SystemTime::now(),
        UNIT_CONFIG_MAX_FILES,
        max_age,
    )
}

/// Prunes unit configs with limits.
fn prune_unit_configs_with_limits(
    units_dir: &Path,
    now: SystemTime,
    max_files: usize,
    max_age: Duration,
) -> io::Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(units_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yaml") {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        entries.push((path, modified));
    }

    let mut fresh_entries = Vec::new();
    for (path, modified) in entries {
        let is_stale = now
            .duration_since(modified)
            .map(|age| age > max_age)
            .unwrap_or(false);
        if is_stale {
            let _ = fs::remove_file(&path);
        } else {
            fresh_entries.push((path, modified));
        }
    }

    fresh_entries.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, _) in fresh_entries.into_iter().skip(max_files) {
        let _ = fs::remove_file(path);
    }

    Ok(())
}

/// Returns the default child name.
fn default_child_name(command: &[String]) -> String {
    let base = command
        .first()
        .map(|value| sanitize_service_name(value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "child".to_string());
    let hash = short_command_hash(command);
    format!("{base}-{hash}")
}

/// Builds the short command hash.
fn short_command_hash(command: &[String]) -> String {
    let mut hasher = Sha256::new();
    for part in command {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    let digest = hasher.finalize();
    format!("{:x}", digest)[..12].to_string()
}

/// Sanitizes service name.
fn sanitize_service_name(input: &str) -> String {
    let mut sanitized = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            sanitized.push(ch);
        } else {
            sanitized.push('-');
        }
    }

    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "unit".to_string()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

/// Renders shell command.
fn render_shell_command(command: &[String]) -> String {
    command
        .iter()
        .map(|part| shell_escape_arg(part))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Escapes escape arg.
fn shell_escape_arg(input: &str) -> String {
    if input.is_empty() {
        return "''".to_string();
    }

    if input
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "_-./:@%+=".contains(ch))
    {
        return input.to_string();
    }

    let escaped = input.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

/// Formats single quoted.
fn yaml_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Resolves config path.
fn resolve_config_path(path: &str) -> Result<PathBuf, Box<dyn Error>> {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return Ok(candidate);
    }

    let cwd_candidate = std::env::current_dir()?.join(&candidate);
    if cwd_candidate.exists() {
        return Ok(cwd_candidate.canonicalize().unwrap_or(cwd_candidate));
    }

    for dir in runtime::config_dirs() {
        let candidate_path = dir.join(&candidate);
        if candidate_path.exists() {
            return Ok(candidate_path);
        }
    }

    Ok(cwd_candidate)
}

/// Handles supervisor running.
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
        Ok(None) | Err(_) => {
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

/// Sends control command.
fn send_control_command(command: ControlCommand) -> Result<(), Box<dyn Error>> {
    match ipc::send_command(&command) {
        Ok(ControlResponse::Message(message)) => {
            println!("{message}");
            Ok(())
        }
        Ok(ControlResponse::Ok) => Ok(()),
        Ok(ControlResponse::Status(_)) => Ok(()),
        Ok(ControlResponse::Inspect(_)) => Ok(()),
        Ok(ControlResponse::Spawned { pid }) => {
            println!("Spawned process with PID: {}", pid);
            Ok(())
        }
        Ok(ControlResponse::Error(message)) => Err(ControlError::Server(message).into()),
        Err(ControlError::NotAvailable) => {
            warn!("No running systemg supervisor found; skipping command");
            let _ = ipc::cleanup_runtime();
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

/// Handles daemonize systemg.
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

/// Registers signal handler.
fn register_signal_handler() -> Result<(), Box<dyn Error>> {
    ctrlc::set_handler(move || {
        println!("systemg is shutting down... terminating child services");
        terminate_tracked_services_on_shutdown();
        let _ = ipc::cleanup_runtime();
        std::process::exit(0);
    })?;

    Ok(())
}
