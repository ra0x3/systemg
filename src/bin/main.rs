use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fs, io,
    io::Write,
    os::unix::io::IntoRawFd,
    path::PathBuf,
    process,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use libc::{SIGKILL, SIGTERM, getpgrp, getppid, killpg};
use nix::{
    sys::signal,
    unistd::{Pid, Uid},
};
use sysinfo::{ProcessesToUpdate, System};
use systemg::{
    charting::{self, ChartConfig, parse_window_duration},
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

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args();
    let euid = Uid::effective();

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
    runtime::set_drop_privileges(args.drop_privileges);
    if args.drop_privileges && !euid.is_root() {
        warn!("--drop-privileges has no effect when not running as root");
    }
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

    if euid.is_root() && runtime_mode == RuntimeMode::User {
        warn!("Running as root without --sys; state will be stored in userspace paths");
    }

    match args.command {
        Commands::Start {
            config,
            daemonize,
            service,
        } => {
            if daemonize {
                if supervisor_running() {
                    // If supervisor is running and we have a specific service, send Start command
                    if let Some(service_name) = service {
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
            json,
            no_color,
            full_cmd,
            watch,
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

            if let Some(interval) = watch {
                let sleep_interval = Duration::from_secs(interval.max(1));
                loop {
                    match fetch_status_snapshot(&effective_config) {
                        Ok(snapshot) => {
                            if let Err(e) = render_status(&snapshot, &render_opts, true) {
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
                            println!("Press Ctrl+C to exit watch mode.");
                        }
                    }
                    thread::sleep(sleep_interval);
                }
            } else {
                let snapshot = fetch_status_snapshot(&effective_config)?;
                let health = render_status(&snapshot, &render_opts, false)?;
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
            unit,
            json,
            no_color,
            window,
        } => {
            let mut effective_config = config.clone();
            if load_config(Some(&config)).is_err()
                && let Ok(Some(hint)) = ipc::read_config_hint()
            {
                effective_config = hint.to_string_lossy().to_string();
            }

            // Parse the window duration
            let window_seconds = match parse_window_duration(&window) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Invalid window duration '{}': {}", window, e);
                    process::exit(1);
                }
            };

            // Calculate samples limit based on window
            let samples_limit = if window_seconds < 3600 {
                window_seconds as usize // For short windows, 1 sample per second
            } else {
                720 // For longer windows, cap at 720 samples
            };

            let payload = fetch_inspect(&effective_config, &unit, samples_limit)?;
            if payload.unit.is_none() {
                eprintln!("Unit '{unit}' not found.");
                process::exit(2);
            }

            let render_opts = InspectRenderOptions {
                json,
                no_color,
                window_seconds,
                window_desc: window.clone(),
                samples_limit,
            };

            let health = render_inspect(&payload, &render_opts)?;
            let exit_code = match health {
                OverallHealth::Healthy => 0,
                OverallHealth::Degraded => 1,
                OverallHealth::Failing => 2,
            };
            process::exit(exit_code);
        }
        Commands::Logs {
            config,
            service,
            lines,
            kind,
        } => {
            // Try to determine the actual config path, falling back to hint if needed
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
            match service {
                Some(service) => {
                    info!("Fetching logs for service: {service}");
                    let process_pid = pid.lock().unwrap().pid_for(&service);

                    if let Some(process_pid) = process_pid {
                        manager.show_log(
                            &service,
                            process_pid,
                            lines,
                            Some(kind.as_str()),
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
                                Some(kind.as_str()),
                            )?;
                        } else {
                            warn!("Service '{service}' is not currently running");
                        }
                    }
                }
                None => {
                    info!("Fetching logs for all services");
                    manager.show_logs(
                        lines,
                        Some(kind.as_str()),
                        Some(&effective_config),
                    )?;
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
            let parent_pid = parent_pid.unwrap_or_else(|| unsafe { getppid() } as u32);

            let spawn_cmd = ControlCommand::Spawn {
                parent_pid,
                name: name.clone(),
                command,
                ttl,
                log_level: log_level.map(|l| l.as_str().to_string()),
            };

            match ipc::send_command(&spawn_cmd) {
                Ok(ControlResponse::Spawned { pid }) => {
                    println!("{}", pid);
                }
                Ok(ControlResponse::Error(msg)) => {
                    error!("Failed to spawn child: {}", msg);
                    std::process::exit(1);
                }
                Ok(_) => {
                    error!("Unexpected response from supervisor");
                    std::process::exit(1);
                }
                Err(err) => {
                    error!("Failed to communicate with supervisor: {}", err);
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
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
    fn peripheral_spawn_rows_render_selected_columns_in_gray() {
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
        assert!(row.contains(&format!("{GRAY}└─ /opt/homebrew/bin/claude{RESET}")));
        assert!(row.contains(&format!("{GRAY}rashad{RESET}")));
        assert!(row.contains(&format!("{GRAY}62751{RESET}")));
        assert!(row.contains(&format!("{GRAY}0.0%{RESET}")));
        assert!(row.contains(&format!("{GRAY}117.7MB{RESET}")));
        assert!(row.contains(&format!(
            "{GRAY}/opt/homebrew/bin/claude --dangerously-skip-permissions{RESET}"
        )));
        assert!(row.contains(&format!("{GRAY}-{RESET}")));
        assert!(row.contains(&format!("{ORANGE}peri{RESET}")));
        assert!(row.contains(&format!("{BRIGHT_GREEN}Running{RESET}")));
        assert!(row.contains(&format!("{GREEN_BOLD}Healthy{RESET}")));
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
}

struct StatusRenderOptions<'a> {
    json: bool,
    no_color: bool,
    full_cmd: bool,
    include_orphans: bool,
    service_filter: Option<&'a str>,
}

struct InspectRenderOptions {
    json: bool,
    no_color: bool,
    window_seconds: u64,
    window_desc: String,
    samples_limit: usize,
}

const GREEN_BOLD: &str = "\x1b[1;32m";
const GREEN: &str = "\x1b[32m";
const DARK_GREEN: &str = "\x1b[38;5;22m"; // Darker green for partial success
const BRIGHT_GREEN: &str = "\x1b[92m";
const YELLOW_BOLD: &str = "\x1b[1;33m";
const RED_BOLD: &str = "\x1b[1;31m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const ORANGE: &str = "\x1b[38;5;208m";
const GRAY: &str = "\x1b[90m";
const MAGENTA: &str = "\x1b[35m";
const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy)]
enum Alignment {
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy)]
struct Column {
    title: &'static str,
    width: usize,
    align: Alignment,
}

fn fetch_status_snapshot(config_path: &str) -> Result<StatusSnapshot, Box<dyn Error>> {
    match ipc::send_command(&ControlCommand::Status) {
        Ok(ControlResponse::Status(snapshot)) => Ok(snapshot),
        Ok(other) => Err(io::Error::other(format!(
            "unexpected supervisor response: {:?}",
            other
        ))
        .into()),
        Err(ControlError::NotAvailable) => {
            let config = load_config(Some(config_path)).ok();
            collect_disk_snapshot(config).map_err(|err| Box::new(err) as Box<dyn Error>)
        }
        Err(err) => Err(Box::new(err)),
    }
}

fn render_status(
    snapshot: &StatusSnapshot,
    opts: &StatusRenderOptions,
    watch_mode: bool,
) -> Result<OverallHealth, Box<dyn Error>> {
    let mut units: Vec<UnitStatus> = snapshot
        .units
        .iter()
        .filter(|unit| opts.include_orphans || unit.kind != UnitKind::Orphaned)
        .cloned()
        .collect();

    if let Some(filter) = opts.service_filter {
        units.retain(|unit| unit.name == filter || unit.hash == filter);
    }

    if units.is_empty() {
        println!("No matching units found.");
        return Ok(OverallHealth::Degraded);
    }

    let health = compute_overall_health(&units);

    if opts.json {
        let filtered_snapshot = StatusSnapshot {
            schema_version: snapshot.schema_version.clone(),
            captured_at: snapshot.captured_at,
            overall_health: health,
            units,
        };
        println!("{}", serde_json::to_string_pretty(&filtered_snapshot)?);
        return Ok(health);
    }

    if watch_mode {
        print!("\x1B[2J\x1B[H");
        let _ = io::stdout().flush();
    }

    let timestamp = snapshot
        .captured_at
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S %Z");

    // Calculate maximum width for STATE column
    let max_state_len = units
        .iter()
        .map(|unit| visible_length(&unit_state_label(unit, opts.no_color)))
        .max()
        .unwrap_or(5)  // Minimum width of "STATE" header
        .max(5);

    // Calculate maximum width for LAST_EXIT column
    let max_last_exit_len = units
        .iter()
        .map(|unit| {
            let last_exit = format_last_exit(unit.last_exit.as_ref(), unit.cron.as_ref());
            visible_length(&last_exit)
        })
        .max()
        .unwrap_or(9)  // Minimum width of "LAST_EXIT" header
        .max(9);

    // Calculate maximum width for USER column
    let max_user_len = units
        .iter()
        .map(|unit| {
            unit.process.as_ref()
                .and_then(|p| p.user.as_ref())
                .map(|u| visible_length(u))
                .unwrap_or(1)
        })
        .max()
        .unwrap_or(4)  // Minimum width of "USER" header
        .max(4);

    let max_cmd_len = units
        .iter()
        .map(max_unit_command_width)
        .chain(
            units
                .iter()
                .map(|unit| max_spawn_command_width(&unit.spawned_children)),
        )
        .max()
        .unwrap_or(3)
        .max(3);
    let command_width = if opts.full_cmd {
        max_cmd_len
    } else {
        max_cmd_len.min(48)
    };

    // Keep UNIT width bounded so deeply nested trees stay aligned with the table.
    let mut max_unit_name_len = units
        .iter()
        .map(|unit| visible_length(&unit.name))
        .max()
        .unwrap_or(4)
        .max(4);
    let spawn_tree_width = units
        .iter()
        .map(|unit| max_spawn_label_width(&unit.spawned_children))
        .max()
        .unwrap_or(0);
    max_unit_name_len = max_unit_name_len.max(spawn_tree_width);
    let unit_width_cap = command_width.max(4);
    max_unit_name_len = max_unit_name_len.min(unit_width_cap);

    // Create dynamic columns with adjusted widths
    let columns_array = [
        Column {
            title: "UNIT",
            width: max_unit_name_len,
            align: Alignment::Left,
        },
        Column {
            title: "KIND",
            width: 6,
            align: Alignment::Left,
        },
        Column {
            title: "STATE",
            width: max_state_len,
            align: Alignment::Left,
        },
        Column {
            title: "USER",
            width: max_user_len,
            align: Alignment::Left,
        },
        Column {
            title: "PID",
            width: 8,
            align: Alignment::Right,
        },
        Column {
            title: "CPU",
            width: 10,
            align: Alignment::Right,
        },
        Column {
            title: "RSS",
            width: 10,
            align: Alignment::Right,
        },
        Column {
            title: "UPTIME",
            width: 18,
            align: Alignment::Left,
        },
        Column {
            title: "CMD",
            width: command_width,
            align: Alignment::Left,
        },
        Column {
            title: "LAST_EXIT",
            width: max_last_exit_len,
            align: Alignment::Left,
        },
        Column {
            title: "HEALTH",
            width: 10,
            align: Alignment::Left,
        },
    ];

    let columns = &columns_array;
    println!("{}", make_top_border(columns));
    println!(
        "{}",
        format_banner(
            &format!(
                "Status captured at {} (schema {})",
                timestamp, snapshot.schema_version
            ),
            columns,
        )
    );
    println!("{}", format_banner("", columns));
    println!(
        "{}",
        format_banner(
            &format!(
                "Overall health: {}",
                colorize(
                    overall_health_label(health),
                    overall_health_color(health),
                    opts.no_color
                )
            ),
            columns,
        )
    );

    let (state_counts, health_counts) = count_states_and_health(&units);
    println!("{}", format_banner("", columns));
    println!(
        "{}",
        format_breakdown_banner(&state_counts, &health_counts, columns, opts.no_color)
    );
    println!("{}", format_banner("", columns));

    println!("{}", make_separator_border(columns));
    println!("{}", format_header_row(columns));
    println!("{}", make_separator_border(columns));

    for unit in &units {
        println!("{}", format_unit_row(unit, columns, opts.no_color));
        // Print spawned children in tree format
        if !unit.spawned_children.is_empty() {
            render_spawn_rows(&unit.spawned_children, columns, opts.no_color);
        }
    }

    println!("{}", make_separator_border(columns));
    println!("{}", make_bottom_border(columns));

    let _ = io::stdout().flush();
    Ok(health)
}

fn colorize(text: &str, color: &str, no_color: bool) -> String {
    if no_color {
        text.to_string()
    } else {
        format!("{}{}{}", color, text, RESET)
    }
}

fn overall_health_label(health: OverallHealth) -> &'static str {
    match health {
        OverallHealth::Healthy => "Healthy",
        OverallHealth::Degraded => "Degraded",
        OverallHealth::Failing => "Failing",
    }
}

fn overall_health_color(health: OverallHealth) -> &'static str {
    match health {
        OverallHealth::Healthy => GREEN_BOLD,
        OverallHealth::Degraded => ORANGE,
        OverallHealth::Failing => RED_BOLD,
    }
}

fn unit_health_label(health: UnitHealth) -> &'static str {
    match health {
        UnitHealth::Healthy => "Healthy",
        UnitHealth::Degraded => "Degraded",
        UnitHealth::Failing => "Failing",
        UnitHealth::Inactive => "Inactive",
    }
}

fn health_label_extended(unit: &UnitStatus) -> String {
    // Special handling for crons with tracking issues or minor errors
    if let Some(cron) = &unit.cron
        && let Some(last) = &cron.last_run
        && let Some(status) = &last.status
    {
        match status {
            CronExecutionStatus::Failed(reason)
                if reason.starts_with("Failed to get PID") =>
            {
                return "Healthy-".to_string(); // Healthy but couldn't track properly
            }
            CronExecutionStatus::Success => {
                // Check if it had a non-zero exit code that we're treating as success
                if let Some(code) = last.exit_code
                    && code == 0
                {
                    return "Healthy+".to_string(); // Perfect health
                }
            }
            _ => {}
        }
    }

    // Default to the standard label
    unit_health_label(unit.health).to_string()
}

fn unit_health_color(health: UnitHealth) -> &'static str {
    match health {
        UnitHealth::Healthy => GREEN_BOLD,
        UnitHealth::Degraded => ORANGE,
        UnitHealth::Failing => RED_BOLD,
        UnitHealth::Inactive => GRAY,
    }
}

fn unit_state_label(unit: &UnitStatus, no_color: bool) -> String {
    if let Some(process) = &unit.process {
        return match process.state {
            ProcessState::Running => colorize("Running", BRIGHT_GREEN, no_color),
            ProcessState::Zombie => colorize("Zombie", RED, no_color),
            ProcessState::Missing => colorize("Missing", RED_BOLD, no_color),
        };
    }

    if let Some(lifecycle) = unit.lifecycle {
        return match lifecycle {
            ServiceLifecycleStatus::Running => {
                colorize("Running", BRIGHT_GREEN, no_color)
            }
            ServiceLifecycleStatus::ExitedSuccessfully => colorize("Ok", GREEN, no_color),
            ServiceLifecycleStatus::ExitedWithError => colorize("NotOk", RED, no_color),
            ServiceLifecycleStatus::Stopped => colorize("Stopped", GRAY, no_color),
            ServiceLifecycleStatus::Skipped => colorize("Skipped", GRAY, no_color),
        };
    }

    if let Some(cron) = &unit.cron {
        if let Some(last) = cron.last_run.as_ref() {
            if let Some(status) = &last.status {
                return match status {
                    CronExecutionStatus::Success => {
                        // Check exit code to determine if it was a full or partial success
                        match last.exit_code {
                            Some(0) => colorize("Idle", GRAY, no_color),
                            Some(_) => colorize("OkWithErr", DARK_GREEN, no_color),
                            None => colorize("Idle", GRAY, no_color),
                        }
                    }
                    CronExecutionStatus::Failed(reason) => {
                        // Special case: "Failed to get PID" is a tracking error, not a real failure
                        // The job likely ran but systemg couldn't track it properly
                        if reason.contains("Failed to get PID") {
                            colorize("Idle", GRAY, no_color)
                        } else if let Some(exit_code) = last.exit_code {
                            // Job completed with an exit code
                            if exit_code == 0 {
                                // Marked as failed but exited successfully - treat as partial success
                                colorize("PartialSuccess", DARK_GREEN, no_color)
                            } else {
                                // Real failure with non-zero exit
                                colorize("Failed", RED, no_color)
                            }
                        } else {
                            // Failed without completing
                            colorize("Failed", RED, no_color)
                        }
                    }
                    CronExecutionStatus::OverlapError => {
                        colorize("Overlap", YELLOW_BOLD, no_color)
                    }
                };
            }

            return colorize("Running", BRIGHT_GREEN, no_color);
        }

        return colorize("Scheduled", YELLOW, no_color);
    }

    colorize("Not running", GRAY, no_color)
}

fn format_uptime_column(uptime: Option<&UptimeInfo>) -> String {
    if let Some(info) = uptime {
        info.human.clone()
    } else {
        "-".to_string()
    }
}

fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        let minutes = seconds / 60;
        let secs = seconds % 60;
        if secs > 0 {
            format!("{}m {}s", minutes, secs)
        } else {
            format!("{}m", minutes)
        }
    } else {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        if minutes > 0 {
            format!("{}h {}m", hours, minutes)
        } else {
            format!("{}h", hours)
        }
    }
}

