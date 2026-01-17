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
use libc::{SIGKILL, SIGTERM, getpgrp, killpg};
use nix::{
    sys::signal,
    unistd::{Pid, Uid},
};
use sysinfo::{ProcessesToUpdate, System};
use systemg::{
    charting::{self, ChartConfig, is_live_window, parse_window_duration},
    cli::{Cli, Commands, parse_args},
    config::load_config,
    cron::{CronExecutionStatus, CronStateFile},
    daemon::{Daemon, PidFile, ServiceLifecycleStatus},
    ipc::{self, ControlCommand, ControlError, ControlResponse, InspectPayload},
    logs::{LogManager, resolve_log_path},
    metrics::MetricSample,
    runtime::{self, RuntimeMode},
    status::{
        CronUnitStatus, ExitMetadata, OverallHealth, ProcessState, StatusSnapshot,
        UnitHealth, UnitKind, UnitMetricsSummary, UnitStatus, UptimeInfo,
        collect_disk_snapshot, compute_overall_health,
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
    init_logging(&args);

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
                include_orphans: all,
                service_filter: service.as_deref(),
            };

            if let Some(interval) = watch {
                let sleep_interval = Duration::from_secs(interval.max(1));
                loop {
                    let snapshot = fetch_status_snapshot(&effective_config)?;
                    render_status(&snapshot, &render_opts, true)?;
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

            let is_live = is_live_window(window_seconds) && !json;

            // Calculate samples limit based on window
            let samples_limit = if window_seconds < 3600 {
                window_seconds as usize // For short windows, 1 sample per second
            } else {
                720 // For longer windows, cap at 720 samples
            };

            if is_live {
                // Live mode with auto-refresh
                use std::sync::{
                    Arc,
                    atomic::{AtomicBool, Ordering},
                };

                let running = Arc::new(AtomicBool::new(true));
                let r = running.clone();

                // Set up Ctrl+C handler
                ctrlc::set_handler(move || {
                    r.store(false, Ordering::SeqCst);
                    print!("\x1B[999B\nStopping live view...\n");
                })?;

                let mut last_health = OverallHealth::Healthy;
                let mut first_iteration = true;

                while running.load(Ordering::SeqCst) {
                    // Clear terminal and move cursor to top-left
                    if first_iteration {
                        // Full clear on first iteration
                        print!("\x1B[2J\x1B[H");
                        first_iteration = false;
                    } else {
                        // Just move cursor to home for updates
                        print!("\x1B[H");
                    }
                    io::stdout().flush()?;

                    // Fetch fresh data
                    let payload = fetch_inspect(&effective_config, &unit, samples_limit)?;
                    if payload.unit.is_none() {
                        eprintln!("Unit '{unit}' not found.");
                        process::exit(2);
                    }

                    let render_opts = InspectRenderOptions {
                        json: false,
                        no_color,
                        window_seconds,
                        window_desc: window.clone(),
                        samples_limit,
                        is_live: true,
                    };

                    last_health = render_inspect(&payload, &render_opts)?;

                    // Show live mode indicator
                    println!();
                    println!("Live view ({}) - Press Ctrl+C to stop", window);

                    // Sleep for 1 second before next update
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }

                let exit_code = match last_health {
                    OverallHealth::Healthy => 0,
                    OverallHealth::Degraded => 1,
                    OverallHealth::Failing => 2,
                };
                process::exit(exit_code);
            } else {
                // Historical mode - one-shot
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
                    is_live: false,
                };

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
                    manager.show_logs(lines, kind.as_deref(), Some(&effective_config))?;
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

struct StatusRenderOptions<'a> {
    json: bool,
    no_color: bool,
    include_orphans: bool,
    service_filter: Option<&'a str>,
}

struct InspectRenderOptions {
    json: bool,
    no_color: bool,
    window_seconds: u64,
    window_desc: String,
    samples_limit: usize,
    is_live: bool,
}

const GREEN_BOLD: &str = "\x1b[1;32m";
const GREEN: &str = "\x1b[32m";
const DARK_GREEN: &str = "\x1b[38;5;22m"; // Darker green for partial success
const BRIGHT_GREEN: &str = "\x1b[92m";
const YELLOW_BOLD: &str = "\x1b[1;33m";
const RED_BOLD: &str = "\x1b[1;31m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const CYAN_BOLD: &str = "\x1b[1;36m";
const YELLOW: &str = "\x1b[33m";
const ORANGE: &str = "\x1b[38;5;208m";
const GRAY: &str = "\x1b[90m";
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
    }

    let timestamp = snapshot
        .captured_at
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S %Z");

    // Calculate the maximum widths for each column based on actual data
    let max_unit_name_len = units
        .iter()
        .map(|unit| visible_length(&unit.name))
        .max()
        .unwrap_or(4)  // Minimum width of "UNIT" header
        .max(4); // Ensure at least as wide as "UNIT" header

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
    let full_header_border = make_full_border(columns, '=');
    println!("{}", full_header_border);
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
    println!(
        "{}",
        format_banner(
            &format!(
                "Overall health {}",
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
    println!(
        "{}",
        format_breakdown_banner(&state_counts, &health_counts, columns, opts.no_color)
    );

    println!("{}", make_border(columns, '='));
    println!("{}", format_header_row(columns));
    println!("{}", make_border(columns, '-'));

    for unit in &units {
        println!("{}", format_unit_row(unit, columns, opts.no_color));
    }

    println!("{}", make_border(columns, '='));
    println!("{}", full_header_border);

    io::stdout().flush()?;
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
        OverallHealth::Healthy => "healthy",
        OverallHealth::Degraded => "degraded",
        OverallHealth::Failing => "failing",
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
        UnitHealth::Healthy => "healthy",
        UnitHealth::Degraded => "degraded",
        UnitHealth::Failing => "failing",
        UnitHealth::Inactive => "inactive",
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
                return "healthy-".to_string(); // Healthy but couldn't track properly
            }
            CronExecutionStatus::Success => {
                // Check if it had a non-zero exit code that we're treating as success
                if let Some(code) = last.exit_code
                    && code == 0
                {
                    return "healthy+".to_string(); // Perfect health
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

fn make_full_border(columns: &[Column], fill_char: char) -> String {
    let inner_width = total_inner_width(columns);
    format!("+{}+", fill_char.to_string().repeat(inner_width))
}

fn make_border(columns: &[Column], fill_char: char) -> String {
    let mut line = String::from("+");
    for column in columns {
        line.push_str(&fill_char.to_string().repeat(column.width + 2));
        line.push('+');
    }
    line
}

fn format_banner(text: &str, columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    let content = ansi_pad(text, inner_width, Alignment::Center);
    format!("|{}|", content)
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
            format!("{}: {}", colorize(state, color, no_color), count)
        })
        .collect::<Vec<_>>()
        .join(", ");

    let mut healths: Vec<_> = health_counts.iter().collect();
    healths.sort_by_key(|(k, _)| k.as_str());
    let health_str = healths
        .iter()
        .map(|(health, count)| {
            let color = if health.starts_with("healthy") {
                if health.ends_with('+') {
                    GREEN_BOLD
                } else {
                    GREEN
                }
            } else if health.as_str() == "degraded" {
                ORANGE
            } else if health.as_str() == "failing" {
                RED_BOLD
            } else {
                GRAY
            };
            format!("{}: {}", colorize(health, color, no_color), count)
        })
        .collect::<Vec<_>>()
        .join(", ");

    let breakdown = format!(
        "{}[States]{} {} | {}[Health]{} {}",
        CYAN_BOLD, RESET, state_str, CYAN_BOLD, RESET, health_str
    );
    format_banner(&breakdown, columns)
}

fn format_header_row(columns: &[Column]) -> String {
    let mut row = String::from("|");
    for column in columns {
        row.push(' ');
        row.push_str(&ansi_pad(column.title, column.width, Alignment::Center));
        row.push(' ');
        row.push('|');
    }
    row
}

fn format_unit_row(unit: &UnitStatus, columns: &[Column], no_color: bool) -> String {
    let kind_label = match unit.kind {
        UnitKind::Service => "svc",
        UnitKind::Cron => "cron",
        UnitKind::Orphaned => "orph",
    };

    let colored_kind_label = match unit.kind {
        UnitKind::Service => colorize(kind_label, CYAN, no_color),
        UnitKind::Cron => colorize(kind_label, YELLOW, no_color),
        UnitKind::Orphaned => kind_label.to_string(),
    };

    let state = unit_state_label(unit, no_color);
    let pid = unit
        .process
        .as_ref()
        .map(|runtime| runtime.pid.to_string())
        .unwrap_or_else(|| "-".to_string());
    let cpu_col = format_cpu_column(unit.metrics.as_ref());
    let rss_col = format_rss_column(unit.metrics.as_ref());
    let uptime = format_uptime_column(unit.uptime.as_ref());
    let last_exit = format_last_exit(unit.last_exit.as_ref(), unit.cron.as_ref());
    let health_label_text = health_label_extended(unit);
    let health_color = if health_label_text == "healthy-" {
        GREEN // Darker green for healthy-
    } else {
        unit_health_color(unit.health)
    };
    let health_label = colorize(&health_label_text, health_color, no_color);

    let name_width = columns
        .first()
        .map(|col| col.width)
        .unwrap_or_else(|| unit.name.len());
    let display_name = if visible_length(&unit.name) > name_width {
        ellipsize(&unit.name, name_width)
    } else {
        unit.name.clone()
    };

    let values = [
        display_name,
        colored_kind_label,
        state,
        pid,
        cpu_col,
        rss_col,
        uptime,
        last_exit,
        health_label,
    ];

    format_row(&values, columns)
}

fn format_row(values: &[String; 9], columns: &[Column]) -> String {
    let mut row = String::from("|");
    for (value, column) in values.iter().zip(columns.iter()) {
        row.push(' ');
        row.push_str(&ansi_pad(value, column.width, column.align));
        row.push(' ');
        row.push('|');
    }
    row
}

fn ansi_pad(value: &str, width: usize, align: Alignment) -> String {
    let len = visible_length(value);
    if len > width {
        // Truncate with ellipsis if content exceeds column width
        return ellipsize(value, width);
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
        println!("Command: {}", command);
    }

    if let Some(metrics) = unit.metrics.as_ref() {
        println!(
            "Metrics: latest {:.1}% CPU, avg {:.1}% CPU, max {:.1}% CPU, RSS {} across {} samples",
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
            is_live: opts.is_live,
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

fn init_logging(args: &Cli) {
    let filter = if let Some(level) = args.log_level {
        EnvFilter::new(level.as_str())
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };

    // Ensure the log directory exists
    let log_dir = runtime::log_dir();
    if let Err(err) = fs::create_dir_all(&log_dir) {
        eprintln!("Failed to create log directory {:?}: {}", log_dir, err);
    }
    let log_path = log_dir.join("supervisor.log");

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
