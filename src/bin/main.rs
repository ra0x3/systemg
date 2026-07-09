use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fs, io,
    io::Write,
    os::unix::io::IntoRawFd,
    path::{Path, PathBuf},
    process,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
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
    cli::{Cli, Commands, OutputFormat, parse_args},
    config::{EffectiveLogsConfig, load_config},
    cron::{CronExecutionStatus, CronStateFile},
    daemon::{Daemon, PidFile, ServiceLifecycleStatus},
    ipc::{self, ControlCommand, ControlError, ControlResponse, InspectPayload},
    logs::{
        LogFilter, LogFormat, LogManager, LogSection, LogWriter, RotatingLogWriter,
        get_service_log_path, prune_logs, resolve_log_path, supervisor_log_path,
        write_log_section_header,
    },
    metrics::MetricSample,
    runtime::{self, RuntimeMode},
    spawn::{SpawnedChild, SpawnedChildKind, SpawnedExit},
    status::{
        CronUnitStatus, ExitMetadata, OverallHealth, ProcessState, ProjectRunMode,
        SpawnedProcessNode, StatusSnapshot, UnitHealth, UnitIntent, UnitKind,
        UnitMetricsSummary, UnitState, UnitStatus, UptimeInfo, collect_disk_snapshot,
        compute_overall_health, explain_unit_health, format_elapsed,
    },
    supervisor::Supervisor,
    validate::{self, ValidationReport},
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const UNIT_CONFIG_MAX_FILES: usize = 200;
const UNIT_CONFIG_MAX_AGE_DAYS: u64 = 30;
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
const INSPECT_CRON_HISTORY_LIMIT: usize = 10;
const FETCH_SPINNER_DELAY: Duration = Duration::from_millis(120);
const FETCH_SPINNER_TICK: Duration = Duration::from_millis(80);
const FETCH_SPINNER_FRAMES: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];
const RESTART_DAEMON_ACK_TIMEOUT: Duration = Duration::from_millis(250);
const DEFAULT_CONFIG_PATH: &str = "systemg.yaml";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InspectStreamAction {
    Refresh,
    Exit,
    Start,
    Stop,
    Restart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogsStreamAction {
    Refresh,
    Exit,
}

/// Returns true when a control action is invalid for a cron unit. Cron units
/// are scheduler entries, so start/stop/restart must not dispatch for them.
fn inspect_stream_action_blocked_for_cron(
    kind: UnitKind,
    action: InspectStreamAction,
) -> bool {
    kind == UnitKind::Cron
        && matches!(
            action,
            InspectStreamAction::Start
                | InspectStreamAction::Stop
                | InspectStreamAction::Restart
        )
}

fn inspect_stream_event_action(event: Event) -> Option<InspectStreamAction> {
    match event {
        Event::Key(key_event) if stream_exit_key_event(&key_event) => {
            Some(InspectStreamAction::Exit)
        }
        Event::Key(KeyEvent {
            code: KeyCode::Char('s') | KeyCode::Char('S'),
            ..
        }) => Some(InspectStreamAction::Start),
        Event::Key(KeyEvent {
            code: KeyCode::Char('x') | KeyCode::Char('X'),
            ..
        }) => Some(InspectStreamAction::Stop),
        Event::Key(KeyEvent {
            code: KeyCode::Char('r') | KeyCode::Char('R'),
            ..
        }) => Some(InspectStreamAction::Restart),
        _ => None,
    }
}

fn run_inspect_stream_control_action(
    action: InspectStreamAction,
    config: &str,
    service: &str,
    project: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let command = match action {
        InspectStreamAction::Start => "start",
        InspectStreamAction::Stop => "stop",
        InspectStreamAction::Restart => "restart",
        InspectStreamAction::Refresh | InspectStreamAction::Exit => return Ok(()),
    };

    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sysg"));
    let mut child = process::Command::new(current_exe);
    child.args([command, "--config", config, "--service", service]);
    if let Some(project) = project {
        child.args(["--project", project]);
    }

    let _status = child
        .stdin(process::Stdio::inherit())
        .stdout(process::Stdio::inherit())
        .stderr(process::Stdio::inherit())
        .status()?;

    Ok(())
}

fn logs_stream_event_action(event: Event) -> Option<LogsStreamAction> {
    match event {
        Event::Key(key_event) if stream_exit_key_event(&key_event) => {
            Some(LogsStreamAction::Exit)
        }
        _ => None,
    }
}

fn stream_exit_key_event(key_event: &KeyEvent) -> bool {
    matches!(key_event.code, KeyCode::Esc)
        || matches!(key_event.code, KeyCode::Char('c') | KeyCode::Char('C'))
            && key_event.modifiers.contains(KeyModifiers::CONTROL)
}

fn wait_for_inspect_stream_action(
    sleep_interval: Duration,
) -> Result<InspectStreamAction, Box<dyn Error>> {
    let deadline = Instant::now() + sleep_interval;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(InspectStreamAction::Refresh);
        }

        let poll_timeout = remaining.min(Duration::from_millis(50));
        if event::poll(poll_timeout)?
            && let Some(action) = inspect_stream_event_action(event::read()?)
        {
            return Ok(action);
        }
    }
}

fn wait_for_logs_stream_action(
    sleep_interval: Duration,
) -> Result<LogsStreamAction, Box<dyn Error>> {
    let deadline = Instant::now() + sleep_interval;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(LogsStreamAction::Refresh);
        }

        let poll_timeout = remaining.min(Duration::from_millis(50));
        if event::poll(poll_timeout)?
            && let Some(action) = logs_stream_event_action(event::read()?)
        {
            return Ok(action);
        }
    }
}

fn render_inspect_stream_frame<W: io::Write>(
    writer: &mut W,
    frame_lines: &[String],
    previous_line_count: usize,
) -> io::Result<usize> {
    write!(writer, "\x1B[H")?;

    let total_lines = frame_lines.len().max(previous_line_count);
    for line_idx in 0..total_lines {
        write!(writer, "\x1B[2K")?;
        if let Some(line) = frame_lines.get(line_idx) {
            write!(writer, "{line}")?;
        }
        if line_idx + 1 < total_lines {
            write!(writer, "\r\n")?;
        }
    }

    write!(writer, "\x1B[J")?;
    Ok(frame_lines.len())
}

fn write_inspect_stream_frame(
    frame_lines: &[String],
    previous_line_count: usize,
) -> io::Result<usize> {
    let mut stdout = io::stdout().lock();
    let line_count =
        render_inspect_stream_frame(&mut stdout, frame_lines, previous_line_count)?;
    stdout.flush()?;
    Ok(line_count)
}

fn logs_stream_frame_lines(output: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::to_string)
        .collect()
}

fn write_logs_stream_frame(
    output: &[u8],
    previous_line_count: usize,
) -> io::Result<usize> {
    let frame_lines = logs_stream_frame_lines(output);
    write_inspect_stream_frame(&frame_lines, previous_line_count)
}

fn stderr_is_tty() -> bool {
    unsafe { libc::isatty(libc::STDERR_FILENO) == 1 }
}