fn format_relative_time(from: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(from);

    if duration.num_seconds() < 60 {
        "< 1m ago".to_string()
    } else if duration.num_minutes() < 60 {
        format!("{}m ago", duration.num_minutes())
    } else if duration.num_hours() < 24 {
        format!("{}h ago", duration.num_hours())
    } else if duration.num_days() < 7 {
        format!("{}d ago", duration.num_days())
    } else {
        format!("{}w ago", duration.num_weeks())
    }
}

fn format_last_exit(
    exit: Option<&ExitMetadata>,
    cron: Option<&CronUnitStatus>,
) -> String {
    if let Some(cron) = cron
        && let Some(last) = &cron.last_run
    {
        let time_str = if let Some(completed_at) = last.completed_at {
            format!(" {}", format_relative_time(completed_at))
        } else if last.status.is_none() {
            // Still running, no completion time
            "".to_string()
        } else {
            // Has status but no completion time, use start time
            format!(" {}", format_relative_time(last.started_at))
        };

        return match &last.status {
            Some(CronExecutionStatus::Success) => {
                if let Some(code) = last.exit_code {
                    if time_str.is_empty() {
                        format!("exit {}", code)
                    } else {
                        format!("exit {},{}", code, time_str)
                    }
                } else {
                    format!("cron ok{}", time_str)
                }
            }
            Some(CronExecutionStatus::Failed(reason)) => {
                if let Some(code) = last.exit_code {
                    if time_str.is_empty() {
                        format!("exit {}", code)
                    } else {
                        format!("exit {},{}", code, time_str)
                    }
                } else if reason.is_empty() {
                    format!("cron failed{}", time_str)
                } else {
                    // Truncate reason if it's too long but keep full text recognizable
                    let display_reason = if reason.len() > 24 {
                        &reason[..24]
                    } else {
                        reason.as_str()
                    };
                    if time_str.is_empty() {
                        format!("failed: {}", display_reason)
                    } else {
                        format!("failed: {},{}", display_reason, time_str)
                    }
                }
            }
            Some(CronExecutionStatus::OverlapError) => format!("overlap{}", time_str),
            None => "running".to_string(),
        };
    }

    match exit {
        Some(metadata) => match (metadata.exit_code, metadata.signal) {
            (Some(code), _) => format!("exit {}", code),
            (None, Some(signal)) => format!("signal {}", signal),
            _ => "unknown".to_string(),
        },
        None => "-".to_string(),
    }
}

