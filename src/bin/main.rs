use std::{
    collections::{HashMap, HashSet},
    error::Error,
    ffi::CString,
    fs, io,
    io::{IsTerminal, Write},
    os::unix::io::IntoRawFd,
    path::{Path, PathBuf},
    process,
    sync::{
        Arc,
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
use sysinfo::{
    Pid as SysPid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, Users,
};
use systemg::{
    charting::{self, ChartConfig, parse_stream_duration},
    cli::{Cli, Commands, OutputFormat, parse_args},
    config::{Config, EffectiveLogsConfig, load_config},
    constants::{PROCESS_CHECK_INTERVAL, SERVICE_POLL_INTERVAL},
    cron::{CronExecutionStatus, CronStateFile},
    daemon::{Daemon, ServiceLifecycleStatus},
    ipc::{self, ControlCommand, ControlError, ControlResponse, InspectPayload},
    logs::{
        LogFilter, LogFormat, LogManager, LogSection, LogWriter, RotatingLogWriter,
        get_service_log_path, prune_logs, resolve_log_path, supervisor_log_path,
        write_log_section_header,
    },
    metrics::MetricSample,
    runtime::{self, RuntimeMode},
    spawn::{SpawnedChild, SpawnedChildKind, SpawnedExit},
    state_store::StateStore,
    status::{
        CronUnitStatus, ExitMetadata, OverallHealth, ProcessState, ProjectRunMode,
        SpawnedProcessNode, StatusSnapshot, UnitHealth, UnitIntent, UnitKind,
        UnitMetricsSummary, UnitState, UnitStatus, UptimeInfo, collect_disk_snapshot,
        compute_overall_health, explain_unit_health, format_elapsed,
    },
    supervisor::{Supervisor, SupervisorError},
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
const BUSY_PROBE_EVERY_TICKS: usize = 12;
const RESTART_DAEMON_ACK_TIMEOUT: Duration = Duration::from_millis(250);
/// Maximum time allowed for a replacement supervisor to confirm its version.
const UPGRADE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound for each version probe during supervisor replacement.
const UPGRADE_PROBE_TIMEOUT: Duration = Duration::from_millis(300);
/// Delay between replacement supervisor version probes.
const UPGRADE_PROBE_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_CONFIG_PATH: &str = "systemg.yaml";
/// Maximum time allowed for the supervisor to acknowledge a shutdown request.
const SUPERVISOR_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum time allowed for a requested supervisor shutdown to finish cleanly.
const SUPERVISOR_GRACEFUL_EXIT_TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum wait after each forced supervisor signal.
const SUPERVISOR_FORCE_EXIT_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum time allowed for supervisor runtime files to disappear.
const SUPERVISOR_RUNTIME_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum time allowed for a new supervisor control socket to become usable.
const SUPERVISOR_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Interval between foreground attachment and reconnect checks.
const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Interval between visible boot progress probes.
const BOOT_PROGRESS_INTERVAL: Duration = Duration::from_millis(400);
/// Prefix used for foreground boot progress.
const FOREGROUND_PROGRESS_PREFIX: &str = "  • ";
/// Column left unused so a terminal cannot auto-wrap the progress line.
const TERMINAL_AUTOWRAP_MARGIN: usize = 1;
/// Error returned when another thread poisoned foreground output ownership.
const FOREGROUND_OUTPUT_LOCK_ERROR: &str = "foreground output lock is poisoned";
/// Number of initial follow retries per foreground backoff.
const FOREGROUND_START_RETRIES: usize = 2;
/// Number of reconnect retries per foreground backoff.
const FOREGROUND_RECONNECT_RETRIES: usize = 4;
/// Number of historical lines included by the first foreground attachment.
const FOREGROUND_BACKLOG_LINES: usize = 20;
/// Capacity of the single-result foreground boot channel.
const FOREGROUND_BOOT_RESULT_CAPACITY: usize = 1;
/// Number of service log lines inspected for a foreground port collision.
const PORT_DIAGNOSTIC_TAIL_LINES: usize = 12;
/// Thread name for terminal progress rendering.
const SPINNER_THREAD: &str = "sysg-spinner";
/// Thread name for interactive log streaming.
const LOG_STREAM_THREAD: &str = "sysg-log-stream";
/// Thread name for attached foreground project boots.
const FOREGROUND_BOOT_THREAD: &str = "sysg-foreground-boot";
/// Thread name for bounded supervisor health probes.
const SUPERVISOR_PROBE_THREAD: &str = "sysg-supervisor-probe";

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

        let poll_timeout = remaining.min(SERVICE_POLL_INTERVAL);
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

        let poll_timeout = remaining.min(SERVICE_POLL_INTERVAL);
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

thread_local! {
    /// The subcommand currently being dispatched, so the top-level catch-all can
    /// attach help for THAT command instead of a fixed `sysg logs` suggestion.
    static CURRENT_COMMAND: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
}

/// Records the command being run so `catchall_diag` can tailor its help.
fn set_current_command(command: &Commands) {
    CURRENT_COMMAND.with(|c| c.set(command.name()));
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

/// Renders one spinner frame as an IN-PLACE update.
///
/// The line MUST fit the terminal width. `\r` returns to the start of the
/// current row only — so once a frame wraps, every later frame repaints on the
/// new row and leaves the wrapped remainder stranded, turning a spinner that
/// should occupy one line into a wall of near-identical lines. Truncating to the
/// visible width is what keeps it a single updating line.
fn format_progress_spinner_frame(frame: &str, label: &str) -> String {
    let width = terminal_width();
    // frame + space, and leave a column spare so writing the final cell cannot
    // trigger the terminal's auto-wrap.
    let budget = width.saturating_sub(frame.chars().count() + 2);
    let shown: String = if label.chars().count() > budget {
        let keep = budget.saturating_sub(1);
        label
            .chars()
            .take(keep)
            .chain(std::iter::once('…'))
            .collect()
    } else {
        label.to_string()
    };
    format!("\r{frame} {shown}\x1B[K")
}

/// Usable terminal width.
///
/// A failed or nonsense probe must not shrink the line to a stub — an
/// unset/zero winsize (common under a bare pty, or when stdout is not the
/// controlling terminal) once truncated the spinner to 19 columns on a
/// 120-column terminal. Prefer the real size, fall back to `COLUMNS`, then to
/// a sane default, and never accept a width too small to say anything.
fn terminal_width() -> usize {
    use systemg::constants::{DEFAULT_TERMINAL_WIDTH, MIN_SPINNER_WIDTH};

    let probed = crossterm::terminal::size()
        .ok()
        .map(|(cols, _)| cols as usize)
        .filter(|cols| *cols >= MIN_SPINNER_WIDTH);
    let from_env = || {
        std::env::var("COLUMNS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|cols| *cols >= MIN_SPINNER_WIDTH)
    };
    probed.or_else(from_env).unwrap_or(DEFAULT_TERMINAL_WIDTH)
}

/// Returns the terminal sequence that clears the active progress row.
fn clear_progress_spinner_line() -> &'static str {
    "\r\x1B[2K\r"
}

/// Runs `operation` while rendering a delayed progress spinner on a terminal.
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
    let spinner =
        match thread::Builder::new()
            .name(SPINNER_THREAD.into())
            .spawn(move || {
                thread::sleep(FETCH_SPINNER_DELAY);
                if spinner_stop.load(Ordering::Relaxed) {
                    return;
                }

                let mut stderr = io::stderr().lock();
                let mut frame_idx = 0usize;
                let mut op_label: Option<String> = None;
                let mut ticks_since_probe = 0usize;
                loop {
                    if spinner_stop.load(Ordering::Relaxed) {
                        let _ = write!(stderr, "{}", clear_progress_spinner_line());
                        let _ = stderr.flush();
                        return;
                    }

                    if ticks_since_probe == 0 {
                        op_label = ipc::current_op().map(|op| op.describe());
                    }
                    ticks_since_probe = (ticks_since_probe + 1) % BUSY_PROBE_EVERY_TICKS;

                    let shown = match &op_label {
                        Some(detail) => format!("{label}: {detail}"),
                        None => label.to_string(),
                    };
                    let frame =
                        FETCH_SPINNER_FRAMES[frame_idx % FETCH_SPINNER_FRAMES.len()];
                    let _ = write!(
                        stderr,
                        "{}",
                        format_progress_spinner_frame(frame, &shown)
                    );
                    let _ = stderr.flush();
                    frame_idx += 1;
                    thread::sleep(FETCH_SPINNER_TICK);
                }
            }) {
            Ok(handle) => handle,
            Err(_) => return operation(),
        };

    let result = operation();
    stop.store(true, Ordering::Relaxed);
    let _ = spinner.join();
    if stderr_is_tty() {
        let mut stderr = io::stderr().lock();
        let _ = write!(stderr, "{}", clear_progress_spinner_line());
        let _ = stderr.flush();
    }
    result
}

/// Runs an operation under an in-place spinner, then prints its success message
/// below the cleared progress row.
fn with_progress_message<F>(
    label: &'static str,
    operation: F,
) -> Result<(), Box<dyn Error>>
where
    F: FnOnce() -> Result<String, Box<dyn Error>>,
{
    let message = with_progress_spinner(label, operation)?;
    println!("\n\n{message}");
    Ok(())
}

use std::fmt;

/// Carries a structured diagnostic up to `main` so it renders after any
/// progress spinner has released the stderr lock — rendering inside a spinner
/// closure deadlocks against the lock the spinner thread holds.
struct DiagError(Box<systemg::diag::Diagnostic>);

impl fmt::Display for DiagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.render(false))
    }
}

impl fmt::Debug for DiagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Error for DiagError {}

/// Wraps errors that never became a structured diagnostic, so every failure
/// leaves the user with next steps instead of a bare message.
fn catchall_diag(message: &str) -> systemg::diag::Diagnostic {
    if message.contains("Failed to read config") {
        return config_read_diag(message);
    }

    // The supervisor answers with a plain error string, so conditions that
    // ALREADY have a precise code were arriving here and being rendered as
    // SG0001 "command failed". Stopping an unknown project, or stopping one
    // twice, is a target problem (SG0202) — telling the user only that a
    // command "failed" hides both what went wrong and that nothing was broken.
    if message.contains("is not managed by this supervisor") {
        let project = message
            .split('\'')
            .nth(1)
            .map(str::to_string)
            .unwrap_or_else(|| "<project>".to_string());
        return systemg::diag::Diagnostic::error(
            systemg::diag::SgCode::TargetNotFound,
            format!("no project '{project}' is loaded in the running supervisor"),
        )
        .note("it was never started, or it has already been stopped")
        .help_cmd("list what is loaded", "sysg status")
        .help_cmd("start it", format!("sysg start -p {project}"))
        .help_docs();
    }

    let command = CURRENT_COMMAND.with(|c| c.get());
    let mut diag = systemg::diag::Diagnostic::error(
        systemg::diag::SgCode::Catchall,
        "command failed",
    )
    .note(message);
    // Point at the failing command's own help, not a fixed `sysg logs` — a
    // `status` error suggesting you check logs is nonsensical.
    if !command.is_empty() {
        diag = diag.help_cmd(format!("{command} help"), format!("sysg {command} --help"));
    }
    diag.help_docs()
}

/// holds each project's manifest, so the real fix is to target the project by
/// id rather than hunt for a file in the current directory.
fn config_read_diag(message: &str) -> systemg::diag::Diagnostic {
    let mut diag = systemg::diag::Diagnostic::error(
        systemg::diag::SgCode::ConfigFileUnreadable,
        "could not read a local config file",
    )
        .note(message)
        .note(
            "the resident supervisor keeps each project's config; you usually do not need a local file. \
Target the project by id with -p instead.",
        );

    if let Ok(Some(hint)) = ipc::read_config_hint() {
        diag = diag.note(format!(
            "the running supervisor last loaded: {}",
            hint.display()
        ));
    }

    diag.help_cmd("list what is loaded", "sysg status")
        .help_cmd("start a resident project", "sysg start -p <project>")
        .help_docs()
}

/// Runs the `sysg` command-line entrypoint.
fn main() -> process::ExitCode {
    let outcome = match run() {
        Ok(()) => process::ExitCode::SUCCESS,
        Err(err) => {
            if let Some(diag) = err.downcast_ref::<DiagError>() {
                eprintln!("{}", diag.0.render_for_terminal());
            } else {
                eprintln!("{}", catchall_diag(&err.to_string()).render_for_terminal());
            }
            process::ExitCode::FAILURE
        }
    };
    // Last line of defence: whatever path got us here, the user gets their
    // terminal back in a usable state.
    let _ = crossterm::terminal::disable_raw_mode();
    outcome
}