fn stdout_is_tty() -> bool {
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

/// Returns whether systemg is running in agent mode (non-interactive automation).
///
/// True when `--plain` was passed (which sets `SYSTEMG_AGENT` for this process),
/// or when `SYSTEMG_AGENT` / `NO_COLOR` is set in the environment.
fn agent_mode() -> bool {
    let set = |name: &str| matches!(std::env::var(name), Ok(value) if !value.is_empty() && value != "0");
    set("SYSTEMG_AGENT") || set("NO_COLOR")
}

/// Applies the global `--plain` flag by enabling agent mode for this process,
/// so every downstream `agent_mode()` check observes it uniformly.
fn apply_plain_mode(plain: bool) {
    if plain {
        unsafe {
            std::env::set_var("SYSTEMG_AGENT", "1");
        }
    }
}

/// Decides whether to follow given explicit flags and the environment.
///
/// Explicit flags win; otherwise systemg follows only on an interactive stdout
/// with agent mode disabled, so pipes, SSH, and agents get a one-shot snapshot.
fn logs_follow_decision(
    follow_flag: bool,
    no_follow_flag: bool,
    stdout_tty: bool,
    agent_mode: bool,
) -> bool {
    if follow_flag {
        return true;
    }
    if no_follow_flag {
        return false;
    }
    stdout_tty && !agent_mode
}

/// Resolves whether `sysg logs` should follow the stream for this invocation.
fn resolve_logs_follow(follow_flag: bool, no_follow_flag: bool) -> bool {
    logs_follow_decision(follow_flag, no_follow_flag, stdout_is_tty(), agent_mode())
}

fn format_progress_spinner_frame(frame: &str, label: &str) -> String {
    format!("\r{frame} {label}\x1B[K")
}

fn clear_progress_spinner_line() -> &'static str {
    "\r\x1B[2K\r"
}

fn with_progress_spinner<T, F>(
    label: &'static str,
    operation: F,
) -> Result<T, Box<dyn Error>>
where
    F: FnOnce() -> Result<T, Box<dyn Error>>,
{
    if !stderr_is_tty() {
        return operation();
    }

    let stop = Arc::new(AtomicBool::new(false));
    let spinner_stop = Arc::clone(&stop);
    let spinner = thread::spawn(move || {
        thread::sleep(FETCH_SPINNER_DELAY);
        if spinner_stop.load(Ordering::Relaxed) {
            return;
        }

        let mut stderr = io::stderr().lock();
        let mut frame_idx = 0usize;
        loop {
            if spinner_stop.load(Ordering::Relaxed) {
                let _ = write!(stderr, "{}", clear_progress_spinner_line());
                let _ = stderr.flush();
                return;
            }

            let frame = FETCH_SPINNER_FRAMES[frame_idx % FETCH_SPINNER_FRAMES.len()];
            let _ = write!(stderr, "{}", format_progress_spinner_frame(frame, label));
            let _ = stderr.flush();
            frame_idx += 1;
            thread::sleep(FETCH_SPINNER_TICK);
        }
    });

    let result = operation();
    stop.store(true, Ordering::Relaxed);
    let _ = spinner.join();
    result
}