fn total_inner_width(columns: &[Column]) -> usize {
    let base: usize = columns.iter().map(|c| c.width + 2).sum();
    base + columns.len().saturating_sub(1)
}

fn make_top_border(columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    format!("╭{}╮", "─".repeat(inner_width))
}

fn make_bottom_border(columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    format!("╰{}╯", "─".repeat(inner_width))
}

fn make_separator_border(columns: &[Column]) -> String {
    let mut line = String::from("├");
    for (i, column) in columns.iter().enumerate() {
        line.push_str(&"─".repeat(column.width + 2));
        if i < columns.len() - 1 {
            line.push('┼');
        }
    }
    line.push('┤');
    line
}

fn format_banner(text: &str, columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    let content = ansi_pad(text, inner_width, Alignment::Center);
    format!("│{}│", content)
}

fn count_states_and_health(
    units: &[UnitStatus],
) -> (HashMap<String, usize>, HashMap<String, usize>) {
    let mut state_counts: HashMap<String, usize> = HashMap::new();
    let mut health_counts: HashMap<String, usize> = HashMap::new();

    for unit in units {
        let state_label = if let Some(process) = &unit.process {
            match process.state {
                ProcessState::Running => "Running",
                ProcessState::Zombie => "Zombie",
                ProcessState::Missing => "Missing",
            }
        } else if let Some(lifecycle) = unit.lifecycle {
            match lifecycle {
                ServiceLifecycleStatus::Running => "Running",
                ServiceLifecycleStatus::ExitedSuccessfully => "Ok",
                ServiceLifecycleStatus::ExitedWithError => "NotOk",
                ServiceLifecycleStatus::Stopped => "Stopped",
                ServiceLifecycleStatus::Skipped => "Skipped",
            }
        } else if unit.kind == UnitKind::Cron {
            "Idle"
        } else {
            "Unknown"
        };

        *state_counts.entry(state_label.to_string()).or_insert(0) += 1;

        let health_label = health_label_extended(unit);
        *health_counts.entry(health_label).or_insert(0) += 1;
    }

    (state_counts, health_counts)
}