/// Dispatches the parsed CLI command.
fn run() -> Result<(), Box<dyn Error>> {
    let args = parse_args();
    set_current_command(&args.command);
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

    // A `start` (daemonized OR foreground) becomes/feeds a supervisor whose
    // internal tracing must NOT spray onto the user's terminal — the foreground
    // terminal is for the project's OWN service logs (via the scoped follow),
    // never `DEBUG systemg::supervisor:` noise. Route all of them to the log file.
    let use_file_logging = matches!(
        &args.command,
        Commands::Start { .. }
            | Commands::Restart {
                daemonize: true,
                ..
            }
            | Commands::Supervise { .. }
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

    let verbose = args.verbose;
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
                resolve_start_target(&config, service.clone(), name.as_deref(), command)?;
            let plan = systemg::start::resolve_plan(
                start_target.config_path.clone(),
                start_target.service.as_deref(),
                project.as_deref(),
                start_target.ad_hoc,
            )
            .map_err(|mismatch| {
                DiagError(Box::new(systemg::start::project_mismatch(
                    &mismatch.flag,
                    &mismatch.selector,
                )))
            })?;

            if daemonize {
                dispatch_start_daemonize(plan, stderr, verbose, args.drop_privileges)?;
            } else {
                dispatch_start_foreground(plan, stderr)?;
            }
        }
        Commands::Stop {
            service,
            project,
            config,
            supervisor,
        } => {
            let config_path =
                resolve_config_path(&config).unwrap_or_else(|_| config.into());
            let plan = systemg::stop::resolve_plan(
                config_path,
                service.as_deref(),
                project.as_deref(),
                supervisor,
            )
            .map_err(stop_plan_diag)?;
            dispatch_stop(plan)?;
        }
        Commands::Restart {
            config,
            service,
            project,
            daemonize,
        } => {
            if args.drop_privileges && supervisor_running() {
                warn!(
                    "--drop-privileges is managed by the running supervisor and has no effect for this restart request"
                );
            }
            let config_path =
                resolve_config_path(&config).unwrap_or_else(|_| config.clone().into());
            let plan = systemg::restart::resolve_plan(
                config_path,
                service.as_deref(),
                project.as_deref(),
            )
            .map_err(|mismatch| {
                DiagError(Box::new(systemg::start::project_mismatch(
                    &mismatch.flag,
                    &mismatch.selector,
                )))
            })?;

            let world = systemg::restart::World {
                supervisor_running: supervisor_running(),
                version_drifted: matches!(
                    daemon_version_drift(),
                    VersionDrift::Drifted(_) | VersionDrift::PreVersionDaemon
                ),
            };
            match systemg::restart::preflight(plan, world) {
                systemg::restart::Preflight::Refused(diag) => {
                    return Err(Box::new(DiagError(diag)));
                }
                systemg::restart::Preflight::Ready(plan) => {
                    dispatch_restart(plan, daemonize, verbose)?;
                }
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

            let mut render_opts = StatusRenderOptions {
                format,
                no_color: no_color || agent_mode(),
                full_cmd,
                include_orphans: all,
                service_filter: service.as_deref(),
                project_filter: target_project.as_deref(),
                offline: false,
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
                    match fetch_status_reading(config.as_deref(), live) {
                        Ok(reading) => {
                            print!("\x1B[2J\x1B[H");
                            print_presence_banner(reading.presence);
                            render_opts.offline =
                                reading.presence != SupervisorPresence::Live;
                            if let Err(e) = render_status(
                                &reading.snapshot,
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
                let reading = with_progress_spinner("Computing", || {
                    fetch_status_reading(config.as_deref(), live)
                })?;

                if let Some(diag) = status_ambiguous_service(
                    &reading.snapshot,
                    service.as_deref(),
                    target_project.as_deref(),
                ) {
                    eprintln!("{}", diag.render_for_terminal());
                    process::exit(2);
                }

                print_presence_banner(reading.presence);
                render_opts.offline = reading.presence != SupervisorPresence::Live;
                let health =
                    render_status(&reading.snapshot, &render_opts, false, render_config)?;

                let exit_code = status_exit_code(reading.presence, health);
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
            if let Err(err) =
                systemg::inspect::resolve_plan(&service, project.as_deref(), live)
            {
                use systemg::inspect::InspectPlanError;
                let diag = match err {
                    InspectPlanError::Mismatch(mismatch) => {
                        systemg::start::project_mismatch(
                            &mismatch.flag,
                            &mismatch.selector,
                        )
                    }
                    InspectPlanError::NotAService => {
                        systemg::inspect::service_not_found(&service)
                    }
                };
                return Err(Box::new(DiagError(Box::new(diag))));
            }
            let target_project = resolve_command_project(
                &effective_config,
                project.clone(),
                Some(&service),
            )?;

            // A bare `-s web` that matches a `web` in more than one loaded
            // project is ambiguous — refuse with SG0006 rather than silently
            // inspecting whichever one resolves first, exactly as status does.
            if project.is_none()
                && let Ok(snapshot) = fetch_status_snapshot(Some(&effective_config), live)
                && let Some(diag) =
                    status_ambiguous_service(&snapshot, Some(&service), None)
            {
                return Err(Box::new(DiagError(Box::new(diag))));
            }

            let stream_seconds = match stream.as_deref() {
                Some(value) => match parse_stream_duration(value) {
                    Ok(seconds) => seconds,
                    Err(err) => {
                        return Err(Box::new(DiagError(Box::new(
                            systemg::inspect::invalid_stream_duration(
                                value,
                                err.to_string(),
                            ),
                        ))));
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
                                let _ = terminal::disable_raw_mode();
                                eprint!(
                                    "{}",
                                    systemg::inspect::service_not_found(&service)
                                        .render_for_terminal()
                                );
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
                    return Err(Box::new(DiagError(Box::new(
                        systemg::inspect::service_not_found(&service),
                    ))));
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
            supervisor,
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
            let logs_modes = systemg::logs_cmd::Modes {
                path,
                purge,
                prune,
                follow,
                supervisor,
            };
            let logs_plan = match systemg::logs_cmd::resolve_plan(
                logs_modes,
                service.as_deref(),
                project.as_deref(),
                max_size.clone(),
                max_age.clone(),
            ) {
                Ok(plan) => plan,
                Err(err) => {
                    use systemg::logs_cmd::LogsPlanError;
                    let diag = match err {
                        LogsPlanError::ConflictingModes { modes } => {
                            systemg::logs_cmd::conflicting_modes(&modes)
                        }
                        LogsPlanError::FollowWithMode { mode } => {
                            systemg::logs_cmd::follow_with_mode(mode)
                        }
                        LogsPlanError::PruneBoundMissing => {
                            systemg::logs_cmd::prune_bound_missing()
                        }
                        LogsPlanError::SupervisorWithSelector => {
                            systemg::logs_cmd::supervisor_with_selector()
                        }
                        LogsPlanError::TargetRequired => {
                            systemg::logs_cmd::target_required()
                        }
                        LogsPlanError::Mismatch(mismatch) => {
                            systemg::start::project_mismatch(
                                &mismatch.flag,
                                &mismatch.selector,
                            )
                        }
                    };
                    return Err(Box::new(DiagError(Box::new(diag))));
                }
            };

            if matches!(logs_plan, systemg::logs_cmd::LogsPlan::Supervisor) {
                LogManager::new().show_supervisor_log(lines)?;
                return Ok(());
            }
            if prune {
                if max_size.is_none() && max_age.is_none() {
                    return Err(Box::new(DiagError(Box::new(
                        systemg::logs_cmd::prune_bound_missing(),
                    ))));
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

            let log_project = target_project.clone().or_else(|| {
                load_config(Some(&effective_config))
                    .ok()
                    .map(|c| c.project.id)
            });
            // The project subdir captured logs live under. A bare `-s` with no
            // resolvable project reads the loose bundle, mirroring the plan's
            // __loose__-only rule.
            let log_project_id = log_project
                .clone()
                .unwrap_or_else(|| systemg::state_store::LOOSE_PROJECT_ID.to_string());

            // A bare `-s <service>` (no -p, no `project/` prefix, no resolvable
            // project) reads the loose bundle only. If that service has no loose
            // log at all, it is not a loose service — refuse with SG0021 rather
            // than silently reading an unrelated project's logs.
            if let Some(service_name) = service.as_deref()
                && project.is_none()
                && !service_name.contains('/')
                && log_project_id == systemg::state_store::LOOSE_PROJECT_ID
                && !purge
                && !path
            {
                let loose = systemg::state_store::LOOSE_PROJECT_ID;
                let has_loose_log = get_service_log_path(loose, service_name).exists()
                    || resolve_log_path(loose, service_name, "stdout").exists()
                    || resolve_log_path(loose, service_name, "stderr").exists();
                if !has_loose_log {
                    return Err(Box::new(DiagError(Box::new(
                        systemg::logs_cmd::loose_service_not_found(service_name),
                    ))));
                }
            }

            // A project-scoped `-p <project> -s <service>` miss is a plain
            // target-not-found (SG0202), reusing the shared stop diagnostic.
            if let Some(service_name) = service.as_deref()
                && log_project_id != systemg::state_store::LOOSE_PROJECT_ID
                && !service_name.contains('/')
                && !purge
                && !path
            {
                let bare = service_selector_name(service_name);
                let scoped_log = get_service_log_path(&log_project_id, bare).exists()
                    || resolve_log_path(&log_project_id, bare, "stdout").exists()
                    || resolve_log_path(&log_project_id, bare, "stderr").exists();
                if !scoped_log {
                    return Err(Box::new(DiagError(Box::new(
                        systemg::stop::service_not_found(bare),
                    ))));
                }
            }

            let manager = LogManager::new();

            if path {
                match service.as_deref() {
                    Some(service_name) => {
                        let active = get_service_log_path(&log_project_id, service_name);
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

            if purge {
                // A serving supervisor owns the in-memory live-log buffer the
                // reader replays from, so clearing files CLI-side would leave it
                // showing "purged" lines. Route the clear through the supervisor
                // when one is up; fall back to a local file clear when it is not.
                if supervisor_running() {
                    match ipc::send_command(&ControlCommand::ClearLogs {
                        service: service.clone(),
                        project: target_project.clone(),
                    }) {
                        Ok(ControlResponse::Message(message)) => {
                            println!("{message}");
                            return Ok(());
                        }
                        Ok(ControlResponse::Ok) => return Ok(()),
                        Ok(ControlResponse::Error(message)) => {
                            return Err(ControlError::Server(message).into());
                        }
                        Ok(other) => {
                            return Err(io::Error::other(format!(
                                "unexpected supervisor response: {other:?}"
                            ))
                            .into());
                        }
                        Err(ControlError::NotAvailable) => {}
                        Err(err) => return Err(err.into()),
                    }
                }
                match service.as_deref() {
                    Some(service_name) => {
                        info!("Purging logs for service: {service_name}");
                        manager.clear_service_logs(&log_project_id, service_name)?;
                    }
                    None => {
                        info!("Purging logs for all services");
                        manager.clear_all_logs()?;
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
                    return Err(Box::new(DiagError(Box::new(
                        systemg::logs_cmd::unsupported_format("xml"),
                    ))));
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
                // The snapshot only ENRICHES the render (it maps a service to its
                // project and marks live units). Logs live on disk and stay
                // readable with no supervisor and no local manifest — which is
                // exactly the post-mortem case — so a snapshot that cannot be
                // built must degrade to an empty one, never sink the command.
                let snapshot = with_progress_spinner("Logging", || {
                    fetch_status_snapshot(Some(&effective_config), false)
                })
                .unwrap_or_else(|_| StatusSnapshot {
                    schema_version: systemg::status::STATUS_SCHEMA_VERSION.to_string(),
                    captured_at: chrono::Utc::now(),
                    overall_health: systemg::status::OverallHealth::Warn,
                    units: Vec::new(),
                });

                match service.as_ref() {
                    Some(service_name) if structured_output => {
                        info!("Fetching logs for service: {service_name}");
                        let service_project = snapshot
                            .units
                            .iter()
                            .find(|unit| {
                                status_unit_matches_selector(
                                    unit,
                                    Some(service_name),
                                    target_project.as_deref(),
                                )
                            })
                            .and_then(|unit| {
                                unit.project.as_ref().map(|project| project.id.clone())
                            })
                            .unwrap_or_else(|| log_project_id.clone());
                        let bytes = manager.collect_service_log(
                            &service_project,
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
                                // `--stream` is a REPEATING render by design, so
                                // redrawing the grouped snapshot is correct here
                                // (unlike the follow path, which must tail).
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
                // A follow in a real terminal must exit on Esc/Ctrl-C. The socket
                // stream blocks, so run it on a background thread and poll keys on
                // the main thread; a key press restores the terminal and returns.
                let follow_tty = follow_logs
                    && unsafe {
                        libc::isatty(libc::STDIN_FILENO) == 1
                            && libc::isatty(libc::STDOUT_FILENO) == 1
                    };
                if follow_tty {
                    let stream_cmd = ControlCommand::Logs {
                        service: service.clone(),
                        project: target_project.clone(),
                        lines,
                        kind: kind.as_ref().map(|kind| kind.as_str().to_string()),
                        follow: true,
                        since: since.clone(),
                        until: until.clone(),
                        grep: grep.clone(),
                        all,
                        structured: structured_output,
                    };
                    let log_format_owned = log_format;
                    let strip_ansi_owned = strip_ansi_output;
                    let service_owned = service.clone();
                    let stream_thread = thread::Builder::new()
                        .name(LOG_STREAM_THREAD.into())
                        .spawn(move || {
                            let mut writer = LogWriter::new(
                                io::stdout(),
                                log_format_owned,
                                strip_ansi_owned,
                                service_owned,
                            );
                            let outcome =
                                ipc::stream_command_output(&stream_cmd, &mut writer);
                            let _ = writer.flush();
                            outcome
                        })?;

                    terminal::enable_raw_mode()?;
                    let key_result = (|| -> Result<(), Box<dyn Error>> {
                        loop {
                            if stream_thread.is_finished() {
                                return Ok(());
                            }
                            if event::poll(PROCESS_CHECK_INTERVAL)?
                                && matches!(
                                    logs_stream_event_action(event::read()?),
                                    Some(LogsStreamAction::Exit)
                                )
                            {
                                return Ok(());
                            }
                        }
                    })();
                    terminal::disable_raw_mode()?;
                    key_result?;

                    // If the stream ended on its own (not a key press), surface a
                    // NotAvailable fallback the same way the blocking path does.
                    // NEVER in follow mode: see `render_logs_once` below.
                    if !follow_logs
                        && stream_thread.is_finished()
                        && let Ok(Err(ControlError::NotAvailable)) = stream_thread.join()
                    {
                        render_logs_once(true)?;
                    }
                } else {
                    match stream_logs_via_supervisor(follow_logs) {
                        Ok(()) => {}
                        Err(err) => match err.downcast_ref::<ControlError>() {
                            Some(ControlError::NotAvailable) if !follow_logs => {
                                render_logs_once(true)?
                            }
                            // A follow that loses the supervisor must stay a
                            // follow: the caller's retry loop reconnects. Falling
                            // back to a static render replayed the whole grouped
                            // snapshot — "Offline Services" and all the finished
                            // one-shot history — into a LIVE terminal, once per
                            // reconnect (measured: 32 replays).
                            Some(ControlError::NotAvailable) => {}
                            _ => return Err(err),
                        },
                    }
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
        Commands::Migrate { config, in_place } => {
            let content = fs::read_to_string(&config)
                .map_err(|e| io::Error::other(format!("failed to read {config}: {e}")))?;
            let converted = systemg::config::migrate_manifest(&content)?;
            if in_place {
                let backup = format!("{config}.bak");
                fs::write(&backup, &content).map_err(|e| {
                    io::Error::other(format!("failed to write backup {backup}: {e}"))
                })?;
                fs::write(&config, &converted).map_err(|e| {
                    io::Error::other(format!("failed to write {config}: {e}"))
                })?;
                println!("Migrated {config} (backup at {backup})");
            } else {
                print!("{converted}");
            }
        }
        Commands::Purge {
            config,
            project,
            force,
        } => {
            dispatch_purge(config, project, force)?;
        }
        Commands::UpgradeInfo => {
            println!(
                "{}",
                serde_json::to_string(&systemg::upgrade::LiveUpgradeInfo::current())?
            );
        }
        Commands::UpgradeSupervisor { binary } => {
            with_progress_message("Updating", || request_live_upgrade(binary))?;
        }
        Commands::Supervise {
            config,
            service,
            pipe_stderr,
            verbose: _,
            foreground,
            handoff,
        } => {
            let mode = if foreground {
                ProjectRunMode::Foreground
            } else {
                ProjectRunMode::Daemon
            };
            run_supervisor_in_process(
                PathBuf::from(config),
                service,
                pipe_stderr,
                mode,
                handoff.map(PathBuf::from),
            );
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

        let lines =
            status_overview_lines(&columns, &units, OverallHealth::Warn, true, false);
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

        let offline =
            status_overview_lines(&columns, &units, OverallHealth::Warn, true, true)
                .join("\n");
        assert!(offline.contains("Status: OFFLINE"));
        assert!(!offline.contains("Status: WARN"));
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
            fs::write(path, "version: \"2\"\nservices: {}\n").expect("write unit file");
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
        fs::write(&path, "version: \"2\"\nservices: {}\n").expect("write unit file");

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

/// Resolves the purge selectors into a plan, runs preflight, and — if cleared —
/// deletes the targeted state.
fn dispatch_purge(
    config: Option<String>,
    project: Option<String>,
    force: bool,
) -> Result<(), Box<dyn Error>> {
    let config_projects = match (&config, &project) {
        (Some(path), None) => Some(purge_config_project_ids(path)?),
        _ => None,
    };

    let plan =
        match systemg::purge::resolve_plan(None, project.as_deref(), config_projects) {
            Ok(plan) => plan,
            Err(mismatch) => {
                return Err(Box::new(DiagError(Box::new(
                    systemg::start::project_mismatch(&mismatch.flag, &mismatch.selector),
                ))));
            }
        };

    let world = purge_world(force);
    let plan = match systemg::purge::preflight(plan, world) {
        systemg::purge::Preflight::Ready(plan) => plan,
        systemg::purge::Preflight::Refused(diag) => {
            return Err(Box::new(DiagError(diag)));
        }
    };

    if force {
        stop_supervisors()?;
        wait_for_runtime_cleared(SUPERVISOR_RUNTIME_TIMEOUT);
    }

    execute_purge(plan)
}

/// A live snapshot of the world for the purge preflight.
fn purge_world(force: bool) -> systemg::purge::World {
    match supervisor_health() {
        SupervisorHealth::Serving => {
            let managed_units = try_live_status(true)
                .map(|reading| {
                    reading
                        .snapshot
                        .units
                        .iter()
                        .filter(|u| u.process.is_some())
                        .count()
                })
                .unwrap_or(0);
            systemg::purge::World {
                supervisor_serving: true,
                managed_units,
                force,
            }
        }
        SupervisorHealth::Dying => systemg::purge::World {
            supervisor_serving: true,
            managed_units: 1,
            force,
        },
        SupervisorHealth::Down => {
            let managed_units = tracked_unit_count();
            systemg::purge::World {
                supervisor_serving: managed_units > 0,
                managed_units,
                force,
            }
        }
    }
}

fn tracked_unit_count() -> usize {
    let root = runtime::state_dir().join(systemg::state_store::PROJECTS_DIR);
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let store = StateStore::at(entry.path());
            match systemg::daemon::PidFile::load(store.clone()) {
                Ok(pid_file) => pid_file
                    .services()
                    .keys()
                    .chain(pid_file.service_pgids().keys())
                    .collect::<HashSet<_>>()
                    .len(),
                Err(_) if store.pid_path().exists() => 1,
                Err(_) => 0,
            }
        })
        .sum()
}

/// Reads a config and returns the project ids it declares (or `__loose__`).
fn purge_config_project_ids(path: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let content = fs::read_to_string(path).map_err(|err| -> Box<dyn Error> {
        Box::new(DiagError(Box::new(config_read_diag(&err.to_string()))))
    })?;
    let configs = systemg::config::parse_config_projects(&content)?;
    Ok(configs.into_iter().map(|c| c.project.id).collect())
}

/// Deletes the state a cleared [`PurgePlan`] targets.
fn execute_purge(plan: systemg::purge::PurgePlan) -> Result<(), Box<dyn Error>> {
    use systemg::purge::PurgePlan;
    match plan {
        PurgePlan::Everything => {
            purge_state_root()?;
            println!("All systemg state has been purged");
        }
        PurgePlan::Config { projects } => {
            purge_projects(&projects)?;
            purge_runtime_files();
            println!("Purged state for {} project(s)", projects.len());
        }
        PurgePlan::Project { project } => {
            let dir = runtime::state_dir()
                .join(systemg::state_store::PROJECTS_DIR)
                .join(&project);
            if !dir.exists() {
                return Err(Box::new(DiagError(Box::new(
                    systemg::purge::project_not_found(&project),
                ))));
            }
            remove_tree(&dir)?;
            println!("Purged state for project '{project}'");
        }
    }
    Ok(())
}

/// Removes the whole state root plus the out-of-root system-mode log dir.
fn purge_state_root() -> Result<(), Box<dyn Error>> {
    let runtime_dir = runtime::state_dir();
    if runtime_dir.exists() {
        info!("Removing systemg runtime directory: {:?}", runtime_dir);
        remove_tree(&runtime_dir)?;
    }
    // In system mode logs live outside the state root; take them too.
    let log_dir = runtime::log_dir();
    if log_dir.exists() && !log_dir.starts_with(&runtime_dir) {
        remove_tree(&log_dir)?;
    }
    Ok(())
}

/// Removes each named project's state directory.
fn purge_projects(projects: &[String]) -> Result<(), Box<dyn Error>> {
    let root = runtime::state_dir().join(systemg::state_store::PROJECTS_DIR);
    for project in projects {
        let dir = root.join(project);
        if dir.exists() {
            remove_tree(&dir)?;
        }
    }
    Ok(())
}

/// Removes the supervisor-level runtime files (socket, pid, config hint).
fn purge_runtime_files() {
    cleanup_stopped_runtime();
}

/// Removes a directory tree, mapping any IO error to the partial-purge SG0402.
fn remove_tree(dir: &Path) -> Result<(), Box<dyn Error>> {
    fs::remove_dir_all(dir).map_err(|err| -> Box<dyn Error> {
        Box::new(DiagError(Box::new(systemg::purge::incomplete(format!(
            "failed to remove {}: {err}",
            dir.display()
        )))))
    })
}

/// Stops supervisors.
fn stop_supervisors() -> Result<(), Box<dyn Error>> {
    let candidates = gather_supervisor_pids();

    if candidates.is_empty() {
        stop_tracked_projects()?;
        cleanup_stopped_runtime();
        return Ok(());
    }

    let mut survivors = HashSet::new();
    let mut fallback_targets = HashSet::new();

    if supervisor_running() {
        match ipc::send_command_with_timeout(
            &ControlCommand::Shutdown,
            SUPERVISOR_REQUEST_TIMEOUT,
        ) {
            Ok(ipc::CommandAck::Response(_)) => {
                for pid in &candidates {
                    if !wait_for_supervisor_exit(*pid, SUPERVISOR_GRACEFUL_EXIT_TIMEOUT) {
                        fallback_targets.insert(*pid);
                    }
                }
            }
            Ok(ipc::CommandAck::Pending) => fallback_targets.extend(&candidates),
            Err(err) => {
                match err {
                    ControlError::NotAvailable => warn!(
                        "Supervisor IPC unavailable during shutdown; falling back to its recorded identity"
                    ),
                    other => warn!("Failed to request supervisor shutdown: {other}"),
                }
                fallback_targets.extend(&candidates);
            }
        }
    } else {
        fallback_targets.extend(&candidates);
    }

    survivors.extend(fallback_targets);

    if survivors.is_empty() {
        stop_tracked_projects()?;
        cleanup_stopped_runtime();
        return Ok(());
    }

    for pid in gather_supervisor_pids() {
        survivors.insert(pid);
    }

    let mut first_error: Option<Box<dyn Error>> = None;
    for pid in survivors {
        if let Err(err) = force_kill(pid) {
            first_error.get_or_insert_with(|| Box::new(err));
        }
    }
    if let Err(err) = stop_tracked_projects() {
        first_error.get_or_insert_with(|| Box::new(err));
    }
    cleanup_stopped_runtime();
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

fn cleanup_stopped_runtime() {
    if let Ok(_lock) = ipc::lock_supervisor_runtime() {
        let _ = ipc::cleanup_runtime();
    }
}

fn stop_tracked_projects() -> Result<(), systemg::error::ProcessManagerError> {
    let root = runtime::state_dir().join(systemg::state_store::PROJECTS_DIR);
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(());
    };
    let mut first_error = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Err(err) = Daemon::stop_tracked(StateStore::at(path.clone())) {
            warn!(
                "Failed to stop every tracked process in {}: {err}",
                path.display()
            );
            first_error.get_or_insert(err);
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Returns the verified PID of the supervisor that owns the current runtime.
///
/// The control-socket peer is authoritative. A recorded PID is accepted only
/// when it still names a session-leading `sysg` supervisor process.
fn gather_supervisor_pids() -> HashSet<libc::pid_t> {
    let mut system = System::new();
    let mut set = HashSet::new();
    if let Some(pid) = verified_supervisor_pid() {
        set.insert(pid);
        return set;
    }
    if let Ok(Some(pid)) = ipc::read_supervisor_pid() {
        let target = SysPid::from_u32(pid as u32);
        system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
        let session_leader = unsafe { libc::getsid(pid) } == pid;
        if session_leader && system.process(target).is_some_and(is_supervisor) {
            set.insert(pid);
        } else {
            warn!(
                "Refusing to signal recorded supervisor PID {pid}: it is not a sysg supervisor"
            );
        }
    }
    set
}

/// Returns whether a process command line identifies a `sysg` supervisor.
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
    let mut is_reexec_supervise = false;

    for arg in cmd {
        let value = arg.to_string_lossy();
        if value == "start" || value == "restart" {
            has_supervisor_command = true;
        } else if value == "supervise" {
            is_reexec_supervise = true;
        }
    }

    is_reexec_supervise || has_supervisor_command
}

/// Waits up to `timeout` for `pid` to disappear or become a zombie.
fn wait_for_supervisor_exit(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let target = Pid::from_raw(pid);

    while Instant::now() < deadline {
        match signal::kill(target, None) {
            Ok(_) => {
                if process_exited(pid) {
                    return true;
                }
                thread::sleep(PROCESS_CHECK_INTERVAL);
            }
            Err(err) => {
                if err == nix::Error::from(nix::errno::Errno::ESRCH) {
                    return true;
                }
                if process_exited(pid) {
                    return true;
                }
                thread::sleep(PROCESS_CHECK_INTERVAL);
            }
        }
    }

    false
}

/// Blocks until the supervisor runtime files (socket + pid) are gone, or the
/// deadline passes.
///
/// A stopped daemon tears its runtime down asynchronously — service stop, monitor
/// join, then `cleanup_runtime` — so `stop_supervisors` returning (process dead)
/// does not mean the socket and pid file are unlinked yet. Recycling must wait
/// for that teardown to finish before forking a successor, otherwise the old
/// daemon's late cleanup races the new daemon's fresh files.
fn wait_for_runtime_cleared(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let socket_gone = ipc::socket_path().map(|p| !p.exists()).unwrap_or(true);
        let pid_gone = ipc::supervisor_pid_path()
            .map(|p| !p.exists())
            .unwrap_or(true);
        if socket_gone && pid_gone {
            return;
        }
        thread::sleep(SERVICE_POLL_INTERVAL);
    }
}

/// Terminates a verified supervisor and confirms that the process exited.
fn force_kill(pid: libc::pid_t) -> io::Result<()> {
    if wait_for_supervisor_exit(pid, PROCESS_CHECK_INTERVAL) {
        return Ok(());
    }

    let verified_peer = verified_supervisor_pid() == Some(pid);
    let verified_process = if verified_peer {
        true
    } else {
        let target = SysPid::from_u32(pid as u32);
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
        (unsafe { libc::getsid(pid) }) == pid
            && system.process(target).is_some_and(is_supervisor)
    };
    if !verified_process {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing to force-kill unverified supervisor PID {pid}"),
        ));
    }

    let pgid = unsafe { libc::getpgid(pid) };

    if pgid >= 0 && pgid == pid {
        unsafe { libc::killpg(pgid, SIGTERM) };
    } else {
        unsafe { libc::kill(pid, SIGTERM) };
    }

    if wait_for_supervisor_exit(pid, SUPERVISOR_FORCE_EXIT_TIMEOUT) {
        return Ok(());
    }

    if pgid >= 0 && pgid == pid {
        unsafe { libc::killpg(pgid, SIGKILL) };
    } else {
        unsafe { libc::kill(pid, SIGKILL) };
    }

    if wait_for_supervisor_exit(pid, SUPERVISOR_FORCE_EXIT_TIMEOUT) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("supervisor PID {pid} survived SIGKILL"),
        ))
    }
}

/// Returns whether a process is absent, dead, or waiting to be reaped.
fn process_exited(pid: libc::pid_t) -> bool {
    let target = SysPid::from_u32(pid as u32);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
    system.process(target).is_none_or(|process| {
        matches!(
            process.status(),
            ProcessStatus::Dead | ProcessStatus::Zombie
        )
    })
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
    let attach_config = config_path.clone();
    let attach_service = service.clone();

    let child_pid = unsafe { libc::fork() };
    if child_pid < 0 {
        return Err(io::Error::last_os_error().into());
    }

    if child_pid == 0 {
        unsafe {
            libc::setsid();
        }
        silence_stdio()?;
        reexec_supervisor(
            &config_path,
            service.as_deref(),
            pipe_stderr,
            false,
            ProjectRunMode::Foreground,
        );
        run_supervisor_in_process(
            config_path,
            service,
            pipe_stderr,
            ProjectRunMode::Foreground,
            None,
        );
    }

    if let Err(err) =
        with_progress_spinner("Starting", || wait_for_supervisor_ready(child_pid))
    {
        unsafe {
            libc::waitpid(child_pid, std::ptr::null_mut(), libc::WNOHANG);
        }
        if supervisor_running() {
            return start_foreground_attached(attach_config, attach_service);
        }
        return Err(err);
    }

    println!("Starting project '{project_id}' in the foreground…");
    println!("Streaming its services' output; press Ctrl-C to stop the project.");
    let output = ForegroundOutput::new();
    let boot_progress =
        match spawn_boot_progress_reporter(project_id.clone(), output.clone()) {
            Ok(progress) => progress,
            Err(err) => {
                let _ = ipc::send_command(&ControlCommand::StopProject {
                    project: project_id,
                });
                return Err(err.into());
            }
        };

    // Stream ONLY this project's service logs to the terminal via a scoped follow
    // (the supervisor's own tracing goes to the log file, and service log writers
    // no longer echo to the terminal), so a foreground start shows its project's
    // output cleanly instead of every managed service + supervisor internals.
    let streaming = Arc::new(AtomicBool::new(true));
    let shutdown = Arc::new(std::sync::Mutex::new(None));
    let follow_handle = match spawn_foreground_log_follow(
        project_id.clone(),
        streaming.clone(),
        shutdown.clone(),
        output,
    ) {
        Ok(handle) => handle,
        Err(err) => {
            boot_progress.stop();
            let _ = ipc::send_command(&ControlCommand::StopProject {
                project: project_id,
            });
            return Err(err.into());
        }
    };

    let boot_report = collect_boot_report(false);
    boot_progress.stop();
    let boot_report = match boot_report {
        Ok(report) => report,
        Err(err) => {
            stop_foreground_follow(&streaming, &shutdown);
            let _ = ipc::send_command(&ControlCommand::StopProject {
                project: project_id,
            });
            let _ = follow_handle.join();
            return Err(err);
        }
    };
    if let Some(diag) = boot_report.failures.into_iter().next() {
        stop_foreground_follow(&streaming, &shutdown);
        let _ = ipc::send_command(&ControlCommand::StopProject {
            project: project_id,
        });
        let _ = follow_handle.join();
        return Err(Box::new(DiagError(Box::new(diag))));
    }

    let result =
        wait_for_foreground_attachment(project_id, streaming.clone(), shutdown.clone());
    streaming.store(false, Ordering::SeqCst);
    let _ = follow_handle.join();
    result
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
    let ctrlc = foreground_ctrlc()?;
    // NO spinner here. A foreground start's whole purpose is to show the
    // project's output, and the fork path (`start_foreground`) prints its header
    // and streams straight away. Wrapping the attach in a spinner made the two
    // paths look completely different for the same command: the first project
    // started streaming, while a second project attaching to that supervisor sat
    // on `⠹ Starting` — the boot runs on a background thread, so the spinner had
    // nothing to report and only hid the stream that followed.
    // Say what is happening BEFORE the boot begins. A slow project (one whose
    // pre_start waits on a DB or tunnel) can take a minute to produce its first
    // line, and a bare spinner left the user unable to tell working from wedged.
    println!("Starting project '{project_id}' in the foreground…");
    println!("Streaming its services' output; press Ctrl-C to stop the project.");

    let streaming = Arc::new(AtomicBool::new(true));
    let shutdown = Arc::new(std::sync::Mutex::new(None));
    let output = ForegroundOutput::new();
    let follow_handle = spawn_foreground_log_follow(
        project_id.clone(),
        streaming.clone(),
        shutdown.clone(),
        output.clone(),
    )?;

    let boot_progress = match spawn_boot_progress_reporter(project_id.clone(), output) {
        Ok(progress) => progress,
        Err(err) => {
            stop_foreground_follow(&streaming, &shutdown);
            let _ = follow_handle.join();
            return Err(err.into());
        }
    };
    let (boot_tx, boot_rx) = mpsc::sync_channel(FOREGROUND_BOOT_RESULT_CAPACITY);
    let boot_handle = match thread::Builder::new()
        .name(FOREGROUND_BOOT_THREAD.into())
        .spawn(move || {
            let result =
                send_control_command_inner(command, false).map_err(|err| err.to_string());
            let _ = boot_tx.send(result);
        }) {
        Ok(handle) => handle,
        Err(err) => {
            boot_progress.stop();
            stop_foreground_follow(&streaming, &shutdown);
            let _ = follow_handle.join();
            return Err(err.into());
        }
    };
    let boot_result = loop {
        match boot_rx.recv_timeout(PROCESS_CHECK_INTERVAL) {
            Ok(result) => break result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break Err("foreground boot worker stopped".to_string());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if ctrlc.try_recv().is_ok() {
            stop_foreground_follow(&streaming, &shutdown);
            let stop_result = stop_foreground_project(&project_id);
            let _ = boot_handle.join();
            boot_progress.stop();
            let _ = follow_handle.join();
            return stop_result;
        }
    };
    let _ = boot_handle.join();
    boot_progress.stop();
    if let Err(err) = boot_result {
        stop_foreground_follow(&streaming, &shutdown);
        let _ = follow_handle.join();
        return Err(io::Error::other(err).into());
    }

    let result = wait_for_foreground_attachment_with_ctrlc(
        project_id,
        streaming.clone(),
        shutdown.clone(),
        ctrlc,
    );
    // The wait already cleared the flag and force-closed the stream on its exit
    // path; join the (now unblocked) follow thread.
    streaming.store(false, Ordering::SeqCst);
    let _ = follow_handle.join();
    result
}

/// A running boot-progress reporter; call `stop()` to end it.
struct BootProgressReporter {
    /// Shared foreground terminal whose active progress row must be cleared.
    output: ForegroundOutput,
    /// Stop signal observed by the progress thread.
    running: Arc<AtomicBool>,
    /// Reporter thread joined during orderly shutdown.
    handle: Option<thread::JoinHandle<()>>,
}

impl BootProgressReporter {
    /// Stops the reporter and waits for its final line.
    fn stop(mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = self.output.clear_progress();
    }
}

/// Serialized owner of foreground progress and service-log terminal writes.
#[derive(Clone)]
struct ForegroundOutput {
    /// Mutable terminal state shared by the progress and log threads.
    inner: Arc<std::sync::Mutex<ForegroundOutputState>>,
}

/// State required to clear and restore one in-place terminal progress row.
struct ForegroundOutputState {
    /// Standard-output handle used by both foreground writers.
    stdout: io::Stdout,
    /// Whether standard output supports cursor-control sequences.
    terminal: bool,
    /// Most recent progress text, retained while service logs are written.
    progress: Option<String>,
    /// Whether the retained progress text is currently visible.
    progress_visible: bool,
}

impl ForegroundOutput {
    /// Creates an output owner configured for the current standard output.
    fn new() -> Self {
        let stdout = io::stdout();
        let terminal = stdout.is_terminal();
        Self {
            inner: Arc::new(std::sync::Mutex::new(ForegroundOutputState {
                stdout,
                terminal,
                progress: None,
                progress_visible: false,
            })),
        }
    }

    /// Whether progress can be repainted in place on this output stream.
    fn is_terminal(&self) -> bool {
        self.inner
            .lock()
            .map(|state| state.terminal)
            .unwrap_or(false)
    }

    /// Displays a progress update in place, or as a line for redirected output.
    fn show_progress(&self, line: &str) -> io::Result<()> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| io::Error::other(FOREGROUND_OUTPUT_LOCK_ERROR))?;
        state.progress = Some(line.to_string());
        state.render_progress()
    }

    /// Removes the active progress row before foreground reporting ends.
    fn clear_progress(&self) -> io::Result<()> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| io::Error::other(FOREGROUND_OUTPUT_LOCK_ERROR))?;
        state.progress = None;
        state.clear_visible_progress()
    }

    /// Writes one complete service-log fragment and restores progress below it.
    fn write_log(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| io::Error::other(FOREGROUND_OUTPUT_LOCK_ERROR))?;
        state.clear_visible_progress()?;
        state.stdout.write_all(bytes)?;
        if state.terminal {
            state.render_progress()
        } else {
            state.stdout.flush()
        }
    }

    /// Writes a foreground stream notice without colliding with progress.
    fn write_notice(&self, message: &str) -> io::Result<()> {
        let mut line = message.as_bytes().to_vec();
        line.push(b'\n');
        self.write_log(&line)
    }

    /// Flushes the shared standard-output handle.
    fn flush(&self) -> io::Result<()> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| io::Error::other(FOREGROUND_OUTPUT_LOCK_ERROR))?;
        state.stdout.flush()
    }
}

impl ForegroundOutputState {
    /// Clears the cursor-controlled progress row when it is visible.
    fn clear_visible_progress(&mut self) -> io::Result<()> {
        if self.terminal && self.progress_visible {
            self.stdout
                .write_all(clear_progress_spinner_line().as_bytes())?;
            self.progress_visible = false;
        }
        Ok(())
    }

    /// Renders the retained progress according to output capabilities.
    fn render_progress(&mut self) -> io::Result<()> {
        let Some(progress) = self.progress.as_deref() else {
            return self.stdout.flush();
        };
        if self.terminal {
            self.stdout
                .write_all(format_foreground_progress(progress).as_bytes())?;
            self.progress_visible = true;
        } else {
            writeln!(self.stdout, "{FOREGROUND_PROGRESS_PREFIX}{progress}")?;
        }
        self.stdout.flush()
    }
}

/// Buffers follow-stream chunks until complete lines can be written atomically.
struct ForegroundLogOutput {
    /// Shared terminal owner that coordinates with progress rendering.
    output: ForegroundOutput,
    /// Partial log line retained across stream writes.
    pending: Vec<u8>,
}

impl ForegroundLogOutput {
    /// Creates a line-buffered writer for the shared foreground terminal.
    fn new(output: ForegroundOutput) -> Self {
        Self {
            output,
            pending: Vec::new(),
        }
    }

    /// Writes all complete buffered lines through one terminal transaction.
    fn write_complete_lines(&mut self) -> io::Result<()> {
        if let Some(end) = self.pending.iter().rposition(|byte| *byte == b'\n') {
            let lines: Vec<u8> = self.pending.drain(..=end).collect();
            self.output.write_log(&lines)?;
        }
        Ok(())
    }
}

impl Write for ForegroundLogOutput {
    /// Buffers bytes and commits every complete line to the shared terminal.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        self.write_complete_lines()?;
        Ok(bytes.len())
    }

    /// Commits all buffered output, terminating a final partial log line.
    fn flush(&mut self) -> io::Result<()> {
        self.write_complete_lines()?;
        if !self.pending.is_empty() {
            self.pending.push(b'\n');
            self.write_complete_lines()?;
        }
        self.output.flush()
    }
}

/// Formats one boot-progress row without allowing terminal auto-wrap.
fn format_foreground_progress(line: &str) -> String {
    let prefix_width = FOREGROUND_PROGRESS_PREFIX.chars().count();
    let budget = terminal_width()
        .saturating_sub(prefix_width)
        .saturating_sub(TERMINAL_AUTOWRAP_MARGIN);
    let shown = if line.chars().count() > budget {
        let keep = budget.saturating_sub(1);
        line.chars()
            .take(keep)
            .chain(std::iter::once('…'))
            .collect::<String>()
    } else {
        line.to_string()
    };
    format!("\r{FOREGROUND_PROGRESS_PREFIX}{shown}\x1B[K")
}

/// Reports what a project's boot is DOING while it happens.
///
/// A foreground attach hands the boot to a background thread in the supervisor,
/// so the terminal has nothing to show until the first service writes a log
/// line. On a project whose `pre_start` waits on a DB or tunnel that is a minute
/// of silence, and the spinner this replaces said only "Starting" — leaving the
/// user unable to tell a slow boot from a wedged one. The supervisor publishes
/// its live operation (dependency waits, health-check attempts) through the op
/// slot; this surfaces it, printing only when the message CHANGES so a slow step
/// does not scroll the terminal.
fn spawn_boot_progress_reporter(
    project_id: String,
    output: ForegroundOutput,
) -> io::Result<BootProgressReporter> {
    let running = Arc::new(AtomicBool::new(true));
    let thread_running = Arc::clone(&running);
    let thread_output = output.clone();
    let handle = thread::Builder::new()
        .name(format!("boot-progress-{project_id}"))
        .spawn(move || {
            let terminal = thread_output.is_terminal();
            let mut last_line = String::new();
            let mut last_phase = None;
            while thread_running.load(Ordering::SeqCst) {
                if let Some(op) = ipc::current_op()
                    && op.label.contains(&project_id)
                {
                    let line = op.describe();
                    let phase = (op.label, op.detail);
                    let changed = if terminal {
                        line != last_line
                    } else {
                        last_phase.as_ref() != Some(&phase)
                    };
                    if changed {
                        let _ = thread_output.show_progress(&line);
                        last_line = line;
                        last_phase = Some(phase);
                    }
                }
                thread::sleep(BOOT_PROGRESS_INTERVAL);
            }
        })?;
    Ok(BootProgressReporter {
        output,
        running,
        handle: Some(handle),
    })
}

/// Streams a foreground project's logs in-process on a supervised loop. Connects
/// to the supervisor's follow stream and pumps it to stdout; on any drop, if the
/// caller still wants the stream AND the project is still loaded, it announces
/// the interruption on the shared terminal and reconnects after a short backoff.
/// Returns when the caller clears `streaming` or the project is gone — never
/// freezes silently.
fn spawn_foreground_log_follow(
    project_id: String,
    streaming: Arc<AtomicBool>,
    shutdown: Arc<std::sync::Mutex<Option<std::os::unix::net::UnixStream>>>,
    output: ForegroundOutput,
) -> io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name(format!("fg-logs-{project_id}"))
        .spawn(move || {
            let mut announced_interrupt = false;
            // Backlog is for the FIRST attach only. Every reconnect re-requested
            // the last N lines, and the supervisor answers that with the full
            // grouped render — "Offline Services" plus the finished output of
            // every completed one-shot — dumped into a live terminal. Measured
            // on a real project: 32 reconnects replayed the build/probe history
            // 32 times. After the first attach a follow must TAIL ONLY.
            let mut backlog_lines = FOREGROUND_BACKLOG_LINES;
            // A project that has not registered its first unit yet is BOOTING,
            // not gone. `supervisor_running()` goes true when the control socket
            // appears — well before any service is spawned — so without this
            // grace the very first follow attempt sees an unknown project and
            // retires the stream permanently, leaving the terminal silent for
            // the whole run. Only trust "project is gone" after it was seen.
            let mut project_ever_seen = false;
            let startup_grace =
                Instant::now() + systemg::constants::FOREGROUND_ATTACH_GRACE;
            while streaming.load(Ordering::SeqCst) {
                let command = ControlCommand::Logs {
                    service: None,
                    project: Some(project_id.clone()),
                    lines: backlog_lines,
                    kind: None,
                    follow: true,
                    since: None,
                    until: None,
                    grep: None,
                    all: false,
                    structured: false,
                };
                let terminal_writer = ForegroundLogOutput::new(output.clone());
                let mut writer =
                    LogWriter::new(terminal_writer, LogFormat::Text, false, None);
                let outcome = ipc::stream_command_output_interruptible(
                    &command,
                    &mut writer,
                    Some(&shutdown),
                );
                let _ = writer.flush();
                // Subsequent attempts tail only: no backlog replay.
                let loaded_now = project_loaded_in_supervisor(&project_id);
                if outcome.is_ok() || project_ever_seen || loaded_now {
                    backlog_lines = 0;
                }
                // Drop the connection handle now that this attempt ended, so a
                // stale shutdown target isn't left dangling between reconnects.
                if let Ok(mut guard) = shutdown.lock() {
                    *guard = None;
                }

                // A clean caller-requested stop: leave quietly.
                if !streaming.load(Ordering::SeqCst) {
                    return;
                }
                // The project is genuinely gone — nothing left to follow. During
                // the startup grace an unseen project is still booting, so keep
                // retrying instead of retiring the stream.
                if loaded_now {
                    project_ever_seen = true;
                } else if project_ever_seen || Instant::now() >= startup_grace {
                    if announced_interrupt {
                        let _ = output.write_notice(&format!(
                            "• log stream ended: project '{project_id}' stopped."
                        ));
                    }
                    return;
                } else {
                    for _ in 0..FOREGROUND_START_RETRIES {
                        if !streaming.load(Ordering::SeqCst) {
                            return;
                        }
                        thread::sleep(FOREGROUND_POLL_INTERVAL);
                    }
                    continue;
                }
                // The stream dropped but the project lives — say so and retry.
                match outcome {
                    Ok(()) => {
                        let _ = output.write_notice(&format!(
                            "⚠ log stream ended unexpectedly; reconnecting to '{project_id}'…"
                        ));
                    }
                    Err(err) => {
                        let _ = output.write_notice(&format!(
                            "⚠ log stream interrupted ({err}); reconnecting to '{project_id}'…"
                        ));
                    }
                }
                announced_interrupt = true;
                for _ in 0..FOREGROUND_RECONNECT_RETRIES {
                    if !streaming.load(Ordering::SeqCst) {
                        return;
                    }
                    thread::sleep(FOREGROUND_POLL_INTERVAL);
                }
            }
        })
}

/// Stops the log-follow: clears the streaming flag and force-closes the live
/// connection so a follow thread blocked in the copy loop unblocks immediately.
fn stop_foreground_follow(
    streaming: &Arc<AtomicBool>,
    shutdown: &Arc<std::sync::Mutex<Option<std::os::unix::net::UnixStream>>>,
) {
    streaming.store(false, Ordering::SeqCst);
    if let Ok(guard) = shutdown.lock()
        && let Some(stream) = guard.as_ref()
    {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Waits for Ctrl-C and then stops the foreground-owned project. The follow
/// stream is torn down on EVERY exit path so Ctrl-C (or a detach) stops the
/// console logs immediately instead of leaving them streaming.
fn wait_for_foreground_attachment(
    project_id: String,
    streaming: Arc<AtomicBool>,
    shutdown: Arc<std::sync::Mutex<Option<std::os::unix::net::UnixStream>>>,
) -> Result<(), Box<dyn Error>> {
    let ctrlc = foreground_ctrlc()?;
    wait_for_foreground_attachment_with_ctrlc(project_id, streaming, shutdown, ctrlc)
}

/// Installs the foreground Ctrl-C handler and returns its notification channel.
fn foreground_ctrlc() -> Result<mpsc::Receiver<()>, ctrlc::Error> {
    let (tx, rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = tx.send(());
    })?;
    Ok(rx)
}

fn wait_for_foreground_attachment_with_ctrlc(
    project_id: String,
    streaming: Arc<AtomicBool>,
    shutdown: Arc<std::sync::Mutex<Option<std::os::unix::net::UnixStream>>>,
    ctrlc: mpsc::Receiver<()>,
) -> Result<(), Box<dyn Error>> {
    // Wake on Ctrl-C, the supervisor disappearing, OR this project being stopped
    // out from under us. Blocking only on Ctrl-C wedged the foreground forever
    // when a `stop --supervisor`/crash tore the supervisor down, OR when a
    // `stop -p <this-project>` from ANOTHER terminal removed just this project
    // while the supervisor stayed up (hosting siblings) — the terminal never
    // returned. In both detach cases the project is already gone; nothing to stop.
    // A project that has not registered its first unit yet is still BOOTING.
    // `supervisor_running()` goes true when the control socket appears — well
    // before any service is spawned — so without this grace the very first
    // check mistook a booting project for one stopped elsewhere and detached
    // instantly, killing the terminal owner: no streamed output, and no process
    // left to receive Ctrl-C. Only trust "gone" once it has actually been seen.
    let mut project_ever_seen = false;
    let startup_grace = Instant::now() + systemg::constants::FOREGROUND_ATTACH_GRACE;
    loop {
        if ctrlc.recv_timeout(FOREGROUND_POLL_INTERVAL).is_ok() {
            break;
        }
        if !supervisor_running() {
            info!("Foreground supervisor is no longer running; detaching.");
            stop_foreground_follow(&streaming, &shutdown);
            return Ok(());
        }
        if project_loaded_in_supervisor(&project_id) {
            project_ever_seen = true;
        } else if project_ever_seen || Instant::now() >= startup_grace {
            info!("Foreground project '{project_id}' was stopped elsewhere; detaching.");
            stop_foreground_follow(&streaming, &shutdown);
            return Ok(());
        }
    }

    // Ctrl-C: stop the console stream at once, then tear down the project.
    stop_foreground_follow(&streaming, &shutdown);

    stop_foreground_project(&project_id)?;

    // The supervisor is impartial, warm-persistent infrastructure: it does NOT
    // shut down just because its last project left. Ctrl-C on a foreground
    // project deregisters that project and returns the terminal; the supervisor
    // stays running, idle and ready for the next `sysg start`. It ends ONLY on an
    // explicit `sysg stop --supervisor` (or `purge`).

    Ok(())
}

fn stop_foreground_project(project_id: &str) -> Result<(), Box<dyn Error>> {
    match ipc::send_command(&ControlCommand::StopProject {
        project: project_id.to_string(),
    }) {
        Ok(ControlResponse::Message(message)) => {
            println!("{message}");
            Ok(())
        }
        Ok(ControlResponse::Ok) => Ok(()),
        Ok(ControlResponse::Error(message)) => Err(ControlError::Server(message).into()),
        Ok(other) => Err(io::Error::other(format!(
            "unexpected supervisor response: {:?}",
            other
        ))
        .into()),
        Err(err) => Err(err.into()),
    }
}

/// Waits for a newly forked supervisor child to publish its control socket.
fn wait_for_supervisor_ready(child_pid: libc::pid_t) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + SUPERVISOR_CONNECT_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(peer) = verified_supervisor_pid() {
            if peer == child_pid {
                return Ok(());
            }
            if process_exited(child_pid) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("supervisor PID {peer} acquired the runtime first"),
                )
                .into());
            }
        }

        if process_exited(child_pid) {
            return Err(
                io::Error::other("foreground supervisor exited during startup").into(),
            );
        }

        thread::sleep(SERVICE_POLL_INTERVAL);
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "timed out waiting for foreground supervisor to start",
    )
    .into())
}

/// Dispatches a resolved (preflight-cleared) restart plan.
fn dispatch_restart(
    plan: systemg::restart::RestartPlan,
    daemonize: bool,
    verbose: bool,
) -> Result<(), Box<dyn Error>> {
    use systemg::restart::RestartPlan;

    if let RestartPlan::Recycle { config } = plan {
        return recycle_supervisor_for_restart(config);
    }

    let config_path = restart_plan_config(&plan);

    if !supervisor_running() {
        if daemonize {
            return start_supervisor_daemon(config_path, None, false, verbose);
        }
        warn!(
            "No running supervisor detected; executing restart in local one-shot mode. \
Use --daemonize in deployment scripts to ensure daemonized supervision is restored if detection fails."
        );
        let daemon = build_daemon(&config_path.to_string_lossy())?;
        return with_progress_spinner("Restarting", || {
            daemon
                .restart_services()
                .map_err(|err| Box::new(err) as Box<dyn Error>)
        });
    }

    let command = match plan {
        RestartPlan::Recycle { .. } => unreachable!("handled above"),
        RestartPlan::Everything { config } => ControlCommand::Restart {
            config: restart_scoped_config(&config),
            service: None,
            project: None,
        },
        RestartPlan::Project { config, project } => ControlCommand::Restart {
            config: restart_scoped_config(&config),
            service: None,
            project: Some(project),
        },
        RestartPlan::Service {
            config,
            service,
            project,
        } => ControlCommand::Restart {
            // Thread the resolved config through so a scoped `restart -c <file>
            // -s svc` reloads the manifest and applies that service's changed
            // config on the bounce — dropping it here silently ignored -c.
            config: restart_scoped_config(&config),
            service: Some(service),
            project,
        },
    };

    if daemonize {
        restart_daemonized(command, config_path, false)
    } else {
        with_progress_message("Restarting", || send_control_message(command))
    }
}

/// The config path a restart plan carries (for the not-running fork/one-shot).
fn restart_plan_config(plan: &systemg::restart::RestartPlan) -> PathBuf {
    use systemg::restart::RestartPlan;
    match plan {
        RestartPlan::Everything { config }
        | RestartPlan::Recycle { config }
        | RestartPlan::Project { config, .. }
        | RestartPlan::Service { config, .. } => config.clone(),
    }
}

/// The config path to hand a scoped restart, or `None` to fall back to the
/// resident supervisor's loaded manifest. A scoped restart submits its `-c` file
/// so the changed service is reloaded and applied; but the default
/// `systemg.yaml` that no one actually passed must not be sent (it may not
/// exist), so it degrades to `None` and the supervisor uses what it has.
fn restart_scoped_config(config: &Path) -> Option<String> {
    let is_default_name = config
        .file_name()
        .is_some_and(|name| name == DEFAULT_CONFIG_PATH);
    if is_default_name && !config.exists() {
        return None;
    }
    Some(config.to_string_lossy().to_string())
}

/// Renders a stop-plan resolution failure as a typed diagnostic.
fn stop_plan_diag(err: systemg::stop::StopPlanError) -> DiagError {
    use systemg::stop::StopPlanError;
    let diag = match err {
        StopPlanError::Mismatch(mismatch) => {
            systemg::start::project_mismatch(&mismatch.flag, &mismatch.selector)
        }
        StopPlanError::SupervisorWithSelector => systemg::diag::Diagnostic::error(
            systemg::diag::SgCode::ConflictingSelectors,
            "--supervisor cannot be combined with a service or project selector",
        )
        .note("--supervisor shuts the whole supervisor down; drop -s/-p to use it")
        .help_docs(),
    };
    DiagError(Box::new(diag))
}

/// Dispatches a resolved stop plan: shuts the supervisor down, sends the resident
/// supervisor a scoped stop, or falls back to a local one-shot stop when no
/// supervisor is running.
fn dispatch_stop(plan: systemg::stop::StopPlan) -> Result<(), Box<dyn Error>> {
    use systemg::stop::StopPlan;

    let health = supervisor_health();

    if let StopPlan::Supervisor = plan {
        stop_supervisors()?;
        wait_for_runtime_cleared(SUPERVISOR_RUNTIME_TIMEOUT);
        if supervisor_running() {
            return Err(Box::new(DiagError(Box::new(
                supervisor_not_responding_diag(),
            ))));
        }
        return Ok(());
    }

    // Never route a unit stop into a daemon that is alive but not serving — the
    // command would hang or be silently dropped (BUG-4). Refuse with SG0205.
    if health == SupervisorHealth::Dying {
        return Err(Box::new(DiagError(Box::new(
            supervisor_not_responding_diag(),
        ))));
    }

    if health == SupervisorHealth::Serving {
        let command = match plan {
            StopPlan::Supervisor => unreachable!("handled above"),
            StopPlan::Everything { .. } => ControlCommand::Stop {
                service: None,
                project: None,
            },
            StopPlan::Project { project } => ControlCommand::Stop {
                service: None,
                project: Some(project),
            },
            StopPlan::Service { service, project } => ControlCommand::Stop {
                service: Some(service),
                project,
            },
        };
        return with_progress_message("Stopping", || send_control_message(command));
    }

    cleanup_stopped_runtime();

    // If the target names a project the resident supervisor already knows, the
    // supervisor owns its config — route the stop there and NEVER demand a local
    // file. This keeps `sysg stop -p <loaded-project>` working from any directory
    // without `-c`; SG0203 is only correct when the project is genuinely unknown.
    let plan_project = match &plan {
        StopPlan::Project { project } => Some(project.clone()),
        StopPlan::Service {
            project: Some(project),
            ..
        } => Some(project.clone()),
        _ => None,
    };
    if let Some(project) = plan_project
        && project_loaded_in_supervisor(&project)
    {
        let command = match plan {
            StopPlan::Service { service, project } => ControlCommand::Stop {
                service: Some(service),
                project,
            },
            _ => ControlCommand::Stop {
                service: None,
                project: Some(project),
            },
        };
        return with_progress_message("Stopping", || send_control_message(command));
    }

    // A `-p <project>` with NO supervisor running has nothing to stop: there is
    // no registry to resolve the project against, and the project's services
    // cannot be running without a supervisor to have started them. Falling
    // through to the local-config path here produced the circular SG0203 —
    // "could not read a local config file … target the project by id with -p"
    // — when `-p` is exactly what the user passed.
    if !supervisor_running()
        && let StopPlan::Project { project }
        | StopPlan::Service {
            project: Some(project),
            ..
        } = &plan
    {
        use systemg::diag::{Diagnostic, SgCode};
        return Err(Box::new(DiagError(Box::new(
            Diagnostic::error(
                SgCode::TargetNotFound,
                format!("no project '{project}' is running"),
            )
            .note("there is no supervisor, so nothing from that project is running")
            .help_cmd("start it", format!("sysg start -p {project}"))
            .help_docs(),
        ))));
    }

    // No supervisor context for the target: run a one-shot local stop from the
    // config on disk.
    let config = match &plan {
        StopPlan::Everything { config } => config.to_string_lossy().to_string(),
        _ => resolve_config_path(DEFAULT_CONFIG_PATH)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| DEFAULT_CONFIG_PATH.to_string()),
    };
    let daemon = build_daemon(&config)?;
    match plan {
        StopPlan::Service { service, .. } => daemon.stop_service(&service)?,
        _ => daemon.stop_services()?,
    }
    Ok(())
}

/// Dispatches a `--daemonize` start plan: routes to the resident supervisor
/// when one is running, otherwise forks a fresh supervisor from the plan's
/// config.
fn dispatch_start_daemonize(
    plan: systemg::start::StartPlan,
    stderr: bool,
    verbose: bool,
    drop_privileges: bool,
) -> Result<(), Box<dyn Error>> {
    match supervisor_health() {
        SupervisorHealth::Serving => {
            if drop_privileges {
                warn!(
                    "--drop-privileges is managed by the running supervisor and has no effect for this start request"
                );
            }
            match dispatch_start_resident(plan.clone()) {
                Ok(()) => return Ok(()),
                Err(err) if error_is_supervisor_shutting_down(err.as_ref()) => {
                    wait_for_runtime_cleared(SUPERVISOR_RUNTIME_TIMEOUT);
                    if supervisor_running() {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            }
        }
        SupervisorHealth::Dying => {
            return Err(Box::new(DiagError(Box::new(
                supervisor_not_responding_diag(),
            ))));
        }
        SupervisorHealth::Down => {}
    }

    // No usable supervisor: fork one from the plan's config. An ad-hoc unit
    // is started the same way — its staged config becomes the new supervisor's.
    let service = plan_service_name(&plan);
    // `-p <project>` only names a project a RUNNING supervisor already knows.
    // With no supervisor there is no registry to resolve it against, so if the
    // plan's config does not exist we would fork a supervisor that dies on a
    // missing file (a bare os error 2). Say what the user actually has to do.
    if let systemg::start::StartPlan::Project { config, project } = &plan
        && !config.exists()
    {
        use systemg::diag::{Diagnostic, SgCode};
        return Err(Box::new(DiagError(Box::new(
            Diagnostic::error(
                SgCode::ConfigFileUnreadable,
                format!("no running supervisor knows project `{project}`"),
            )
            .note(
                "`-p` targets a project that a running supervisor already has \
                 loaded; with no supervisor there is nothing to target",
            )
            .note(format!(
                "cold-starting an unregistered project needs its manifest: {} does not exist",
                config.display()
            ))
            .help_cmd(
                "start it from its config",
                "sysg start -c <config>.yaml --daemonize",
            )
            .help_cmd("see what is loaded", "sysg status")
            .help_docs(),
        ))));
    }
    let config = plan_config(plan);
    info!("Starting systemg supervisor with config {:?}", config);
    start_supervisor_daemon(config, service, stderr, verbose)
}

/// The config path a plan carries.
fn plan_config(plan: systemg::start::StartPlan) -> PathBuf {
    use systemg::start::StartPlan;
    match plan {
        StartPlan::WholeConfig { config }
        | StartPlan::Project { config, .. }
        | StartPlan::Service { config, .. }
        | StartPlan::StageAdHoc { config } => config,
    }
}

/// Sends a resident supervisor the control command for `plan`.
fn dispatch_start_resident(
    plan: systemg::start::StartPlan,
) -> Result<(), Box<dyn Error>> {
    use systemg::start::StartPlan;

    let command = match plan {
        StartPlan::StageAdHoc { config } => {
            info!(
                "Staged unit config at {config:?}. Running supervisor was left unchanged."
            );
            println!(
                "Unit staged at {}. Run `sysg restart --daemonize --config {}` to apply it.",
                config.display(),
                config.display()
            );
            return Ok(());
        }
        StartPlan::WholeConfig { config } => ControlCommand::AddProject {
            config: config.to_string_lossy().to_string(),
            service: None,
            mode: ProjectRunMode::Daemon,
        },
        StartPlan::Project { project, .. } => ControlCommand::Start {
            service: None,
            project: Some(project),
        },
        StartPlan::Service {
            service, project, ..
        } => ControlCommand::Start {
            service: Some(service),
            project,
        },
    };
    // An `AddProject` returns as soon as the supervisor QUEUES the boot onto a
    // background thread — deliberately, so a slow project cannot block its
    // single-writer owner thread. But returning "loaded" at that point reports
    // success for work that has not happened: health checks, dependency waits
    // and startup failures all land after the user is back at the prompt.
    // Attach must behave like the fork path — show the boot's progress and
    // report its real outcome.
    let awaited_project = match &command {
        ControlCommand::AddProject { config, .. } => {
            load_config(Some(config)).ok().map(|c| c.project.id)
        }
        _ => None,
    };
    let failed = with_progress_spinner("Starting", || {
        if awaited_project.is_some() {
            send_add_project(&command)?;
        } else {
            send_control_command(command)?;
        }
        Ok(match &awaited_project {
            Some(project) => await_queued_boot(project)?,
            None => Vec::new(),
        })
    })?;

    // The supervisor logged the real cause (SG0103 pre-start failure, a health
    // check that never passed, ...). Without this the CLI printed "loaded" and
    // exited 0 while the project had comprehensively failed to start — a script
    // or CI job would read that as a clean start.
    if !failed.is_empty() {
        use systemg::diag::{Diagnostic, SgCode};
        let project = awaited_project.as_deref().unwrap_or("<project>");
        let mut diag = Diagnostic::error(
            SgCode::ProjectServicesNotUp,
            format!("{} service(s) did not come up", failed.len()),
        )
        .note(format!("did not start: {}", failed.join(", ")));
        for service in failed.iter().take(3) {
            diag = diag.help_cmd(
                format!("why '{service}' failed"),
                format!("sysg logs -p {project} -s {service}"),
            );
        }
        return Err(Box::new(DiagError(Box::new(
            diag.help_cmd("supervisor detail", "sysg logs --supervisor")
                .help_docs(),
        ))));
    }
    Ok(())
}

fn send_add_project(command: &ControlCommand) -> Result<(), Box<dyn Error>> {
    match ipc::send_command_with_timeout(command, SUPERVISOR_REQUEST_TIMEOUT) {
        Ok(ipc::CommandAck::Pending) => Ok(()),
        Ok(ipc::CommandAck::Response(ControlResponse::Message(message))) => {
            println!("{message}");
            Ok(())
        }
        Ok(ipc::CommandAck::Response(ControlResponse::Ok)) => Ok(()),
        Ok(ipc::CommandAck::Response(ControlResponse::Error(message))) => {
            Err(ControlError::Server(message).into())
        }
        Ok(ipc::CommandAck::Response(ControlResponse::Diag(diag))) => {
            Err(Box::new(DiagError(diag)))
        }
        Ok(ipc::CommandAck::Response(other)) => Err(io::Error::other(format!(
            "unexpected add-project response: {other:?}"
        ))
        .into()),
        Err(err) => Err(err.into()),
    }
}

/// Blocks until the supervisor's background boot for `project` has settled, then
/// reports which of its services failed to come up.
///
/// `AddProject` deliberately does NOT own the op slot (the supervisor keeps its
/// owner thread free for a slow boot), so polling the op slot sees nothing —
/// what the caller must actually track is the project's own units settling.
///
/// Bounded, so a genuinely slow or wedged boot cannot hang the CLI: on timeout
/// the caller is told what is still pending rather than being left to believe
/// the start succeeded.
fn await_queued_boot(project: &str) -> Result<Vec<String>, ControlError> {
    let deadline = Instant::now() + systemg::constants::START_SETTLE_GRACE;
    // Units left behind by a previous stop are ALREADY `stopped` — not
    // `starting` — so judging the snapshot immediately reads stale state from
    // before this boot and reports failure in ~1s, before a single service has
    // been launched. Correct only by luck when the project really is broken; on
    // a slow but successful boot it is a false failure. Wait for evidence the
    // boot has actually begun (a unit running, or one freshly completed) before
    // any verdict.
    let mut boot_began = false;
    while Instant::now() < deadline {
        thread::sleep(BOOT_PROGRESS_INTERVAL);
        let units = project_service_units(project)?;
        if units.is_empty() {
            continue;
        }
        // A unit is still working while it is starting; anything else has
        // reached a state worth reporting.
        let pending = units
            .iter()
            .any(|(_, state)| matches!(state, UnitState::Unknown));
        if pending {
            boot_began = true;
            continue;
        }
        let bind_failed = units.iter().any(|(service, state)| {
            *state == UnitState::Failed
                && systemg::daemon::output_indicates_port_conflict(
                    &systemg::logs::tail_service_log(
                        project,
                        service,
                        PORT_DIAGNOSTIC_TAIL_LINES,
                    ),
                )
        });
        if !boot_began {
            if units
                .iter()
                .any(|(_, state)| matches!(state, UnitState::Running | UnitState::Done))
                || bind_failed
            {
                boot_began = true;
            } else {
                // Nothing has moved yet: this is the pre-boot snapshot.
                continue;
            }
        }
        let down: Vec<String> = units
            .iter()
            .filter(|(_, state)| {
                matches!(
                    state,
                    UnitState::Stopped | UnitState::Failed | UnitState::Lost
                )
            })
            .map(|(name, _)| name.clone())
            .collect();

        // Everything is up: return AT ONCE. The long grace exists only to give a
        // slow pre-start room to finish, so a healthy project must never pay it.
        if down.is_empty() {
            return Ok(Vec::new());
        }

        if bind_failed {
            return Ok(down);
        }

        // Something is down. It may still be retrying — a service whose
        // `pre_start` waits on a DB cycles through stopped between attempts —
        // so keep waiting out the grace and only report what is STILL down when
        // it expires. Reporting the first down poll blamed services that went on
        // to run healthily.
    }
    // Grace expired with services still down: report the last known offenders.
    Ok(project_service_units(project)?
        .into_iter()
        .filter(|(_, state)| {
            matches!(
                state,
                UnitState::Unknown
                    | UnitState::Stopped
                    | UnitState::Failed
                    | UnitState::Lost
            )
        })
        .map(|(name, _)| name)
        .collect())
}

/// Returns `(unit name, state)` for every service in `project`. An unavailable
/// control plane remains a typed error so the caller can recover from shutdown.
/// Cron units are excluded because they never indicate whether services booted.
fn project_service_units(
    project: &str,
) -> Result<Vec<(String, UnitState)>, ControlError> {
    let snapshot = match ipc::send_command_with_timeout(
        &ControlCommand::Status { live: false },
        SUPERVISOR_REQUEST_TIMEOUT,
    )? {
        ipc::CommandAck::Response(ControlResponse::Status(snapshot)) => snapshot,
        ipc::CommandAck::Pending => return Err(ControlError::Timeout),
        ipc::CommandAck::Response(ControlResponse::Error(message)) => {
            return Err(ControlError::Server(message));
        }
        ipc::CommandAck::Response(other) => {
            return Err(ControlError::Io(io::Error::other(format!(
                "unexpected supervisor status response: {other:?}"
            ))));
        }
    };
    Ok(snapshot
        .units
        .iter()
        .filter(|unit| {
            unit.kind == UnitKind::Service
                && unit
                    .project
                    .as_ref()
                    .is_some_and(|owner| owner.id == project)
        })
        .map(|unit| (unit.name.clone(), unit.state))
        .collect())
}

/// Whether a start lost contact with the resident supervisor. The caller waits
/// for teardown and cold-starts only when the supervisor is actually gone.
fn error_is_supervisor_shutting_down(err: &(dyn Error + 'static)) -> bool {
    if matches!(
        err.downcast_ref::<ControlError>(),
        Some(ControlError::NotAvailable | ControlError::Timeout)
    ) {
        return true;
    }
    let message = err.to_string().to_ascii_lowercase();
    message.contains("dropped the command before replying")
        || message.contains("supervisor is shutting down")
}

/// Dispatches a foreground (non-daemonize) start plan.
fn dispatch_start_foreground(
    plan: systemg::start::StartPlan,
    stderr: bool,
) -> Result<(), Box<dyn Error>> {
    use systemg::start::StartPlan;

    if supervisor_running() {
        // A targeted service/project routes to the resident supervisor; a whole
        // foreground config attaches to it and owns the terminal lifetime.
        return match plan {
            StartPlan::WholeConfig { config } => start_foreground_attached(config, None),
            other => dispatch_start_resident(other),
        };
    }

    match plan {
        StartPlan::StageAdHoc { config }
        | StartPlan::WholeConfig { config }
        | StartPlan::Project { config, .. } => start_foreground(config, None, stderr),
        StartPlan::Service {
            config, service, ..
        } => start_foreground(config, Some(service), stderr),
    }
}

/// The service name a plan targets, if it targets a single service.
fn plan_service_name(plan: &systemg::start::StartPlan) -> Option<String> {
    match plan {
        systemg::start::StartPlan::Service { service, .. } => Some(service.clone()),
        _ => None,
    }
}

/// Starts supervisor daemon.
fn start_supervisor_daemon(
    config_path: PathBuf,
    service: Option<String>,
    pipe_stderr: bool,
    verbose: bool,
) -> Result<(), Box<dyn Error>> {
    let child_pid = unsafe { libc::fork() };
    if child_pid < 0 {
        return Err(io::Error::last_os_error().into());
    }

    if child_pid == 0 {
        detach_daemon()?;
        reexec_supervisor(
            &config_path,
            service.as_deref(),
            pipe_stderr,
            verbose,
            ProjectRunMode::Daemon,
        );
        run_supervisor_in_process(
            config_path,
            service,
            pipe_stderr,
            ProjectRunMode::Daemon,
            None,
        );
    }

    // Wait for the child to publish its socket, then follow the boot stream so
    // the parent reports the truth of what came up — not just "socket is live".
    if let Err(err) = wait_for_supervisor_ready(child_pid) {
        unsafe {
            libc::waitpid(child_pid, std::ptr::null_mut(), libc::WNOHANG);
        }
        if supervisor_running() {
            return send_control_command(ControlCommand::AddProject {
                config: config_path.to_string_lossy().to_string(),
                service,
                mode: ProjectRunMode::Daemon,
            });
        }
        use systemg::diag::{Diagnostic, SgCode};
        return Err(Box::new(DiagError(Box::new(
            Diagnostic::error(SgCode::Catchall, "the supervisor did not come up in time")
                .note(err.to_string())
                .help_cmd("read the supervisor log", "sysg logs --supervisor")
                .help_docs(),
        ))));
    }

    let report = follow_boot(verbose)?;
    if let Some(diag) = report.failures.into_iter().next() {
        return Err(Box::new(DiagError(Box::new(diag))));
    }
    Ok(())
}

/// Re-execs this binary into the internal `supervise` subcommand so a new
/// supervisor starts from a clean, single-threaded process image.
///
/// `fork()` in a multithreaded process is a trap: only the forking thread
/// survives in the child, and any mutex a vanished thread held stays locked
/// forever. A recycle forks the daemon *after* `stop_supervisors` and the
/// version probe have already spun up IPC / sysinfo / spinner threads, so the
/// forked daemon inherited those poisoned locks and wedged — silent, and
/// suppressing its own services' restarts. `execv` replaces the image wholesale,
/// dropping every inherited thread and lock, so the daemon always boots pristine.
/// On success this never returns; on failure it falls through to the in-process
/// boot so a daemon still comes up.
fn reexec_supervisor(
    config: &Path,
    service: Option<&str>,
    pipe_stderr: bool,
    verbose: bool,
    mode: ProjectRunMode,
) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut args: Vec<CString> = Vec::new();
    let push = |args: &mut Vec<CString>, s: &str| {
        if let Ok(c) = CString::new(s) {
            args.push(c);
        }
    };
    push(&mut args, &exe.to_string_lossy());
    push(&mut args, "supervise");
    push(&mut args, "--config");
    push(&mut args, &config.to_string_lossy());
    if let Some(service) = service {
        push(&mut args, "--service");
        push(&mut args, service);
    }
    if pipe_stderr {
        push(&mut args, "--pipe-stderr");
    }
    if verbose {
        push(&mut args, "--verbose");
    }
    if mode == ProjectRunMode::Foreground {
        push(&mut args, "--foreground");
    }
    let _ = nix::unistd::execv(&args[0], &args);
}

/// Boots the supervisor in the current process and exits. Reached post-`execv`
/// via the `supervise` subcommand, or as the fallback when re-exec fails.
fn run_supervisor_in_process(
    config_path: PathBuf,
    service: Option<String>,
    pipe_stderr: bool,
    mode: ProjectRunMode,
    handoff: Option<PathBuf>,
) -> ! {
    install_supervisor_panic_hook();
    let handed_off = handoff.is_some();
    let handoff_path = handoff.clone();
    let supervisor = match handoff {
        Some(path) => Supervisor::from_handoff(path),
        None => Supervisor::new_with_mode(config_path, false, service, mode),
    };
    let mut supervisor = match supervisor {
        Ok(supervisor) => supervisor,
        Err(err) => {
            error!("Supervisor failed to initialize: {err}");
            if let Some(path) = handoff_path
                && let Err(rollback_err) =
                    systemg::upgrade::rollback_handoff(&path, err.to_string())
            {
                error!("Supervisor rollback failed: {rollback_err}");
            }
            process::exit(1);
        }
    };
    if !handed_off {
        supervisor.set_pipe_stderr(pipe_stderr);
    }
    exit_supervisor(supervisor.run());
}

fn exit_supervisor(result: Result<(), SupervisorError>) -> ! {
    match result {
        Ok(()) => process::exit(0),
        Err(SupervisorError::Control(ControlError::RuntimeBusy)) => {
            info!("Another supervisor acquired the runtime first");
            process::exit(0);
        }
        Err(err) => {
            error!("Supervisor exited with error: {err}");
            process::exit(1);
        }
    }
}

fn install_supervisor_panic_hook() {
    std::panic::set_hook(Box::new(|panic| {
        error!("Supervisor panicked: {panic}");
    }));
}

/// Subscribes to the supervisor's boot stream and renders it, returning the
/// report. In quiet mode a spinner is the feedback; in verbose mode each unit
/// prints a line to stderr as it comes up.
///
/// A freshly forked supervisor may not be answering the control socket the
/// instant its PID appears, so the subscription is retried briefly.
fn follow_boot(verbose: bool) -> Result<systemg::start::BootReport, Box<dyn Error>> {
    let report = if verbose {
        collect_boot_report(true)?
    } else {
        with_progress_spinner("Starting", || collect_boot_report(false))?
    };
    Ok(report)
}

/// Reads the supervisor's complete boot journal and converts it into the CLI
/// report used to decide startup success.
fn collect_boot_report(
    verbose: bool,
) -> Result<systemg::start::BootReport, Box<dyn Error>> {
    let deadline = Instant::now() + SUPERVISOR_CONNECT_TIMEOUT;
    loop {
        let mut frames = Vec::new();
        match ipc::stream_boot_frames(|frame| frames.push(frame)) {
            Ok(()) => {
                return Ok(systemg::start::render_boot(frames, verbose, io::stderr()));
            }
            Err(_) if Instant::now() < deadline => {
                thread::sleep(SERVICE_POLL_INTERVAL);
            }
            Err(err) => return Err(err.into()),
        }
    }
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
        let config_path = resolve_config_path(config)?;
        return Ok(StartTarget {
            config_path,
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
        "version: \"{version}\"\nservices:\n  {name}:\n    command: {command}\n",
        version = systemg::config::CURRENT_MANIFEST_VERSION,
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

/// Renders the project-mismatch diagnostic and exits: the requested project is
/// not the one the resolved config defines, which usually means the CLI picked
/// up a different config than the user expects. Lists what the running
/// supervisor actually has loaded so the user can retarget without guessing.
/// Whether `project_id` is currently loaded in the running supervisor. Used to
/// let an explicit `-p <loaded-project>` be authoritative for read commands
/// (logs/inspect/status) even when the config resolved from disk is a different
/// project's — the supervisor already knows the loaded project's config.
fn project_loaded_in_supervisor(project_id: &str) -> bool {
    matches!(
        ipc::send_command(&ControlCommand::Status { live: false }),
        Ok(ControlResponse::Status(snapshot))
            if snapshot.units.iter().any(|unit| {
                unit.project
                    .as_ref()
                    .is_some_and(|project| project.id == project_id)
            })
    )
}

fn fail_project_mismatch(requested: &str, config_project: &str) -> ! {
    use systemg::diag::{Diagnostic, SgCode};

    let loaded: Vec<String> =
        match ipc::send_command(&ControlCommand::Status { live: false }) {
            Ok(ControlResponse::Status(snapshot)) => {
                let mut ids: Vec<String> = snapshot
                    .units
                    .iter()
                    .filter_map(|unit| unit.project.as_ref().map(|p| p.id.clone()))
                    .collect();
                ids.sort();
                ids.dedup();
                ids
            }
            _ => Vec::new(),
        };

    let mut diag = Diagnostic::error(
        SgCode::TargetConfigMismatch,
        format!("project `{requested}` does not match the resolved config"),
    )
    .note(format!(
        "the config sysg resolved for this command defines project `{config_project}`"
    ))
    .note(
        "sysg resolved a config that may not be the one you meant — pass -c to point \
         at the right file, or -p with a loaded project id",
    );

    if !loaded.is_empty() {
        diag = diag.evidence("projects loaded in the running supervisor", loaded);
    }

    let diag = diag
        .help_cmd("list projects", "sysg status")
        .help_cmd(
            "target explicitly",
            "sysg logs -s <service> -p <project-id>",
        )
        .help_docs();

    eprintln!("{}", diag.render_for_terminal());
    process::exit(1);
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
                fail_project_mismatch(&project, &config.project.id);
            }
            Ok(Some(project))
        }
        (Some(config_arg), None) => {
            // `-c <file>` targets EVERY project the file declares, not just the
            // first. Validate the file is loadable, then apply no project filter
            // so the aggregate snapshot surfaces all of its projects.
            let config_path = resolve_config_path(config_arg)?;
            load_config(Some(config_path.to_string_lossy().as_ref()))?;
            Ok(None)
        }
        (None, project) => Ok(project),
    }
}

/// Returns every project id a config file declares (one for a single-project or
/// loose config, N for a multi-project config).
fn config_declared_projects(config_path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let content = fs::read_to_string(config_path)?;
    let configs = systemg::config::parse_config_projects(&content)?;
    Ok(configs.into_iter().map(|c| c.project.id).collect())
}

/// Resolves the project a command should target from an explicit project flag and config.
fn resolve_command_project(
    config_arg: &str,
    explicit_project: Option<String>,
    service: Option<&str>,
) -> Result<Option<String>, Box<dyn Error>> {
    // An explicit `-p` names the target outright, so a missing/unreadable local
    // config must not sink the command before the selector is even consulted.
    // Reading logs after the supervisor is gone — the usual post-mortem — runs
    // from a directory with no manifest, and failing there sent the user in a
    // circle: "target the project with -p" when -p was exactly what they passed.
    let config_path = match resolve_config_path(config_arg) {
        Ok(path) => path,
        Err(err) => {
            if let Some(project) = explicit_project {
                return Ok(Some(project));
            }
            return Err(err);
        }
    };
    let config_value = load_config(Some(config_path.to_string_lossy().as_ref())).ok();

    if let Some(project) = explicit_project {
        // A `-p` that names a project ALREADY LOADED in the running supervisor is
        // authoritative — sysg knows that project's registered config, so the
        // command targets it regardless of whatever config was resolved from the
        // cwd/default. This is what lets `logs -p other-project` (and the status
        // TUI's Logs key) work without pointing -c at the right file.
        if project_loaded_in_supervisor(&project) {
            return Ok(Some(project));
        }
        // A multi-project config declares several projects; `-p` may name any of
        // them, not just the primary. Only reject when the config genuinely does
        // not declare the requested project.
        if config_arg != DEFAULT_CONFIG_PATH
            && let Ok(declared) = config_declared_projects(&config_path)
            && !declared.is_empty()
            && !declared.iter().any(|id| id == &project)
        {
            let primary = config_value
                .as_ref()
                .map(|c| c.project.id.clone())
                .unwrap_or_else(|| declared.join(", "));
            fail_project_mismatch(&project, &primary);
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
    verified_supervisor_pid().is_some()
}

fn verified_supervisor_pid() -> Option<libc::pid_t> {
    let peer = ipc::supervisor_peer_pid().ok()? as libc::pid_t;
    if ipc::read_supervisor_pid().ok().flatten() != Some(peer) {
        warn!("Repairing supervisor PID file from control-socket peer {peer}");
        if let Err(err) = ipc::write_supervisor_pid(peer) {
            warn!("Failed to repair supervisor PID file: {err}");
        }
    }
    Some(peer)
}

/// How much of a running supervisor is actually usable, distinguishing a healthy
/// daemon from one whose process is alive but no longer answering — the stale /
/// dying case that `supervisor_running()`'s bare liveness check cannot see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupervisorHealth {
    /// Process alive and answering its control socket. Safe to route commands to.
    Serving,
    /// Process alive but not answering the socket within the probe window. A
    /// command routed here would hang or be dropped — treat as unusable.
    Dying,
    /// No usable supervisor: no pid and no live socket (stale runtime cleaned).
    Down,
}

const SUPERVISOR_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const SUPERVISOR_PROBE_DEADLINE: Duration = Duration::from_millis(1500);
const SUPERVISOR_PROBE_ATTEMPTS: usize = 3;

/// Probes the resident supervisor's health. Liveness alone is a lie during
/// shutdown — a frozen or mid-teardown daemon keeps its pid but stops serving —
/// so a live pid is confirmed with a bounded `Status` ping over the socket.
fn supervisor_health() -> SupervisorHealth {
    if verified_supervisor_pid().is_some() {
        probe_serving_supervisor()
    } else if !gather_supervisor_pids().is_empty() {
        SupervisorHealth::Dying
    } else {
        SupervisorHealth::Down
    }
}

/// Classifies a live-pid supervisor as Serving or Dying by pinging its control
/// socket. A single busy tick is not death: under load a healthy daemon whose
/// op-slot is momentarily occupied answers `Pending` rather than a response, so
/// each attempt runs on its own hard-deadline thread and a `Pending` retries
/// within the remaining budget. Only when every attempt fails to draw a real
/// response — timeout, `Pending`, or error to the last try — is the daemon
/// declared Dying. A real response at any point is Serving.
fn probe_serving_supervisor() -> SupervisorHealth {
    for _ in 0..SUPERVISOR_PROBE_ATTEMPTS {
        let (tx, rx) = std::sync::mpsc::channel();
        if thread::Builder::new()
            .name(SUPERVISOR_PROBE_THREAD.into())
            .spawn(move || {
                let ack = ipc::send_command_with_timeout(
                    &ControlCommand::Status { live: false },
                    SUPERVISOR_PROBE_TIMEOUT,
                );
                let _ = tx.send(matches!(ack, Ok(ipc::CommandAck::Response(_))));
            })
            .is_err()
        {
            continue;
        }
        match rx.recv_timeout(SUPERVISOR_PROBE_DEADLINE) {
            Ok(true) => return SupervisorHealth::Serving,
            Ok(false) | Err(_) => continue,
        }
    }
    SupervisorHealth::Dying
}

/// Diagnostic for a command refused because the supervisor is alive but not
/// answering — the caller must not route a command into a dying daemon.
fn supervisor_not_responding_diag() -> systemg::diag::Diagnostic {
    systemg::diag::Diagnostic::error(
        systemg::diag::SgCode::SupervisorNotResponding,
        "the supervisor is running but not answering its control socket",
    )
    .note("its process is alive but did not reply within the probe window; it may be shutting down or wedged")
    .help_cmd("force it down", "sysg stop --supervisor")
    .help_cmd("then restart", "sysg start --daemonize")
    .help_docs()
}

/// Sends control command.
fn send_control_command(command: ControlCommand) -> Result<(), Box<dyn Error>> {
    send_control_command_inner(command, true)
}

/// Requests a live supervisor replacement and waits until the same PID reports
/// the staged version or the previous binary confirms rollback.
fn request_live_upgrade(binary: String) -> Result<String, Box<dyn Error>> {
    let target = systemg::upgrade::LiveUpgradeInfo::current();
    match supervisor_health() {
        SupervisorHealth::Down => {
            return Ok("No running supervisor; activation is safe".into());
        }
        SupervisorHealth::Dying => {
            return Err(Box::new(DiagError(Box::new(
                systemg::upgrade::environment_unsafe(
                    "the resident supervisor is alive but is not answering its control socket",
                ),
            ))));
        }
        SupervisorHealth::Serving => {}
    }
    let resident = match ipc::send_command(&ControlCommand::Version) {
        Ok(ControlResponse::DaemonVersion(version)) => version,
        Ok(ControlResponse::Diag(diag)) => return Err(Box::new(DiagError(diag))),
        Ok(other) => {
            return Err(io::Error::other(format!(
                "unexpected supervisor version response: {other:?}"
            ))
            .into());
        }
        Err(err) => return Err(err.into()),
    };
    if resident == target.version.to_string() {
        return Ok(format!("Supervisor is already running {resident}"));
    }
    systemg::upgrade::validate_resident_version(&resident, &target).map_err(DiagError)?;
    let original_pid = ipc::supervisor_peer_pid()?;
    let expected = match ipc::send_command(&ControlCommand::Upgrade { binary }) {
        Ok(ControlResponse::UpgradeAccepted { version }) => version,
        Ok(ControlResponse::Diag(diag)) => return Err(Box::new(DiagError(diag))),
        Ok(ControlResponse::Error(message)) => {
            return Err(ControlError::Server(message).into());
        }
        Ok(other) => {
            return Err(io::Error::other(format!(
                "unexpected supervisor upgrade response: {other:?}"
            ))
            .into());
        }
        Err(err) => return Err(err.into()),
    };

    let deadline = Instant::now() + UPGRADE_CONFIRM_TIMEOUT;
    let mut observed = resident;
    while Instant::now() < deadline {
        if let Ok(ipc::CommandAck::Response(ControlResponse::DaemonVersion(version))) =
            ipc::send_command_with_timeout(
                &ControlCommand::Version,
                UPGRADE_PROBE_TIMEOUT,
            )
        {
            observed = version;
            if observed == expected {
                let resumed_pid = ipc::supervisor_peer_pid()?;
                if resumed_pid != original_pid {
                    return Err(Box::new(DiagError(Box::new(
                        systemg::upgrade::resume_failed(format!(
                            "supervisor PID changed from {original_pid} to {resumed_pid}"
                        )),
                    ))));
                }
                return Ok(format!("Supervisor upgraded in place to {expected}"));
            }
        }
        thread::sleep(UPGRADE_PROBE_INTERVAL);
    }

    Err(Box::new(DiagError(Box::new(
        systemg::upgrade::resume_failed(format!(
            "expected supervisor {expected}, but resident version is {observed}"
        )),
    ))))
}

/// Sends a control command and optionally renders its response message.
fn send_control_command_inner(
    command: ControlCommand,
    announce: bool,
) -> Result<(), Box<dyn Error>> {
    match ipc::send_command(&command) {
        Ok(ControlResponse::Message(message)) => {
            if announce {
                println!("{message}");
            }
            Ok(())
        }
        Ok(ControlResponse::Ok) => Ok(()),
        Ok(ControlResponse::Status(_)) => Ok(()),
        Ok(ControlResponse::Inspect(_)) => Ok(()),
        Ok(ControlResponse::Spawned { pid }) => {
            println!("Spawned process with PID: {}", pid);
            Ok(())
        }
        Ok(ControlResponse::DaemonVersion(version)) => {
            println!("{version}");
            Ok(())
        }
        Ok(ControlResponse::UpgradeAccepted { version }) => {
            if announce {
                println!("Supervisor accepted live upgrade to {version}");
            }
            Ok(())
        }
        Ok(ControlResponse::Error(message)) => Err(ControlError::Server(message).into()),
        Ok(ControlResponse::Diag(diag)) => Err(Box::new(DiagError(diag))),
        Ok(ControlResponse::CurrentOp(_)) => Ok(()),
        Err(ControlError::NotAvailable) => Err(ControlError::NotAvailable.into()),
        Err(ControlError::Timeout) => Err(supervisor_busy_error().into()),
        Err(err) => Err(err.into()),
    }
}

/// Sends a control command whose successful response must contain a message.
fn send_control_message(command: ControlCommand) -> Result<String, Box<dyn Error>> {
    match ipc::send_command(&command) {
        Ok(ControlResponse::Message(message)) => Ok(message),
        Ok(ControlResponse::Error(message)) => Err(ControlError::Server(message).into()),
        Ok(ControlResponse::Diag(diag)) => Err(Box::new(DiagError(diag))),
        Ok(other) => Err(io::Error::other(format!(
            "unexpected supervisor response: {other:?}"
        ))
        .into()),
        Err(ControlError::NotAvailable) => Err(ControlError::NotAvailable.into()),
        Err(ControlError::Timeout) => Err(supervisor_busy_error().into()),
        Err(err) => Err(err.into()),
    }
}

/// Builds the SG0107 diagnostic for a command the supervisor refused because it
/// was already mid-mutation.
///
/// This is an EXPECTED, recoverable condition — the supervisor serialises
/// mutations through a single owner thread, so a concurrent restart is rejected
/// rather than interleaved. Reporting it as the SG0001 catch-all ("command
/// failed") framed correct, defensive behaviour as an unexplained error, and
/// "command not confirmed" implied uncertainty when the outcome is definite:
/// the command did not run, and the fix is to retry.
fn supervisor_busy_error() -> DiagError {
    use systemg::diag::{Diagnostic, SgCode};

    let mut diag = Diagnostic::error(
        SgCode::SupervisorBusy,
        "the supervisor is busy with another operation",
    );
    diag = match ipc::current_op() {
        Some(op) => diag
            .note(format!("in flight: {}", op.describe()))
            .note("this command was NOT applied; the in-flight operation is unaffected"),
        None => diag
            .note("the supervisor did not answer in time")
            .note("this command was NOT applied"),
    };
    DiagError(Box::new(
        diag.note("retry once the operation above finishes")
            .help_cmd("see what it is doing", "sysg status")
            .help_docs(),
    ))
}

/// Sends daemonized restart and recycles the supervisor when an old IPC schema rejects it.
fn restart_daemonized(
    command: ControlCommand,
    config_path: PathBuf,
    allow_recycle: bool,
) -> Result<(), Box<dyn Error>> {
    match daemon_version_drift() {
        VersionDrift::Drifted(daemon_version) => {
            if allow_recycle {
                return recycle_supervisor_for_restart(config_path);
            }
            eprintln!(
                "{YELLOW}Warn: resident supervisor is v{daemon_version} but this CLI is v{}; run a full `sysg restart` to recycle it{RESET}",
                env!("CARGO_PKG_VERSION")
            );
        }
        VersionDrift::PreVersionDaemon => {
            if allow_recycle {
                return recycle_supervisor_for_restart(config_path);
            }
        }
        VersionDrift::Matched | VersionDrift::Unknown => {}
    }

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

/// Outcome of comparing the resident supervisor's version against this CLI.
enum VersionDrift {
    /// Daemon and CLI run the same version.
    Matched,
    /// Daemon runs a different version (older or newer) than this CLI.
    Drifted(String),
    /// Daemon predates the Version command entirely.
    PreVersionDaemon,
    /// No daemon reachable or it was too busy to answer in time.
    Unknown,
}

/// Asks the resident supervisor for its version and compares it to this CLI's.
fn daemon_version_drift() -> VersionDrift {
    match ipc::send_command_with_timeout(
        &ControlCommand::Version,
        RESTART_DAEMON_ACK_TIMEOUT,
    ) {
        Ok(ipc::CommandAck::Response(ControlResponse::DaemonVersion(version))) => {
            if version == env!("CARGO_PKG_VERSION") {
                VersionDrift::Matched
            } else {
                VersionDrift::Drifted(version)
            }
        }
        Ok(ipc::CommandAck::Response(ControlResponse::Error(message)))
            if supervisor_error_is_protocol_mismatch(&message) =>
        {
            VersionDrift::PreVersionDaemon
        }
        Ok(_) => VersionDrift::Unknown,
        Err(ControlError::Serde(_)) => VersionDrift::PreVersionDaemon,
        Err(_) => VersionDrift::Unknown,
    }
}

/// Stops the resident supervisor and starts a fresh daemon with the requested config.
fn recycle_supervisor_for_restart(config_path: PathBuf) -> Result<(), Box<dyn Error>> {
    warn!(
        "Resident supervisor does not match this CLI (version drift or rejected IPC); recycling supervisor with config {:?}",
        config_path
    );

    // Validate the replacement config BEFORE tearing down the working supervisor.
    // Recycling that stops the old daemon and then fails to load the new config
    // leaves the box with no supervisor at all — never trade a running stack for
    // an unvalidated one.
    if let Err(err) = load_config(Some(config_path.to_string_lossy().as_ref())) {
        return Err(Box::new(DiagError(Box::new(
            systemg::restart::recycle_refused(&config_path, err.to_string()),
        ))));
    }

    stop_supervisors()?;
    wait_for_runtime_cleared(SUPERVISOR_RUNTIME_TIMEOUT);
    cleanup_stopped_runtime();
    let recovery_path = config_path.clone();
    start_supervisor_daemon(config_path, None, false, false).map_err(|err| {
        Box::new(DiagError(Box::new(systemg::restart::recycle_failed(
            &recovery_path,
            err.to_string(),
        )))) as Box<dyn Error>
    })
}

fn control_error_is_restart_upgrade_boundary(err: &ControlError) -> bool {
    match err {
        ControlError::Serde(_) | ControlError::NotAvailable => true,
        ControlError::Server(message) => supervisor_error_is_protocol_mismatch(message),
        ControlError::MissingHome
        | ControlError::Unauthorized(_)
        | ControlError::Timeout
        | ControlError::RuntimeBusy => false,
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

/// Detaches the current (already-forked) process into a daemon: new session,
/// root cwd, and stdio pointed at /dev/null. Does not fork again, so the caller's
/// child PID stays the supervisor's PID and boot-failure detection stays correct.
fn detach_daemon() -> std::io::Result<()> {
    unsafe {
        libc::setsid();
        libc::setpgid(0, 0);
    }

    std::env::set_current_dir("/")?;
    silence_stdio()
}

/// Redirects inherited terminal descriptors away from a resident supervisor.
fn silence_stdio() -> std::io::Result<()> {
    let devnull = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    let fd = devnull.into_raw_fd();
    unsafe {
        let _ = libc::dup2(fd, libc::STDIN_FILENO);
        let _ = libc::dup2(fd, libc::STDOUT_FILENO);
        let _ = libc::dup2(fd, libc::STDERR_FILENO);
        libc::close(fd);
    }

    Ok(())
}