/// Runs the `sysg` command-line entrypoint.
fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args();
    apply_plain_mode(args.plain);
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
            project,
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
            let command_project = resolve_command_project(
                &config,
                project.clone(),
                start_target.service.as_deref(),
            )?;

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
                            project: command_project
                                .clone()
                                .or(start_target.project_id.clone()),
                        };
                        with_progress_spinner("Starting", || {
                            send_control_command(command)
                        })?;
                        info!(
                            "Service '{service_name}' start command sent to supervisor"
                        );
                    } else if project.is_some() {
                        let command = ControlCommand::Start {
                            service: None,
                            project: project.clone(),
                        };
                        with_progress_spinner("Starting", || {
                            send_control_command(command)
                        })?;
                    } else {
                        let command = ControlCommand::AddProject {
                            config: start_target
                                .config_path
                                .to_string_lossy()
                                .to_string(),
                            service: None,
                            mode: ProjectRunMode::Daemon,
                        };
                        with_progress_spinner("Starting", || {
                            send_control_command(command)
                        })?;
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
                if supervisor_running() {
                    match start_target.service {
                        Some(service_name) => {
                            let command = ControlCommand::Start {
                                service: Some(service_name.clone()),
                                project: command_project.or(start_target.project_id),
                            };
                            with_progress_spinner("Starting", || {
                                send_control_command(command)
                            })?;
                            info!(
                                "Service '{service_name}' start command sent to supervisor"
                            );
                        }
                        None => {
                            start_foreground_attached(start_target.config_path, None)?;
                        }
                    }
                } else {
                    start_foreground(
                        start_target.config_path,
                        start_target.service,
                        stderr,
                    )?;
                }
            }
        }
        Commands::Stop {
            service,
            project,
            config,
            supervisor,
        } => {
            if supervisor {
                send_control_command(ControlCommand::Shutdown)?;
                return Ok(());
            }

            let service_name = service.clone();
            if supervisor_running() {
                let target_project = if service_name.is_some() {
                    resolve_command_project(&config, project, service_name.as_deref())?
                } else if project.is_some() {
                    project
                } else {
                    Some(resolve_project_context_from_config(&config)?)
                };
                let command = if let Some(name) = service_name.clone() {
                    ControlCommand::Stop {
                        service: Some(name),
                        project: target_project,
                    }
                } else {
                    ControlCommand::Stop {
                        service: None,
                        project: target_project,
                    }
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
            project,
            daemonize,
        } => {
            if supervisor_running() {
                if args.drop_privileges {
                    warn!(
                        "--drop-privileges is managed by the running supervisor and has no effect for this restart request"
                    );
                }
                let target_project = if service.is_some() {
                    resolve_command_project(&config, project.clone(), service.as_deref())?
                } else {
                    project.clone()
                };
                let config_override = if config.is_empty()
                    || (config == DEFAULT_CONFIG_PATH && project.is_some())
                {
                    None
                } else {
                    Some(resolve_config_path(&config)?.display().to_string())
                };

                let command = ControlCommand::Restart {
                    config: config_override.clone(),
                    service: service.clone(),
                    project: target_project,
                };
                if daemonize {
                    let recycle_config_path = config_override
                        .as_ref()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            resolve_config_path(&config)
                                .unwrap_or_else(|_| PathBuf::from(&config))
                        });
                    restart_daemonized(
                        command,
                        recycle_config_path,
                        service.is_none() && project.is_none(),
                    )?;
                } else {
                    with_progress_spinner("Restarting", || {
                        send_control_command(command)
                    })?;
                }
            } else if daemonize {
                let config_path = resolve_config_path(&config)?;
                start_supervisor_daemon(config_path, None, false)?;
            } else {
                warn!(
                    "No running supervisor detected; executing restart in local one-shot mode. \
Use --daemonize in deployment scripts to ensure daemonized supervision is restored if detection fails."
                );
                let daemon = build_daemon(&config)?;
                with_progress_spinner("Restarting", || {
                    daemon
                        .restart_services()
                        .map_err(|err| Box::new(err) as Box<dyn Error>)
                })?;
            }
        }
        Commands::Status {
            config,
            service,
            project,
            all,
            format,
            no_color,
            full_cmd,
            live,
            stream,
        } => {
            let target_project =
                resolve_status_project_filter(config.as_deref(), project.clone())?;
            let render_config = config.as_deref().unwrap_or(DEFAULT_CONFIG_PATH);

            let render_opts = StatusRenderOptions {
                format,
                no_color: no_color || agent_mode(),
                full_cmd,
                include_orphans: all,
                service_filter: service.as_deref(),
                project_filter: target_project.as_deref(),
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
                    match fetch_status_snapshot(config.as_deref(), live) {
                        Ok(snapshot) => {
                            if let Err(e) = render_status(
                                &snapshot,
                                &render_opts,
                                true,
                                render_config,
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
                let snapshot = with_progress_spinner("Computing", || {
                    fetch_status_snapshot(config.as_deref(), live)
                })?;

                let health =
                    render_status(&snapshot, &render_opts, false, render_config)?;

                let exit_code = match health {
                    OverallHealth::Healthy => 0,
                    OverallHealth::Warn => 1,
                    OverallHealth::Failing => 2,
                };
                process::exit(exit_code);
            }
        }
        Commands::Inspect {
            config,
            service,
            project,
            format,
            no_color,
            live,
            stream,
        } => {
            let mut effective_config = config.clone();
            if load_config(Some(&config)).is_err()
                && let Ok(Some(hint)) = ipc::read_config_hint()
            {
                effective_config = hint.to_string_lossy().to_string();
            }
            let target_project = resolve_command_project(
                &effective_config,
                project.clone(),
                Some(&service),
            )?;

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
                format,
                no_color: no_color || agent_mode(),
                window_seconds: stream_seconds,
                window_desc: format!("last {}s", stream_seconds),
                samples_limit,
            };

            if stream.is_some() {
                let is_tty = unsafe {
                    libc::isatty(libc::STDIN_FILENO) == 1
                        && libc::isatty(libc::STDOUT_FILENO) == 1
                };
                let sleep_interval = Duration::from_secs(stream_seconds);

                if is_tty {
                    terminal::enable_raw_mode()?;

                    let result = (|| -> Result<(), Box<dyn Error>> {
                        clear_terminal_output()?;
                        let mut previous_line_count = 0usize;
                        loop {
                            let payload = fetch_inspect(
                                &effective_config,
                                &service,
                                target_project.as_deref(),
                                samples_limit,
                                live,
                            )?;
                            if payload.unit.is_none() {
                                eprintln!("Service '{service}' not found.");
                                process::exit(2);
                            }

                            let (_health, frame_lines) =
                                collect_inspect_lines(&payload, &render_opts)?;
                            previous_line_count = write_inspect_stream_frame(
                                &frame_lines,
                                previous_line_count,
                            )?;

                            match wait_for_inspect_stream_action(sleep_interval)? {
                                InspectStreamAction::Refresh => {}
                                InspectStreamAction::Exit => {
                                    clear_terminal_output()?;
                                    return Ok(());
                                }
                                action @ (InspectStreamAction::Start
                                | InspectStreamAction::Stop
                                | InspectStreamAction::Restart) => {
                                    let unit_kind = payload
                                        .unit
                                        .as_ref()
                                        .map(|unit| unit.kind)
                                        .unwrap_or(UnitKind::Service);
                                    if inspect_stream_action_blocked_for_cron(
                                        unit_kind, action,
                                    ) {
                                        terminal::disable_raw_mode()?;
                                        println!(
                                            "\nCron units cannot be controlled directly; reload the project to reschedule."
                                        );
                                        terminal::enable_raw_mode()?;
                                        continue;
                                    }
                                    terminal::disable_raw_mode()?;
                                    println!();
                                    let action_result = run_inspect_stream_control_action(
                                        action,
                                        &effective_config,
                                        &service,
                                        target_project.as_deref(),
                                    );
                                    terminal::enable_raw_mode()?;
                                    action_result?;
                                    previous_line_count = 0;
                                    clear_terminal_output()?;
                                }
                            }
                        }
                    })();

                    terminal::disable_raw_mode()?;
                    result?;
                    return Ok(());
                }

                loop {
                    let payload = fetch_inspect(
                        &effective_config,
                        &service,
                        target_project.as_deref(),
                        samples_limit,
                        live,
                    )?;
                    if payload.unit.is_none() {
                        eprintln!("Service '{service}' not found.");
                        process::exit(2);
                    }

                    clear_terminal_output()?;
                    let _ = render_inspect(&payload, &render_opts)?;
                    thread::sleep(sleep_interval);
                }
            } else {
                let payload = with_progress_spinner("Inspecting", || {
                    fetch_inspect(
                        &effective_config,
                        &service,
                        target_project.as_deref(),
                        samples_limit,
                        live,
                    )
                })?;
                if payload.unit.is_none() {
                    eprintln!("Service '{service}' not found.");
                    process::exit(2);
                }

                let health = render_inspect(&payload, &render_opts)?;
                let exit_code = match health {
                    OverallHealth::Healthy => 0,
                    OverallHealth::Warn => 1,
                    OverallHealth::Failing => 2,
                };
                process::exit(exit_code);
            }
        }
        Commands::Logs {
            config,
            purge,
            prune,
            max_size,
            max_age,
            service,
            project,
            lines,
            kind,
            follow,
            no_follow,
            since,
            until,
            grep,
            all,
            path,
            format,
            raw,
            strip_ansi,
            no_strip_ansi,
            stream,
        } => {
            if path {
                match service.as_deref() {
                    Some(service_name) => {
                        let active = get_service_log_path(service_name);
                        if all {
                            for path in systemg::logs::rotated_history_paths(&active)
                                .into_iter()
                                .rev()
                            {
                                println!("{}", path.display());
                            }
                        } else {
                            println!("{}", active.display());
                        }
                    }
                    None => println!("{}", systemg::runtime::log_dir().display()),
                }
                return Ok(());
            }
            if prune {
                if max_size.is_none() && max_age.is_none() {
                    eprintln!(
                        "Specify --max-size and/or --max-age to prune rotated logs."
                    );
                    process::exit(2);
                }
                let summary = prune_logs(max_size.as_deref(), max_age.as_deref())?;
                println!(
                    "Pruned {} rotated log file(s), reclaimed {} bytes.",
                    summary.removed_files, summary.reclaimed_bytes
                );
                return Ok(());
            }
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
            let target_project = resolve_command_project(
                &effective_config,
                project.clone(),
                service.as_deref(),
            )?;

            let pid = Arc::new(Mutex::new(PidFile::load().unwrap_or_default()));
            let manager = LogManager::new(pid.clone());

            if purge {
                if target_project.is_some() {
                    let snapshot = with_progress_spinner("Purging logs", || {
                        fetch_status_snapshot(Some(&effective_config), false)
                    })?;
                    let matching_units: Vec<_> = snapshot
                        .units
                        .iter()
                        .filter(|unit| {
                            status_unit_matches_selector(
                                unit,
                                service.as_deref(),
                                target_project.as_deref(),
                            )
                        })
                        .collect();
                    if matching_units.is_empty() {
                        eprintln!("No matching units found.");
                        process::exit(2);
                    }
                    for unit in matching_units {
                        info!("Purging logs for service: {}", unit.name);
                        manager.clear_service_logs(&unit.name)?;
                    }
                } else {
                    match service.as_deref() {
                        Some(service_name) => {
                            info!("Purging logs for service: {service_name}");
                            manager.clear_service_logs(service_name)?;
                        }
                        None => {
                            info!("Purging logs for all services");
                            manager.clear_all_logs()?;
                        }
                    }
                }
                return Ok(());
            }

            let log_filter = LogFilter::from_parts(
                since.as_deref(),
                until.as_deref(),
                grep.as_deref(),
                all,
                chrono::Utc::now(),
            )?;

            let log_format = match format {
                Some(OutputFormat::Json) => LogFormat::Json,
                Some(OutputFormat::Xml) => {
                    eprintln!("`sysg logs` does not support --format xml; use json.");
                    process::exit(2);
                }
                None if raw => LogFormat::Raw,
                None => LogFormat::Text,
            };
            let strip_ansi_output = if no_strip_ansi {
                false
            } else {
                strip_ansi
                    || !matches!(log_format, LogFormat::Text)
                    || !stdout_is_tty()
                    || agent_mode()
            };
            // Whether output must pass through the reformatting LogWriter at all.
            let machine_output =
                !matches!(log_format, LogFormat::Text) || strip_ansi_output;
            // Structured formats (json/raw) intentionally drop banners and read
            // straight from captured bytes; plain text keeps its service header.
            let structured_output = !matches!(log_format, LogFormat::Text);

            let make_log_writer = || {
                LogWriter::new(
                    io::stdout(),
                    log_format,
                    strip_ansi_output,
                    service.clone(),
                )
            };

            let stream_logs_via_supervisor =
                |follow: bool| -> Result<(), Box<dyn Error>> {
                    let command = ControlCommand::Logs {
                        service: service.clone(),
                        project: target_project.clone(),
                        lines,
                        kind: kind.as_ref().map(|kind| kind.as_str().to_string()),
                        follow,
                        since: since.clone(),
                        until: until.clone(),
                        grep: grep.clone(),
                        all,
                        structured: structured_output,
                    };
                    let mut writer = make_log_writer();
                    ipc::stream_command_output(&command, &mut writer)
                        .map_err(|err| Box::new(err) as Box<dyn Error>)?;
                    writer
                        .flush()
                        .map_err(|err| Box::new(err) as Box<dyn Error>)
                };

            let render_logs_once = |snapshot_mode: bool| -> Result<(), Box<dyn Error>> {
                let snapshot = with_progress_spinner("Logging", || {
                    fetch_status_snapshot(Some(&effective_config), false)
                })?;

                match service.as_ref() {
                    Some(service_name) if structured_output => {
                        info!("Fetching logs for service: {service_name}");
                        let bytes = manager.collect_service_log(
                            service_name,
                            lines,
                            kind.as_ref().map(|kind| kind.as_str()),
                            &log_filter,
                        )?;
                        let mut writer = make_log_writer();
                        writer.write_all(&bytes)?;
                        writer.flush()?;
                    }
                    Some(service_name) => {
                        info!("Fetching logs for service: {service_name}");
                        render_service_logs_from_snapshot(
                            &manager,
                            &snapshot,
                            service_name,
                            target_project.as_deref(),
                            lines,
                            kind.as_ref().map(|kind| kind.as_str()),
                            snapshot_mode,
                            &log_filter,
                        )?;
                    }
                    None => {
                        info!("Fetching logs for all services");
                        render_all_logs_from_snapshot(
                            &manager,
                            &snapshot,
                            target_project.as_deref(),
                            lines,
                            kind.as_ref().map(|kind| kind.as_str()),
                            snapshot_mode,
                            &log_filter,
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
                let logs_stream_tty = unsafe {
                    libc::isatty(libc::STDIN_FILENO) == 1
                        && libc::isatty(libc::STDOUT_FILENO) == 1
                };
                if logs_stream_tty {
                    terminal::enable_raw_mode()?;
                }
                let stream_result = (|| -> Result<(), Box<dyn Error>> {
                    let mut previous_line_count = 0usize;
                    if logs_stream_tty {
                        clear_terminal_output()?;
                    }
                    loop {
                        let command = ControlCommand::Logs {
                            service: service.clone(),
                            project: target_project.clone(),
                            lines,
                            kind: kind.as_ref().map(|kind| kind.as_str().to_string()),
                            follow: false,
                            since: since.clone(),
                            until: until.clone(),
                            grep: grep.clone(),
                            all,
                            structured: structured_output,
                        };
                        let mut output = Vec::new();
                        match ipc::stream_command_output(&command, &mut output)
                            .map_err(|err| Box::new(err) as Box<dyn Error>)
                        {
                            Ok(()) => {
                                if logs_stream_tty {
                                    previous_line_count = write_logs_stream_frame(
                                        &output,
                                        previous_line_count,
                                    )?;
                                } else if machine_output {
                                    let mut writer = make_log_writer();
                                    writer.write_all(&output)?;
                                    writer.flush()?;
                                } else {
                                    io::stdout().write_all(&output)?;
                                    io::stdout().flush()?;
                                }
                            }
                            Err(err) => match err.downcast_ref::<ControlError>() {
                                Some(ControlError::NotAvailable) => {
                                    if logs_stream_tty {
                                        terminal::disable_raw_mode()?;
                                        clear_terminal_output()?;
                                    }
                                    render_logs_once(true)?;
                                    previous_line_count = 0;
                                    if logs_stream_tty {
                                        terminal::enable_raw_mode()?;
                                    }
                                }
                                _ => return Err(err),
                            },
                        }
                        if logs_stream_tty {
                            if matches!(
                                wait_for_logs_stream_action(sleep_interval)?,
                                LogsStreamAction::Exit
                            ) {
                                return Ok(());
                            }
                        } else {
                            thread::sleep(sleep_interval);
                        }
                    }
                })();
                if logs_stream_tty {
                    terminal::disable_raw_mode()?;
                }
                stream_result?;
            } else {
                let follow_logs = resolve_logs_follow(follow, no_follow);
                match stream_logs_via_supervisor(follow_logs) {
                    Ok(()) => {}
                    Err(err) => match err.downcast_ref::<ControlError>() {
                        Some(ControlError::NotAvailable) => {
                            render_logs_once(!follow_logs)?
                        }
                        _ => return Err(err),
                    },
                }
            }
        }
        Commands::Validate {
            config,
            format,
            no_color,
        } => {
            let (report, content) = validate::validate(&config);
            let use_color = !(no_color || agent_mode());
            match format {
                Some(fmt) => {
                    println!("{}", serialize_machine_output(&report, fmt)?);
                }
                None => {
                    render_validation_report(&report, content.as_deref(), use_color);
                }
            }
            process::exit(if report.valid { 0 } else { 1 });
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

/// Renders a validation report as a human-readable diagnostic, optionally with
/// a caret-annotated source snippet and ANSI color.
fn render_validation_report(
    report: &ValidationReport,
    content: Option<&str>,
    use_color: bool,
) {
    let paint = |code: &str, text: &str| {
        if use_color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    };

    println!();
    if report.valid {
        println!(
            "  {}  {}",
            paint(GREEN_BOLD, "✓ valid"),
            paint(BRIGHT_WHITE, &report.config)
        );
        println!(
            "  {}",
            paint(GRAY, "This manifest parses and resolves cleanly.")
        );
        println!();
        return;
    }

    let count = report.diagnostics.len();
    let noun = if count == 1 { "problem" } else { "problems" };
    println!(
        "  {}  {} {}",
        paint(RED_BOLD, "✗ invalid"),
        paint(BRIGHT_WHITE, &report.config),
        paint(GRAY, &format!("· {count} {noun}"))
    );

    for (index, diagnostic) in report.diagnostics.iter().enumerate() {
        println!();
        let where_at = match (diagnostic.line, diagnostic.column) {
            (Some(line), Some(col)) => format!("line {line}:{col}"),
            (Some(line), None) => format!("line {line}"),
            _ => "config".to_string(),
        };
        println!(
            "  {} {}  {}",
            paint(RED_BOLD, &format!("{}.", index + 1)),
            paint(RED, &diagnostic.kind),
            paint(GRAY, &where_at)
        );
        println!("     {}", paint(WHITE, &diagnostic.message));

        if let Some(line) = diagnostic.line
            && let Some(text) =
                content.and_then(|c| c.lines().nth(line.saturating_sub(1)))
        {
            println!();
            let gutter = format!("{:>4} │ ", line);
            println!("  {}{}", paint(GRAY, &gutter), paint(BRIGHT_WHITE, text));
            if let Some(col) = diagnostic.column {
                let pad = " ".repeat(2 + gutter.len() + col.saturating_sub(1));
                println!("{}{}", pad, paint(RED_BOLD, "^"));
            }
            println!();
        }

        println!("     {} {}", paint(YELLOW, "why "), diagnostic.why);
        println!("     {} {}", paint(GREEN, "fix "), diagnostic.suggestion);
        println!(
            "     {} {}",
            paint(CYAN, "docs"),
            paint(CYAN, &diagnostic.doc)
        );
    }
    println!();
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
    fn logs_follow_flag_forces_follow() {
        assert!(logs_follow_decision(true, false, false, true));
    }

    #[test]
    fn logs_no_follow_flag_forces_oneshot() {
        assert!(!logs_follow_decision(false, true, true, false));
    }

    #[test]
    fn logs_default_follows_only_on_interactive_non_agent() {
        assert!(logs_follow_decision(false, false, true, false));
        assert!(!logs_follow_decision(false, false, false, false));
        assert!(!logs_follow_decision(false, false, true, true));
        assert!(!logs_follow_decision(false, false, false, true));
    }

    #[test]
    fn inspect_stream_blocks_control_actions_for_cron_units() {
        for action in [
            InspectStreamAction::Start,
            InspectStreamAction::Stop,
            InspectStreamAction::Restart,
        ] {
            assert!(inspect_stream_action_blocked_for_cron(
                UnitKind::Cron,
                action
            ));
            assert!(!inspect_stream_action_blocked_for_cron(
                UnitKind::Service,
                action
            ));
        }

        for action in [InspectStreamAction::Refresh, InspectStreamAction::Exit] {
            assert!(!inspect_stream_action_blocked_for_cron(
                UnitKind::Cron,
                action
            ));
        }
    }

    #[test]
    fn status_restart_control_blocked_for_cron_units() {
        assert!(status_restart_blocked_for_cron(UnitKind::Cron));
        assert!(!status_restart_blocked_for_cron(UnitKind::Service));
        assert!(!status_restart_blocked_for_cron(UnitKind::Orphaned));
    }

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
            project: None,
            kind: UnitKind::Service,
            lifecycle: Some(ServiceLifecycleStatus::Running),
            state: UnitState::Running,
            intent: UnitIntent::Serve,
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
        let unit_row = format_unit_row_focus(&unit, &columns, true, None);
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
        let child_row = format_spawned_child_row(
            &child,
            &columns,
            true,
            "└─ ",
            RowTintFamily::Success,
        );
        assert!(child_row.contains("spwn"));
        assert!(child_row.contains("rashad"));
    }

    #[test]
    fn status_overview_uses_rail_layout_and_large_bullets() {
        let columns = vec![
            Column {
                title: "UNIT",
                width: 24,
                align: Alignment::Left,
            },
            Column {
                title: "KIND",
                width: 4,
                align: Alignment::Left,
            },
            Column {
                title: "STATE",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "USER",
                width: 6,
                align: Alignment::Left,
            },
            Column {
                title: "PID",
                width: 5,
                align: Alignment::Right,
            },
            Column {
                title: "CPU",
                width: 5,
                align: Alignment::Right,
            },
            Column {
                title: "RSS",
                width: 6,
                align: Alignment::Right,
            },
            Column {
                title: "UPTIME",
                width: 8,
                align: Alignment::Left,
            },
            Column {
                title: "CMD",
                width: 20,
                align: Alignment::Left,
            },
            Column {
                title: "LAST_EXIT",
                width: 9,
                align: Alignment::Left,
            },
            Column {
                title: "HEALTH",
                width: 8,
                align: Alignment::Left,
            },
        ];
        let units = vec![
            UnitStatus {
                name: "api".to_string(),
                hash: "api".to_string(),
                project: None,
                kind: UnitKind::Service,
                lifecycle: Some(ServiceLifecycleStatus::Running),
                state: UnitState::Running,
                intent: UnitIntent::Serve,
                health: UnitHealth::Healthy,
                process: None,
                uptime: None,
                last_exit: None,
                cron: None,
                metrics: None,
                command: None,
                runtime_command: None,
                spawned_children: vec![],
            },
            UnitStatus {
                name: "worker".to_string(),
                hash: "worker".to_string(),
                project: None,
                kind: UnitKind::Service,
                lifecycle: Some(ServiceLifecycleStatus::Stopped),
                state: UnitState::Lost,
                intent: UnitIntent::Serve,
                health: UnitHealth::Warn,
                process: None,
                uptime: None,
                last_exit: None,
                cron: None,
                metrics: None,
                command: None,
                runtime_command: None,
                spawned_children: vec![],
            },
        ];

        let lines = status_overview_lines(&columns, &units, OverallHealth::Warn, true);
        let rendered = lines.join("\n");

        assert!(rendered.contains("Status: WARN"));
        assert!(rendered.contains("╟───────────────┬"));
        assert!(rendered.contains("Health"));
        assert!(rendered.contains("Healthy 1"));
        assert!(rendered.contains("•"));
        assert!(rendered.contains("State"));
        assert!(rendered.contains("Running 1"));
        assert!(rendered.contains("Lost 1"));
        assert!(rendered.contains("Intent"));
        assert!(rendered.contains("Serve 2"));
    }

    #[test]
    fn inspect_overview_renders_state_under_kind() {
        let unit = UnitStatus {
            name: "orchestrator".to_string(),
            hash: "abc123".to_string(),
            project: None,
            kind: UnitKind::Service,
            lifecycle: Some(ServiceLifecycleStatus::Running),
            state: UnitState::Running,
            intent: UnitIntent::Serve,
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
        let payload = InspectPayload {
            unit: Some(unit),
            samples: Vec::new(),
        };
        let opts = InspectRenderOptions {
            format: None,
            no_color: true,
            window_seconds: 5,
            window_desc: "last 5s".to_string(),
            samples_limit: 5,
        };

        let (_health, lines) =
            collect_inspect_lines(&payload, &opts).expect("inspect lines");
        let kind_index = lines
            .iter()
            .position(|line| line.contains("Kind: service"))
            .expect("kind line");

        assert!(
            lines
                .get(kind_index + 1)
                .is_some_and(|line| line.contains("State: Running")),
            "expected State row immediately under Kind row:\n{}",
            lines.join("\n")
        );
    }

    #[test]
    fn status_project_groups_preserve_project_boundaries() {
        let units = vec![
            UnitStatus {
                name: "api".to_string(),
                hash: "hash-a".to_string(),
                project: Some(systemg::status::ProjectStatus {
                    id: "arbitration".to_string(),
                    name: "Arbitration".to_string(),
                    mode: ProjectRunMode::Daemon,
                    config_path: None,
                }),
                kind: UnitKind::Service,
                lifecycle: None,
                state: UnitState::Unknown,
                intent: UnitIntent::Manual,
                health: UnitHealth::Healthy,
                process: None,
                uptime: None,
                last_exit: None,
                cron: None,
                metrics: None,
                command: None,
                runtime_command: None,
                spawned_children: vec![],
            },
            UnitStatus {
                name: "api".to_string(),
                hash: "hash-b".to_string(),
                project: Some(systemg::status::ProjectStatus {
                    id: "gamecast".to_string(),
                    name: "Gamecast".to_string(),
                    mode: ProjectRunMode::Daemon,
                    config_path: None,
                }),
                kind: UnitKind::Service,
                lifecycle: None,
                state: UnitState::Unknown,
                intent: UnitIntent::Manual,
                health: UnitHealth::Healthy,
                process: None,
                uptime: None,
                last_exit: None,
                cron: None,
                metrics: None,
                command: None,
                runtime_command: None,
                spawned_children: vec![],
            },
        ];

        let groups = status_project_groups(&units, true);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "Arbitration (arbitration) [daemon]");
        assert_eq!(groups[0].1[0].1.hash, "hash-a");
        assert_eq!(groups[1].0, "Gamecast (gamecast) [daemon]");
        assert_eq!(groups[1].1[0].1.hash, "hash-b");
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

        let row = format_spawned_child_row(
            &child,
            &columns,
            false,
            "└─ ",
            RowTintFamily::Neutral,
        );
        assert!(
            row.starts_with(nested_row_tint_color(RowTintFamily::Neutral, child.depth))
        );
        assert!(row.ends_with(RESET));
        assert!(row.contains("└─ /opt/homebrew/bin/claude"));
        assert!(row.contains("rashad"));
        assert!(row.contains("62751"));
        assert!(row.contains("0.0%"));
        assert!(row.contains("117.7MB"));
        assert!(row.contains("/opt/homebrew/bin/claude --dangerously-skip-permissions"));
        assert!(row.contains("peri"));
        assert!(row.contains("Running"));
        assert!(row.contains("Healthy"));
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
        let shallow_row = format_spawned_child_row(
            &shallow,
            &columns,
            false,
            "└─ ",
            RowTintFamily::Success,
        );
        shallow.depth = 4;
        let deep_row = format_spawned_child_row(
            &shallow,
            &columns,
            false,
            "└─ ",
            RowTintFamily::Success,
        );

        assert!(
            shallow_row.starts_with(nested_row_tint_color(RowTintFamily::Success, 1))
        );
        assert!(deep_row.starts_with(nested_row_tint_color(RowTintFamily::Success, 4)));
        assert_ne!(
            nested_row_tint_color(RowTintFamily::Success, 1),
            nested_row_tint_color(RowTintFamily::Success, 4)
        );
        assert!(shallow_row.contains("└─ worker"));
        assert!(deep_row.contains("└─ worker"));
    }

    #[test]
    fn spawned_rows_inherit_running_parent_green_family() {
        let unit = UnitStatus {
            name: "orchestrator".to_string(),
            hash: "abc123".to_string(),
            project: None,
            kind: UnitKind::Service,
            lifecycle: Some(ServiceLifecycleStatus::Running),
            state: UnitState::Unknown,
            intent: UnitIntent::Manual,
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

        assert_eq!(
            nested_row_tint_color(unit_row_tint_family(&unit), 1),
            "\x1b[38;5;71m"
        );
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

        let row = format_spawned_child_row(
            &child,
            &columns,
            false,
            "└─ ",
            RowTintFamily::Neutral,
        );
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
            project: None,
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
            project: None,
            daemonize: false,
        }));
        assert!(!drop_privileges_applies_to_command(&Commands::Status {
            config: None,
            service: None,
            project: None,
            all: false,
            format: None,
            no_color: false,
            full_cmd: false,
            stream: None,
            live: false,
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
            project: None,
            kind: UnitKind::Service,
            lifecycle: Some(ServiceLifecycleStatus::Running),
            state: UnitState::Unknown,
            intent: UnitIntent::Manual,
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
            status: "success".to_string(),
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
            status: "success".to_string(),
            user: "postgres".to_string(),
            pid: "12345".to_string(),
            command: "sh scripts/migrate-provider-data.sh --delete --sink rds --force"
                .to_string(),
        }];
        let mut widths = compute_inspect_cron_preferred_widths(&rows);
        let original_cmd = widths[5];
        shrink_inspect_cron_widths_to_fit(&mut widths, 60);
        assert!(widths[5] < original_cmd);
        assert!(widths[3] >= INSPECT_CRON_SOFT_MIN_WIDTHS[3]);
    }

    #[test]
    fn format_inspect_cron_status_colors_by_outcome() {
        let success =
            format_inspect_cron_status(Some(&CronExecutionStatus::Success), false);
        assert!(success.contains("success"));
        assert!(success.contains(BRIGHT_GREEN));

        let running = format_inspect_cron_status(None, false);
        assert!(running.contains("running"));
        assert!(running.contains(LIGHT_BLUE));

        let failed = format_inspect_cron_status(
            Some(&CronExecutionStatus::Failed("boom".into())),
            false,
        );
        assert!(failed.contains("failed: boom"));
        assert!(failed.contains(RED_BOLD));
    }

    #[test]
    fn format_inspect_cron_status_respects_no_color() {
        let success =
            format_inspect_cron_status(Some(&CronExecutionStatus::Success), true);
        assert_eq!(success, "success");
    }

    #[test]
    fn wrap_paragraph_respects_width_and_keeps_words_whole() {
        let text = "the quick brown fox jumps over the lazy dog";
        let lines = wrap_paragraph(text, 20);
        assert!(lines.iter().all(|line| visible_length(line) <= 20));
        let rejoined = lines.join(" ");
        assert_eq!(rejoined, text);
    }

    #[test]
    fn render_health_report_includes_required_sections() {
        let mut unit = UnitStatus {
            name: "api".into(),
            hash: "hash".into(),
            project: None,
            kind: UnitKind::Service,
            lifecycle: Some(ServiceLifecycleStatus::Stopped),
            state: UnitState::Stopped,
            intent: UnitIntent::Serve,
            health: UnitHealth::Warn,
            process: None,
            uptime: None,
            last_exit: None,
            cron: None,
            metrics: None,
            command: None,
            runtime_command: None,
            spawned_children: Vec::new(),
        };
        unit.intent = UnitIntent::Serve;

        let rendered = render_health_report(&unit, true).join("\n");
        assert!(rendered.contains("# "));
        assert!(rendered.contains("Severity:"));
        assert!(rendered.contains("TLDR:"));
        assert!(rendered.contains("## Description"));
        assert!(rendered.contains("## Recommended Fix"));
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

    #[test]
    fn inspect_stream_event_action_exits_on_escape() {
        let action = inspect_stream_event_action(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));

        assert_eq!(action, Some(InspectStreamAction::Exit));
    }

    #[test]
    fn inspect_stream_event_action_exits_on_ctrl_c() {
        let action = inspect_stream_event_action(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));

        assert_eq!(action, Some(InspectStreamAction::Exit));
    }

    #[test]
    fn inspect_stream_event_action_starts_on_s() {
        let action = inspect_stream_event_action(Event::Key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::NONE,
        )));

        assert_eq!(action, Some(InspectStreamAction::Start));
    }

    #[test]
    fn inspect_stream_event_action_stops_on_x() {
        let action = inspect_stream_event_action(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));

        assert_eq!(action, Some(InspectStreamAction::Stop));
    }

    #[test]
    fn inspect_stream_event_action_restarts_on_r() {
        let action = inspect_stream_event_action(Event::Key(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        )));

        assert_eq!(action, Some(InspectStreamAction::Restart));
    }

    #[test]
    fn inspect_stream_event_action_ignores_other_keys() {
        let action = inspect_stream_event_action(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )));

        assert_eq!(action, None);
    }

    #[test]
    fn logs_stream_event_action_exits_on_escape() {
        let action = logs_stream_event_action(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));

        assert_eq!(action, Some(LogsStreamAction::Exit));
    }

    #[test]
    fn logs_stream_event_action_exits_on_ctrl_c() {
        let action = logs_stream_event_action(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));

        assert_eq!(action, Some(LogsStreamAction::Exit));
    }

    #[test]
    fn logs_stream_event_action_ignores_other_keys() {
        let action = logs_stream_event_action(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )));

        assert_eq!(action, None);
    }

    #[test]
    fn status_interactive_exit_key_event_exits_on_ctrl_c() {
        let key_event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        assert!(status_interactive_exit_key_event(&key_event));
    }

    #[test]
    fn render_inspect_stream_frame_starts_at_home_and_clears_stale_lines() {
        let frame = vec!["new top".to_string()];
        let mut output = Vec::new();
        let line_count =
            render_inspect_stream_frame(&mut output, &frame, 3).expect("write frame");

        assert_eq!(line_count, 1);
        assert_eq!(
            String::from_utf8(output).expect("utf8"),
            "\x1B[H\x1B[2Knew top\r\n\x1B[2K\r\n\x1B[2K\x1B[J"
        );
    }

    #[test]
    fn render_inspect_stream_frame_rewrites_each_visible_line_without_full_clear() {
        let frame = vec!["alpha".to_string(), "beta".to_string()];
        let mut output = Vec::new();

        let line_count =
            render_inspect_stream_frame(&mut output, &frame, 1).expect("write frame");

        assert_eq!(line_count, 2);
        let rendered = String::from_utf8(output).expect("utf8");
        assert!(rendered.starts_with("\x1B[H\x1B[2Kalpha\r\n\x1B[2Kbeta"));
        assert!(
            !rendered.contains("\x1B[2J"),
            "steady-state frame writes should not clear the full terminal"
        );
    }

    #[test]
    fn render_logs_stream_frame_rewrites_lines_with_carriage_returns() {
        let output = b"header\n\nlog line one\nlog line two\n";
        let mut frame_output = Vec::new();
        let frame_lines = logs_stream_frame_lines(output);
        let line_count = render_inspect_stream_frame(&mut frame_output, &frame_lines, 5)
            .expect("write frame");

        assert_eq!(line_count, 4);
        assert_eq!(
            String::from_utf8(frame_output).expect("utf8"),
            "\x1B[H\x1B[2Kheader\r\n\x1B[2K\r\n\x1B[2Klog line one\r\n\x1B[2Klog line two\r\n\x1B[2K\x1B[J"
        );
    }

    #[test]
    fn progress_spinner_frame_uses_requested_label() {
        assert_eq!(
            format_progress_spinner_frame("⠋", "Computing"),
            "\r⠋ Computing\x1B[K"
        );
        assert_eq!(
            format_progress_spinner_frame("⠙", "Inspecting"),
            "\r⠙ Inspecting\x1B[K"
        );
        assert_eq!(
            format_progress_spinner_frame("⠹", "Starting"),
            "\r⠹ Starting\x1B[K"
        );
        assert_eq!(
            format_progress_spinner_frame("⠸", "Restarting"),
            "\r⠸ Restarting\x1B[K"
        );
    }

    #[test]
    fn progress_spinner_clear_sequence_erases_the_active_line() {
        assert_eq!(clear_progress_spinner_line(), "\r\x1B[2K\r");
    }

    #[test]
    fn restart_protocol_mismatch_detection_matches_schema_errors_only() {
        assert!(supervisor_error_is_protocol_mismatch(
            "failed to serialise control message: invalid type: null, expected a string"
        ));
        assert!(supervisor_error_is_protocol_mismatch(
            "missing field `project` at line 1 column 42"
        ));
        assert!(!supervisor_error_is_protocol_mismatch(
            "failed to load config sysg.config.yaml"
        ));
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

    let mut has_supervisor_command = false;
    let mut has_daemonize = false;

    for arg in cmd {
        let value = arg.to_string_lossy();
        if value == "start" || value == "restart" {
            has_supervisor_command = true;
        } else if value == "--daemonize" {
            has_daemonize = true;
        }
    }

    has_supervisor_command && has_daemonize
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
        if let Err(err) = runtime::create_private_dir(&log_dir) {
            eprintln!("Failed to create log directory {:?}: {}", log_dir, err);
        }
        let log_path = supervisor_log_path();

        let writer = match RotatingLogWriter::open(
            log_path.clone(),
            EffectiveLogsConfig::default(),
        ) {
            Ok(writer) => writer,
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
            .with_writer(writer)
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
    let config = load_config(Some(config_path.to_string_lossy().as_ref()))?;
    let project_id = config.project.id.clone();

    let child_pid = unsafe { libc::fork() };
    if child_pid < 0 {
        return Err(io::Error::last_os_error().into());
    }

    if child_pid == 0 {
        unsafe {
            libc::setsid();
        }

        let mut supervisor = Supervisor::new_with_mode(
            config_path,
            false,
            service,
            ProjectRunMode::Foreground,
        )?;
        supervisor.set_pipe_stderr(pipe_stderr);
        if let Err(err) = supervisor.run() {
            error!("Supervisor exited with error: {err}");
            process::exit(1);
        }
        process::exit(0);
    }

    with_progress_spinner("Starting", || wait_for_supervisor_ready(child_pid))?;
    wait_for_foreground_attachment(project_id)
}

/// Adds a foreground project to the resident supervisor and owns its terminal lifetime.
fn start_foreground_attached(
    config_path: PathBuf,
    service: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let config = load_config(Some(config_path.to_string_lossy().as_ref()))?;
    let project_id = config.project.id.clone();
    let command = ControlCommand::AddProject {
        config: config_path.to_string_lossy().to_string(),
        service,
        mode: ProjectRunMode::Foreground,
    };
    with_progress_spinner("Starting", || send_control_command(command))?;

    wait_for_foreground_attachment(project_id)
}

/// Waits for Ctrl-C and then stops the foreground-owned project.
fn wait_for_foreground_attachment(project_id: String) -> Result<(), Box<dyn Error>> {
    let (tx, rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })?;
    let _ = rx.recv();

    let stop_result: Result<(), Box<dyn Error>> =
        match ipc::send_command(&ControlCommand::StopProject {
            project: project_id.clone(),
        }) {
            Ok(ControlResponse::Message(message)) => {
                println!("{message}");
                Ok(())
            }
            Ok(ControlResponse::Ok) => Ok(()),
            Ok(ControlResponse::Error(message)) => {
                Err(ControlError::Server(message).into())
            }
            Ok(other) => Err(io::Error::other(format!(
                "unexpected supervisor response: {:?}",
                other
            ))
            .into()),
            Err(err) => Err(err.into()),
        };
    stop_result?;

    if !supervisor_has_daemon_projects()? {
        match ipc::send_command(&ControlCommand::Shutdown) {
            Ok(ControlResponse::Message(message)) => {
                println!("{message}");
            }
            Ok(ControlResponse::Ok) => {}
            Ok(ControlResponse::Error(message)) => {
                return Err(ControlError::Server(message).into());
            }
            Ok(other) => {
                return Err(io::Error::other(format!(
                    "unexpected supervisor response: {:?}",
                    other
                ))
                .into());
            }
            Err(ControlError::NotAvailable) => {}
            Err(err) => return Err(err.into()),
        }
    }

    Ok(())
}