fn format_breakdown_banner(
    state_counts: &HashMap<String, usize>,
    health_counts: &HashMap<String, usize>,
    columns: &[Column],
    no_color: bool,
) -> String {
    let mut states: Vec<_> = state_counts.iter().collect();
    states.sort_by_key(|(k, _)| k.as_str());
    let state_str = states
        .iter()
        .map(|(state, count)| {
            let color = match state.as_str() {
                "Running" => BRIGHT_GREEN,
                "Ok" => GREEN,
                "NotOk" => RED,
                "Zombie" | "Missing" => RED_BOLD,
                "Stopped" | "Skipped" | "Idle" => GRAY,
                _ => "",
            };
            format!("• {}: {}", colorize(state, color, no_color), count)
        })
        .collect::<Vec<_>>()
        .join("  ");

    let mut healths: Vec<_> = health_counts.iter().collect();
    healths.sort_by_key(|(k, _)| k.as_str());
    let health_str = healths
        .iter()
        .map(|(health, count)| {
            let color = if health.starts_with("Healthy") {
                if health.ends_with('+') {
                    GREEN_BOLD
                } else {
                    GREEN
                }
            } else if health.as_str() == "Degraded" {
                ORANGE
            } else if health.as_str() == "Failing" {
                RED_BOLD
            } else {
                GRAY
            };
            format!("• {}: {}", colorize(health, color, no_color), count)
        })
        .collect::<Vec<_>>()
        .join("  ");

    let breakdown = if !state_str.is_empty() && !health_str.is_empty() {
        format!("{}  {}  {}", state_str, "|", health_str)
    } else if !state_str.is_empty() {
        state_str
    } else {
        health_str
    };
    format_banner(&breakdown, columns)
}

