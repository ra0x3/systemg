use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use libc::{SIGKILL, SIGTERM, getpgrp, killpg};
use nix::{
    sys::signal,
    unistd::{Pid, Uid},
};
use std::{
    collections::HashSet,
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
use sysinfo::{ProcessesToUpdate, System};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use systemg::{
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
        collect_snapshot_from_disk, compute_overall_health,
    },
    supervisor::Supervisor,
};

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
            since,
            samples,
            table,
        } => {
            let mut effective_config = config.clone();
            if load_config(Some(&config)).is_err()
                && let Ok(Some(hint)) = ipc::read_config_hint()
            {
                effective_config = hint.to_string_lossy().to_string();
            }

            let payload = fetch_inspect(&effective_config, &unit, samples)?;
            if payload.unit.is_none() {
                eprintln!("Unit '{unit}' not found.");
                process::exit(2);
            }

            let render_opts = InspectRenderOptions {
                json,
                no_color,
                since,
                samples_limit: samples,
                table,
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
    since: Option<u64>,
    samples_limit: usize,
    table: bool,
}

const GREEN_BOLD: &str = "\x1b[1;32m";
const GREEN: &str = "\x1b[32m";
const BRIGHT_GREEN: &str = "\x1b[92m";
const YELLOW_BOLD: &str = "\x1b[1;33m";
const RED_BOLD: &str = "\x1b[1;31m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
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

const TABLE_COLUMNS: [Column; 9] = [
    Column {
        title: "UNIT",
        width: 24,
        align: Alignment::Left,
    },
    Column {
        title: "KIND",
        width: 6,
        align: Alignment::Left,
    },
    Column {
        title: "STATE",
        width: 12,
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
        width: 20,
        align: Alignment::Left,
    },
    Column {
        title: "LAST EXIT",
        width: 18,
        align: Alignment::Left,
    },
    Column {
        title: "HEALTH",
        width: 8,
        align: Alignment::Center,
    },
];

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
            collect_snapshot_from_disk(config)
                .map_err(|err| Box::new(err) as Box<dyn Error>)
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

    let columns = &TABLE_COLUMNS;
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
    }
}