/// Waits for a newly forked supervisor child to publish its control socket.
fn wait_for_supervisor_ready(child_pid: libc::pid_t) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if supervisor_running() {
            return Ok(());
        }

        let target = Pid::from_raw(child_pid);
        if let Err(err) = signal::kill(target, None)
            && err == nix::Error::from(nix::errno::Errno::ESRCH)
        {
            return Err(
                io::Error::other("foreground supervisor exited during startup").into(),
            );
        }

        thread::sleep(Duration::from_millis(50));
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "timed out waiting for foreground supervisor to start",
    )
    .into())
}

/// Returns whether the resident supervisor still owns any daemon-mode project.
fn supervisor_has_daemon_projects() -> Result<bool, Box<dyn Error>> {
    match ipc::send_command(&ControlCommand::Status { live: true }) {
        Ok(ControlResponse::Status(snapshot)) => Ok(snapshot.units.iter().any(|unit| {
            unit.project
                .as_ref()
                .is_some_and(|project| project.mode == ProjectRunMode::Daemon)
        })),
        Ok(ControlResponse::Error(message)) => Err(ControlError::Server(message).into()),
        Ok(other) => Err(io::Error::other(format!(
            "unexpected supervisor response: {:?}",
            other
        ))
        .into()),
        Err(ControlError::NotAvailable) => Ok(false),
        Err(err) => Err(err.into()),
    }
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
    project_id: Option<String>,
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
        let config_path = resolve_config_path(config)?;
        let project_id = Some(
            load_config(Some(config_path.to_string_lossy().as_ref()))?
                .project
                .id,
        );
        return Ok(StartTarget {
            config_path,
            service,
            project_id,
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
        project_id: None,
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

    fresh_entries.sort_by_key(|entry| std::cmp::Reverse(entry.1));
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

/// Strips an optional `project/service` selector prefix.
fn service_selector_name(selector: &str) -> &str {
    selector
        .split_once('/')
        .map(|(_, service)| service)
        .unwrap_or(selector)
}

/// Resolves the project represented by a required config-backed command context.
fn resolve_project_context_from_config(
    config_arg: &str,
) -> Result<String, Box<dyn Error>> {
    let config_path = resolve_config_path(config_arg)?;
    let config = load_config(Some(config_path.to_string_lossy().as_ref()))?;
    Ok(config.project.id)
}

/// Resolves the project filter for status without treating the default config as mandatory.
fn resolve_status_project_filter(
    config_arg: Option<&str>,
    explicit_project: Option<String>,
) -> Result<Option<String>, Box<dyn Error>> {
    match (config_arg, explicit_project) {
        (Some(config_arg), Some(project)) => {
            let config_path = resolve_config_path(config_arg)?;
            let config = load_config(Some(config_path.to_string_lossy().as_ref()))?;
            if config.project.id != project {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "project '{}' does not match config project '{}'",
                        project, config.project.id
                    ),
                )
                .into());
            }
            Ok(Some(project))
        }
        (Some(config_arg), None) => {
            let config_path = resolve_config_path(config_arg)?;
            let config = load_config(Some(config_path.to_string_lossy().as_ref()))?;
            Ok(Some(config.project.id))
        }
        (None, project) => Ok(project),
    }
}