fn format_header_row(columns: &[Column]) -> String {
    let mut row = String::from('│');
    for column in columns {
        row.push(' ');
        row.push_str(&ansi_pad(column.title, column.width, Alignment::Center));
        row.push(' ');
        row.push('│');
    }
    row
}

fn format_unit_row(unit: &UnitStatus, columns: &[Column], no_color: bool) -> String {
    let kind_label = match unit.kind {
        UnitKind::Service => "srvc",
        UnitKind::Cron => "cron",
        UnitKind::Orphaned => "orph",
    };

    let colored_kind_label = match unit.kind {
        UnitKind::Service => colorize(kind_label, CYAN, no_color),
        UnitKind::Cron => colorize(kind_label, YELLOW, no_color),
        UnitKind::Orphaned => kind_label.to_string(),
    };

    let state = unit_state_label(unit, no_color);
    let user = unit
        .process
        .as_ref()
        .and_then(|runtime| runtime.user.as_ref())
        .map(|u| u.to_string())
        .unwrap_or_else(|| "-".to_string());
    let pid = unit
        .process
        .as_ref()
        .map(|runtime| runtime.pid.to_string())
        .unwrap_or_else(|| "-".to_string());
    let cpu_col = format_cpu_column(unit.metrics.as_ref());
    let rss_col = format_rss_column(unit.metrics.as_ref());
    let uptime = format_uptime_column(unit.uptime.as_ref());
    let last_exit = format_last_exit(unit.last_exit.as_ref(), unit.cron.as_ref());
    let command = unit
        .command
        .as_ref()
        .or(unit.runtime_command.as_ref())
        .cloned()
        .unwrap_or_else(|| "-".to_string());
    let health_label_text = health_label_extended(unit);
    let health_color = if health_label_text == "Healthy-" {
        GREEN // Darker green for Healthy-
    } else {
        unit_health_color(unit.health)
    };
    let health_label = colorize(&health_label_text, health_color, no_color);

    let name_width = columns
        .first()
        .map(|col| col.width)
        .unwrap_or_else(|| unit.name.len());
    let display_name = truncate_unit_name(&unit.name, name_width);

    let values = [
        display_name,
        colored_kind_label,
        state,
        user,
        pid,
        cpu_col,
        rss_col,
        uptime,
        command,
        last_exit,
        health_label,
    ];

    format_row(&values, columns)
}

fn render_spawn_rows(nodes: &[SpawnedProcessNode], columns: &[Column], no_color: bool) {
    visit_spawn_tree(nodes, "", &mut |child, prefix, _| {
        println!(
            "{}",
            format_spawned_child_row(child, columns, no_color, prefix)
        );
    });
}

fn max_spawn_label_width(nodes: &[SpawnedProcessNode]) -> usize {
    let mut max_len = 0;
    visit_spawn_tree(nodes, "", &mut |child, prefix, _| {
        let candidate = format!("{}{}", prefix, child.name);
        let len = visible_length(&candidate);
        if len > max_len {
            max_len = len;
        }
    });
    max_len
}

fn max_spawn_command_width(nodes: &[SpawnedProcessNode]) -> usize {
    let mut max_len = 0;
    visit_spawn_tree(nodes, "", &mut |child, _, _| {
        let len = visible_length(&child.command);
        if len > max_len {
            max_len = len;
        }
    });
    max_len
}

fn max_unit_command_width(unit: &UnitStatus) -> usize {
    unit.command
        .as_ref()
        .or(unit.runtime_command.as_ref())
        .map(|cmd| visible_length(cmd))
        .unwrap_or(1)
}

fn count_spawn_nodes(nodes: &[SpawnedProcessNode]) -> usize {
    let mut total = 0;
    visit_spawn_tree(nodes, "", &mut |_, _, _| total += 1);
    total
}