fn unit_health_color(health: UnitHealth) -> &'static str {
    match health {
        UnitHealth::Healthy => GREEN_BOLD,
        UnitHealth::Degraded => ORANGE,
        UnitHealth::Failing => RED_BOLD,
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
            ServiceLifecycleStatus::ExitedSuccessfully => {
                colorize("Succeeded", GREEN, no_color)
            }
            ServiceLifecycleStatus::ExitedWithError => colorize("Failed", RED, no_color),
            ServiceLifecycleStatus::Stopped => colorize("Stopped", GRAY, no_color),
            ServiceLifecycleStatus::Skipped => colorize("Skipped", GRAY, no_color),
        };
    }

    if let Some(cron) = &unit.cron {
        if let Some(last) = cron.last_run.as_ref() {
            if let Some(status) = &last.status {
                return match status {
                    CronExecutionStatus::Success => colorize("Idle", GRAY, no_color),
                    CronExecutionStatus::Failed(_) => colorize("Failed", RED, no_color),
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
        format!("{} ({}s)", info.human, info.seconds)
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
                    format!("exit {}{}", code, time_str)
                } else {
                    format!("cron ok{}", time_str)
                }
            }
            Some(CronExecutionStatus::Failed(reason)) => {
                if let Some(code) = last.exit_code {
                    format!("exit {}{}", code, time_str)
                } else if reason.is_empty() {
                    format!("cron failed{}", time_str)
                } else {
                    // Truncate reason if it's too long
                    let truncated_reason = if reason.len() > 20 {
                        format!("{}...", &reason[..17])
                    } else {
                        reason.clone()
                    };
                    format!("failed: {}{}", truncated_reason, time_str)
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
    let health_label = colorize(
        unit_health_label(unit.health),
        unit_health_color(unit.health),
        no_color,
    );

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
    if len >= width {
        return value.to_string();
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
            let snapshot = collect_snapshot_from_disk(Some(config))?;
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
                    filter_samples(&last_run.metrics, opts.since, opts.samples_limit)
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
        filter_samples(&payload.samples, opts.since, opts.samples_limit)
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

    if let Some(metrics) = unit.metrics.as_ref() {
        println!(
            "Metrics: latest {:.1}% CPU, avg {:.1}% CPU, max {:.1}% CPU, RSS {} across {} samples",
            metrics.latest_cpu_percent,
            metrics.average_cpu_percent,
            metrics.max_cpu_percent,
            format_bytes(metrics.latest_rss_bytes),
            metrics.samples,
        );
    } else {
        println!("Metrics: not available (collector has not observed samples yet)");
    }

    // Use chart visualization by default, table if requested
    if !filtered_samples.is_empty() {
        // For cron jobs, indicate we're showing last run's metrics
        if unit.kind == UnitKind::Cron {
            if let Some(cron_status) = &unit.cron
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
        } else {
            println!();
            println!("Resource Usage Over Time");
        }

        if opts.table {
            // Legacy table view
            println!();
            println!("{:<24} {:>8} {:>10}", "TIMESTAMP", "CPU", "RSS");
            println!("{:-<24} {:-<8} {:-<10}", "", "", "");
            for sample in filtered_samples {
                println!(
                    "{:<24} {:>7.1}% {:>10}",
                    format_timestamp(sample.timestamp),
                    sample.cpu_percent,
                    format_bytes(sample.rss_bytes),
                );
            }
        } else {
            // Default chart visualization
            render_metrics_chart(&filtered_samples, opts.no_color);
        }
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

fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// Renders a combined ASCII chart for CPU and RSS metrics over time
fn render_metrics_chart(samples: &[MetricSample], no_color: bool) {
    if samples.is_empty() {
        return;
    }

    // Chart dimensions - fixed width for consistency
    let chart_height = 20;
    let chart_width = 80; // Fixed width for neat formatting

    // Find max values for scaling
    let max_cpu = samples
        .iter()
        .map(|s| s.cpu_percent as f64)
        .fold(0.0, f64::max)
        .max(10.0); // Minimum 10% scale for visibility

    let max_rss_gb = samples
        .iter()
        .map(|s| s.rss_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        .fold(0.0, f64::max)
        .max(0.1); // Minimum 0.1GB scale
    // Downsample if we have more samples than width
    let step = if samples.len() > chart_width {
        samples.len() as f64 / chart_width as f64
    } else {
        1.0
    };

    println!();
    println!("Resource Usage Over Time");
    println!();

    // Draw chart with dual y-axes
    for row in 0..chart_height {
        // Left Y-axis (CPU %)
        if row == 0 {
            print!("{:>6.1}% ┤", max_cpu);
        } else if row == chart_height - 1 {
            print!("{:>6.1}% ┤", 0.0);
        } else if row == chart_height / 2 {
            print!("{:>6.1}% ┤", max_cpu / 2.0);
        } else {
            print!("{:>8}┤", "");
        }

        // Draw the chart area
        for col in 0..chart_width {
            let sample_idx = ((col as f64) * step) as usize;
            if sample_idx >= samples.len() {
                print!(" ");
                continue;
            }

            let cpu_val = samples[sample_idx].cpu_percent as f64;
            let rss_val =
                samples[sample_idx].rss_bytes as f64 / (1024.0 * 1024.0 * 1024.0);

            let cpu_height = ((cpu_val / max_cpu) * chart_height as f64) as usize;
            let rss_height = ((rss_val / max_rss_gb) * chart_height as f64) as usize;

            let current_height = chart_height - row - 1;

            // Legend box in top-right
            if row <= 3 && col >= chart_width - 20 && col < chart_width - 1 {
                let legend_col = col - (chart_width - 20);
                if row == 0 && legend_col < 19 {
                    let legend = "┌─────────────────┐";
                    print!("{}", legend.chars().nth(legend_col).unwrap_or(' '));
                    continue;
                } else if row == 1 && legend_col < 19 {
                    if legend_col == 0 {
                        print!("│");
                    } else if legend_col == 1 {
                        print!(" ");
                    } else if legend_col == 2 {
                        print!("{}", if no_color { "" } else { GREEN });
                    } else if legend_col == 3 {
                        print!("*");
                    } else if legend_col == 4 {
                        print!("{}", if no_color { "" } else { RESET });
                    } else if (5..=11).contains(&legend_col) {
                        print!(
                            "{}",
                            " CPU %  ".chars().nth(legend_col - 5).unwrap_or(' ')
                        );
                    } else if legend_col == 18 {
                        print!("│");
                    } else {
                        print!(" ");
                    }
                    continue;
                } else if row == 2 && legend_col < 19 {
                    if legend_col == 0 {
                        print!("│");
                    } else if legend_col == 1 {
                        print!(" ");
                    } else if legend_col == 2 {
                        print!("{}", if no_color { "" } else { YELLOW });
                    } else if legend_col == 3 {
                        print!("•");
                    } else if legend_col == 4 {
                        print!("{}", if no_color { "" } else { RESET });
                    } else if (5..=11).contains(&legend_col) {
                        print!(
                            "{}",
                            " RSS GB ".chars().nth(legend_col - 5).unwrap_or(' ')
                        );
                    } else if legend_col == 18 {
                        print!("│");
                    } else {
                        print!(" ");
                    }
                    continue;
                } else if row == 3 && legend_col < 19 {
                    let legend = "└─────────────────┘";
                    print!("{}", legend.chars().nth(legend_col).unwrap_or(' '));
                    continue;
                }
            }

            // Plot the data points
            if current_height == cpu_height {
                print!(
                    "{}*{}",
                    if no_color { "" } else { GREEN },
                    if no_color { "" } else { RESET }
                );
            } else if current_height == rss_height {
                print!(
                    "{}•{}",
                    if no_color { "" } else { YELLOW },
                    if no_color { "" } else { RESET }
                );
            } else if current_height == 0 && cpu_val == 0.0 && col % 3 == 0 {
                // Show dots for 0% CPU on the baseline at intervals
                print!(
                    "{}·{}",
                    if no_color { "" } else { GREEN },
                    if no_color { "" } else { RESET }
                );
            } else {
                print!(" ");
            }
        }

        // Right Y-axis (RSS GB)
        print!("┤");
        if row == 0 {
            println!(" {:.2}GB", max_rss_gb);
        } else if row == chart_height - 1 {
            println!(" {:.2}GB", 0.0);
        } else if row == chart_height / 2 {
            println!(" {:.2}GB", max_rss_gb / 2.0);
        } else {
            println!();
        }
    }

    // X-axis
    print!("{:>8}└", "");
    for _ in 0..chart_width {
        print!("─");
    }
    println!("┘");

    // Time labels
    if !samples.is_empty() {
        let start_time = samples.first().unwrap().timestamp;
        let end_time = samples.last().unwrap().timestamp;

        let start_label = start_time
            .with_timezone(&Local)
            .format("%H:%M:%S")
            .to_string();
        let end_label = end_time
            .with_timezone(&Local)
            .format("%H:%M:%S")
            .to_string();

        let padding = chart_width.saturating_sub(start_label.len() + end_label.len());
        print!(
            "{:>8} {}{:padding$}{}",
            "",
            start_label,
            "",
            end_label,
            padding = padding
        );
        println!();
    }
}

fn overall_health_from_unit(unit: &UnitStatus) -> OverallHealth {
    match unit.health {
        UnitHealth::Healthy => OverallHealth::Healthy,
        UnitHealth::Degraded => OverallHealth::Degraded,
        UnitHealth::Failing => OverallHealth::Failing,
    }
}

fn purge_all_state() -> Result<(), Box<dyn Error>> {
    stop_resident_supervisors();

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

fn stop_resident_supervisors() {
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
        forcefully_terminate_supervisor(pid);
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

        if is_daemonized_supervisor_process(process) {
            set.insert(pid.as_u32() as libc::pid_t);
        }
    }

    set
}

fn is_daemonized_supervisor_process(process: &sysinfo::Process) -> bool {
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
                if supervisor_process_exited(pid) {
                    return true;
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                if err == nix::Error::from(nix::errno::Errno::ESRCH) {
                    return true;
                }
                if supervisor_process_exited(pid) {
                    return true;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    false
}

fn forcefully_terminate_supervisor(pid: libc::pid_t) {
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

fn supervisor_process_exited(pid: libc::pid_t) -> bool {
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