/// Resolves the project a command should target from an explicit project flag and config.
fn resolve_command_project(
    config_arg: &str,
    explicit_project: Option<String>,
    service: Option<&str>,
) -> Result<Option<String>, Box<dyn Error>> {
    let config_path = resolve_config_path(config_arg)?;
    let config_value = load_config(Some(config_path.to_string_lossy().as_ref())).ok();

    if let Some(project) = explicit_project {
        if config_arg != DEFAULT_CONFIG_PATH
            && let Some(config) = config_value.as_ref()
            && config.project.id != project
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "project '{}' does not match config project '{}'",
                    project, config.project.id
                ),
            )
            .into());
        }
        return Ok(Some(project));
    }

    let Some(config) = config_value else {
        return Ok(None);
    };

    if config_arg != DEFAULT_CONFIG_PATH {
        return Ok(Some(config.project.id));
    }

    if let Some(service) = service
        && config.services.contains_key(service_selector_name(service))
    {
        return Ok(Some(config.project.id));
    }

    Ok(None)
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
                match ipc::send_command(&ControlCommand::Status { live: false }) {
                    Ok(_) => {
                        warn!("Found running systemg supervisor socket without PID file");
                        return true;
                    }
                    Err(ControlError::NotAvailable) => {
                        warn!("Found stale socket without PID file, cleaning up");
                        let _ = ipc::cleanup_runtime();
                    }
                    Err(err) => {
                        warn!(
                            "Failed to query supervisor socket without PID file: {err}"
                        );
                    }
                }
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