fn visit_spawn_tree<F>(nodes: &[SpawnedProcessNode], prefix: &str, f: &mut F)
where
    F: FnMut(&SpawnedChild, &str, bool),
{
    for (idx, node) in nodes.iter().enumerate() {
        let is_last = idx == nodes.len() - 1;
        let connector = if is_last { "└─ " } else { "├─ " };
        let display_prefix = format!("{}{}", prefix, connector);
        f(&node.child, &display_prefix, is_last);
        let child_prefix = format!("{}{}", prefix, if is_last { "   " } else { "│  " });
        visit_spawn_tree(&node.children, &child_prefix, f);
    }
}

fn format_spawned_child_row(
    child: &SpawnedChild,
    columns: &[Column],
    no_color: bool,
    prefix: &str,
) -> String {
    let is_peripheral = matches!(child.kind, SpawnedChildKind::Peripheral);
    let name_width = columns.first().map(|col| col.width).unwrap_or(4);
    let child_name = truncate_nested_unit_label(prefix, &child.name, name_width);
    let user = child
        .user
        .as_ref()
        .map(|u| u.to_string())
        .unwrap_or_else(|| "-".to_string());
    let pid = child.pid.to_string();
    let cpu_col = child
        .cpu_percent
        .map(|cpu| format!("{cpu:.1}%"))
        .unwrap_or_else(|| "-".to_string());
    let rss_col = child
        .rss_bytes
        .map(format_bytes)
        .unwrap_or_else(|| "-".to_string());

    let (state, health_label) = if let Some(exit) = &child.last_exit {
        let succeeded = exit.exit_code.map(|code| code == 0).unwrap_or(false)
            && exit.signal.is_none();

        let state_label = if succeeded {
            colorize("Exited", YELLOW_BOLD, no_color)
        } else {
            colorize("Exited", RED_BOLD, no_color)
        };

        let health = if succeeded {
            colorize("Healthy", GREEN_BOLD, no_color)
        } else {
            colorize("Failing", RED_BOLD, no_color)
        };

        (state_label, health)
    } else {
        (
            colorize("Running", BRIGHT_GREEN, no_color),
            colorize("Healthy", GREEN_BOLD, no_color),
        )
    };

    let uptime = if let Some(exit) = &child.last_exit
        && let Some(finished_at) = exit.finished_at
    {
        let exit_elapsed = finished_at
            .elapsed()
            .unwrap_or_else(|_| std::time::Duration::from_secs(0));
        format_elapsed(exit_elapsed.as_secs())
    } else {
        let elapsed = child
            .started_at
            .elapsed()
            .unwrap_or_else(|_| std::time::Duration::from_secs(0));
        format_elapsed(elapsed.as_secs())
    };

    let last_exit = format_spawn_exit(child.last_exit.as_ref());
    let command = if child.command.is_empty() {
        "-".to_string()
    } else {
        child.command.clone()
    };

    let kind_label = match child.kind {
        SpawnedChildKind::Spawned => colorize("spwn", MAGENTA, no_color),
        SpawnedChildKind::Peripheral => colorize("peri", ORANGE, no_color),
    };

    let values = [
        style_peripheral_column(child_name, is_peripheral, no_color),
        kind_label,
        state,
        style_peripheral_column(user, is_peripheral, no_color),
        style_peripheral_column(pid, is_peripheral, no_color),
        style_peripheral_column(cpu_col, is_peripheral, no_color),
        style_peripheral_column(rss_col, is_peripheral, no_color),
        style_peripheral_column(uptime, is_peripheral, no_color),
        style_peripheral_column(command, is_peripheral, no_color),
        style_peripheral_column(last_exit, is_peripheral, no_color),
        health_label,
    ];

    format_row(&values, columns)
}

fn style_peripheral_column(value: String, is_peripheral: bool, no_color: bool) -> String {
    if is_peripheral {
        colorize(&value, GRAY, no_color)
    } else {
        value
    }
}

fn format_spawn_exit(exit: Option<&SpawnedExit>) -> String {
    match exit {
        Some(exit) => {
            let mut parts = Vec::new();
            if let Some(code) = exit.exit_code {
                parts.push(format!("code {code}"));
            }
            if let Some(signal) = exit.signal {
                parts.push(format!("signal {signal}"));
            }
            if let Some(timestamp) = exit.finished_at {
                let ts: DateTime<Utc> = DateTime::<Utc>::from(timestamp);
                parts.push(ts.format("%Y-%m-%d %H:%M:%S").to_string());
            }

            if parts.is_empty() {
                "-".to_string()
            } else {
                format!("exit {}", parts.join(", "))
            }
        }
        None => "-".to_string(),
    }
}

fn format_row(values: &[String; 11], columns: &[Column]) -> String {
    let mut row = String::from('│');
    for (value, column) in values.iter().zip(columns.iter()) {
        row.push(' ');
        row.push_str(&ansi_pad(value, column.width, column.align));
        row.push(' ');
        row.push('│');
    }
    row
}

fn ansi_pad(value: &str, width: usize, align: Alignment) -> String {
    let len = visible_length(value);
    if len > width {
        // Truncate with ellipsis while preserving wrapping ANSI color.
        return ellipsize_ansi_aware(value, width);
    }

    let pad = width - len;
    match align {
        Alignment::Left => format!("{}{}", value, " ".repeat(pad)),
        Alignment::Right => format!("{}{}", " ".repeat(pad), value),
        Alignment::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), value, " ".repeat(right))
        }
    }
}

fn ellipsize_ansi_aware(value: &str, width: usize) -> String {
    if !value.contains('\u{1b}') {
        return ellipsize(value, width);
    }

    let plain = strip_ansi(value);
    let truncated = ellipsize(&plain, width);

    let prefix_len = leading_ansi_len(value);
    let has_wrapping_reset = value.ends_with(RESET);
    if prefix_len == 0 {
        return truncated;
    }

    let mut out = String::new();
    out.push_str(&value[..prefix_len]);
    out.push_str(&truncated);
    if has_wrapping_reset {
        out.push_str(RESET);
    }
    out
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            for next in &mut chars {
                if next == 'm' {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn leading_ansi_len(value: &str) -> usize {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() && bytes[i] == 0x1b && bytes[i + 1] == b'[' {
        i += 2;
        while i < bytes.len() {
            let b = bytes[i];
            i += 1;
            if b == b'm' {
                break;
            }
        }
    }
    i
}

fn visible_length(text: &str) -> usize {
    let mut len = 0;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            for next in &mut chars {
                if next == 'm' {
                    break;
                }
            }
        } else {
            len += 1;
        }
    }
    len
}

fn ellipsize(value: &str, width: usize) -> String {
    if width <= 3 {
        return "...".chars().take(width).collect();
    }

    let mut result = String::new();
    let mut iter = value.chars();
    for _ in 0..(width - 3) {
        if let Some(ch) = iter.next() {
            result.push(ch);
        } else {
            return value.to_string();
        }
    }
    result.push_str("...");
    result
}

fn ellipsize_from_front(value: &str, width: usize) -> String {
    if width <= 3 {
        return "...".chars().take(width).collect();
    }

    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= width {
        return value.to_string();
    }

    let keep = width - 3;
    let suffix: String = chars[chars.len() - keep..].iter().collect();
    format!("...{}", suffix)
}

fn truncate_unit_name(name: &str, width: usize) -> String {
    if visible_length(name) <= width {
        return name.to_string();
    }
    if name.contains('/') {
        return ellipsize_from_front(name, width);
    }
    ellipsize(name, width)
}

fn truncate_nested_unit_label(prefix: &str, name: &str, width: usize) -> String {
    let prefix_len = visible_length(prefix);
    if prefix_len >= width {
        return ellipsize(prefix, width);
    }

    let name_budget = width - prefix_len;
    format!("{}{}", prefix, truncate_unit_name(name, name_budget))
}

fn format_cpu_column(metrics: Option<&UnitMetricsSummary>) -> String {
    metrics
        .map(|summary| format!("{:.1}%", summary.latest_cpu_percent))
        .unwrap_or_else(|| "-".to_string())
}

fn format_rss_column(metrics: Option<&UnitMetricsSummary>) -> String {
    metrics
        .map(|summary| format_bytes(summary.latest_rss_bytes))
        .unwrap_or_else(|| "-".to_string())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    if bytes < 1024 {
        return format!("{}B", bytes);
    }

    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= 1024.0 && idx < UNITS.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }

    format!("{:.1}{}B", value, UNITS[idx])
}

fn fetch_inspect(
    config_path: &str,
    unit: &str,
    samples: usize,
) -> Result<InspectPayload, Box<dyn Error>> {
    let limit = samples.min(u32::MAX as usize) as u32;
    match ipc::send_command(&ControlCommand::Inspect {
        unit: unit.to_string(),
        samples: limit,
    }) {
        Ok(ControlResponse::Inspect(payload)) => Ok(*payload),
        Ok(other) => Err(io::Error::other(format!(
            "unexpected supervisor response: {:?}",
            other
        ))
        .into()),
        Err(ControlError::NotAvailable) => {
            let config = load_config(Some(config_path))?;
            let snapshot = collect_disk_snapshot(Some(config))?;
            let unit_status = snapshot
                .units
                .into_iter()
                .find(|status| status.name == unit || status.hash == unit);
            Ok(InspectPayload {
                unit: unit_status,
                samples: Vec::new(),
            })
        }
        Err(err) => Err(err.into()),
    }
}