/// Sends daemonized restart and recycles the supervisor when an old IPC schema rejects it.
fn restart_daemonized(
    command: ControlCommand,
    config_path: PathBuf,
    allow_recycle: bool,
) -> Result<(), Box<dyn Error>> {
    match ipc::send_command_with_timeout(&command, RESTART_DAEMON_ACK_TIMEOUT) {
        Ok(ipc::CommandAck::Pending) => Ok(()),
        Ok(ipc::CommandAck::Response(ControlResponse::Message(_))) => Ok(()),
        Ok(ipc::CommandAck::Response(ControlResponse::Ok)) => Ok(()),
        Ok(ipc::CommandAck::Response(ControlResponse::Error(message))) => {
            if allow_recycle && supervisor_error_is_protocol_mismatch(&message) {
                recycle_supervisor_for_restart(config_path)
            } else {
                Err(ControlError::Server(message).into())
            }
        }
        Ok(ipc::CommandAck::Response(other)) => Err(io::Error::other(format!(
            "unexpected supervisor response: {:?}",
            other
        ))
        .into()),
        Err(err) => {
            if allow_recycle && control_error_is_restart_upgrade_boundary(&err) {
                recycle_supervisor_for_restart(config_path)
            } else {
                Err(err.into())
            }
        }
    }
}

/// Stops the resident supervisor and starts a fresh daemon with the requested config.
fn recycle_supervisor_for_restart(config_path: PathBuf) -> Result<(), Box<dyn Error>> {
    warn!(
        "Restart IPC was rejected by the resident supervisor; recycling supervisor with config {:?}",
        config_path
    );
    stop_supervisors();
    let _ = ipc::cleanup_runtime();
    start_supervisor_daemon(config_path, None, false)
}

fn control_error_is_restart_upgrade_boundary(err: &ControlError) -> bool {
    match err {
        ControlError::Serde(_) | ControlError::NotAvailable => true,
        ControlError::Server(message) => supervisor_error_is_protocol_mismatch(message),
        ControlError::MissingHome | ControlError::Unauthorized(_) => false,
        ControlError::Io(err) => matches!(
            err.kind(),
            io::ErrorKind::UnexpectedEof
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::BrokenPipe
        ),
    }
}

fn supervisor_error_is_protocol_mismatch(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "serialise",
        "serialize",
        "deserialize",
        "invalid type",
        "missing field",
        "unknown field",
        "expected",
    ]
    .iter()
    .any(|needle| message.contains(needle))
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