fn render_inspect(
    payload: &InspectPayload,
    opts: &InspectRenderOptions,
) -> Result<OverallHealth, Box<dyn Error>> {
    if payload.unit.is_none() {
        println!("No unit matching the requested identifier.");
        return Ok(OverallHealth::Failing);
    }

    let unit = payload.unit.as_ref().unwrap();
    let health = overall_health_from_unit(unit);

    // For cron units, get metrics from the last execution if available
    let filtered_samples = if unit.kind == UnitKind::Cron {
        // Try to get metrics from the last completed cron run
        if let Some(cron_status) = &unit.cron {
            if let Some(last_run) = cron_status.recent_runs.first() {
                // Use metrics from the last run if available
                if !last_run.metrics.is_empty() {
                    filter_samples(
                        &last_run.metrics,
                        Some(opts.window_seconds),
                        opts.samples_limit,
                    )
                } else {
                    // No metrics available from last run
                    vec![]
                }
            } else {
                // No runs yet
                vec![]
            }
        } else {
            vec![]
        }
    } else {
        // For regular services, use live samples
        filter_samples(
            &payload.samples,
            Some(opts.window_seconds),
            opts.samples_limit,
        )
    };

    if opts.json {
        let json_payload = InspectPayload {
            unit: Some(unit.clone()),
            samples: filtered_samples,
        };
        println!("{}", serde_json::to_string_pretty(&json_payload)?);
        return Ok(health);
    }

    println!("Inspecting unit: {}", unit.name);
    println!(
        "Kind: {}",
        match unit.kind {
            UnitKind::Service => "service",
            UnitKind::Cron => "cron",
            UnitKind::Orphaned => "orphaned",
        }
    );
    println!(
        "Health: {}",
        colorize(
            overall_health_label(health),
            overall_health_color(health),
            opts.no_color
        )
    );

    if let Some(process) = &unit.process {
        println!("PID: {}", process.pid);
    }

    if let Some(uptime) = unit.uptime.as_ref() {
        println!("Uptime: {} ({}s)", uptime.human, uptime.seconds);
    }

    println!(
        "Last exit: {}",
        format_last_exit(unit.last_exit.as_ref(), unit.cron.as_ref())
    );

    if let Some(command) = &unit.command {
        println!("Configured command: {}", command);
    }
    if let Some(runtime_command) = &unit.runtime_command {
        println!("Runtime command: {}", runtime_command);
    }

    if !unit.spawned_children.is_empty() {
        println!();
        let total_children = count_spawn_nodes(&unit.spawned_children);
        println!("Spawned Processes ({} total):", total_children);
        println!("{:-<60}", "");
        visit_spawn_tree(&unit.spawned_children, "", &mut |child, prefix, _| {
            let uptime = child
                .started_at
                .elapsed()
                .map(|d| format_duration(d.as_secs()))
                .unwrap_or_else(|_| "0s".to_string());
            let depth_info = if child.depth > 0 {
                format!(", depth: {}", child.depth)
            } else {
                String::new()
            };
            println!(
                "  {}{} [PID: {}, Uptime: {}{}]",
                prefix, child.name, child.pid, uptime, depth_info
            );
        });
        println!("{:-<60}", "");
    }

    if let Some(metrics) = unit.metrics.as_ref() {
        println!(
            "Metrics: latest {:.4}% CPU, avg {:.4}% CPU, max {:.4}% CPU, RSS {} across {} samples",
            metrics.latest_cpu_percent,
            metrics.average_cpu_percent,
            metrics.max_cpu_percent,
            format_bytes(metrics.latest_rss_bytes),
            metrics.samples,
        );
    } else if unit.kind == UnitKind::Cron {
        println!("Metrics: awaiting next cron execution");
    } else if unit.process.is_some() {
        println!("Metrics: collector initializing (may take a few seconds)");
    } else {
        println!("Metrics: not available (service not running)");
    }

    // Use gnuplot visualization for metrics
    if !filtered_samples.is_empty() {
        // For cron jobs, indicate we're showing last run's metrics
        if unit.kind == UnitKind::Cron
            && let Some(cron_status) = &unit.cron
            && let Some(last_run) = cron_status.recent_runs.first()
        {
            let run_time = last_run
                .started_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
            println!();
            println!("Resource Usage from Last Run ({}):", run_time);
        }

        // Use gnuplot for charting
        let chart_config = ChartConfig {
            no_color: opts.no_color,
            window_desc: opts.window_desc.clone(),
        };

        if let Err(e) = charting::render_metrics_chart(&filtered_samples, &chart_config) {
            warn!("Failed to render chart: {}", e);
            // Fallback is handled within render_metrics_chart
        }
    } else if !opts.json {
        println!();
        println!("No metrics available for the specified window.");
    }

    // Show cron history for cron units
    if unit.kind == UnitKind::Cron
        && let Some(cron_status) = &unit.cron
        && !cron_status.recent_runs.is_empty()
    {
        println!();
        println!("Recent Cron Runs (last 10):");
        println!("{:-<24} {:>10} {:>12} {:>10}", "", "", "", "");
        println!(
            "{:<24} {:>10} {:>12} {:>10}",
            "STARTED", "STATUS", "DURATION", "EXIT CODE"
        );
        println!("{:-<24} {:-<10} {:-<12} {:-<10}", "", "", "", "");

        let runs_to_show = cron_status.recent_runs.iter().take(10);
        for run in runs_to_show {
            let started = run
                .started_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();

            let status = match &run.status {
                Some(CronExecutionStatus::Success) => {
                    colorize("Success", GREEN, opts.no_color)
                }
                Some(CronExecutionStatus::Failed(_)) => {
                    colorize("Failed", RED, opts.no_color)
                }
                Some(CronExecutionStatus::OverlapError) => {
                    colorize("Overlap", YELLOW_BOLD, opts.no_color)
                }
                None => colorize("Running", BRIGHT_GREEN, opts.no_color),
            };

            let duration = if let Some(completed) = run.completed_at {
                let dur = completed.signed_duration_since(run.started_at);
                if dur.num_seconds() < 60 {
                    format!("{}s", dur.num_seconds())
                } else if dur.num_minutes() < 60 {
                    format!("{}m", dur.num_minutes())
                } else {
                    format!("{}h", dur.num_hours())
                }
            } else {
                "-".to_string()
            };

            let exit_code = run
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string());

            println!(
                "{:<24} {:>10} {:>12} {:>10}",
                started, status, duration, exit_code
            );
        }
    }

    Ok(health)
}

fn filter_samples(
    samples: &[MetricSample],
    since: Option<u64>,
    limit: usize,
) -> Vec<MetricSample> {
    let mut filtered: Vec<MetricSample> = if let Some(seconds) = since {
        let cutoff = Utc::now()
            .checked_sub_signed(ChronoDuration::seconds(
                seconds.min(i64::MAX as u64) as i64
            ))
            .unwrap_or(DateTime::<Utc>::MIN_UTC);
        samples
            .iter()
            .filter(|sample| sample.timestamp >= cutoff)
            .cloned()
            .collect()
    } else {
        samples.to_vec()
    };

    if filtered.len() > limit {
        filtered = filtered
            .into_iter()
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
    }

    filtered
}

fn overall_health_from_unit(unit: &UnitStatus) -> OverallHealth {
    match unit.health {
        UnitHealth::Healthy => OverallHealth::Healthy,
        UnitHealth::Degraded => OverallHealth::Degraded,
        UnitHealth::Failing => OverallHealth::Failing,
        UnitHealth::Inactive => OverallHealth::Healthy, // Inactive units don't affect overall health
    }
}

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

fn gather_supervisor_pids() -> HashSet<libc::pid_t> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);

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
