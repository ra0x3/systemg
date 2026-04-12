/// Represents status render options.
struct StatusRenderOptions<'a> {
    json: bool,
    no_color: bool,
    #[allow(dead_code)]
    full_cmd: bool,
    include_orphans: bool,
    service_filter: Option<&'a str>,
}

/// Represents inspect render options.
struct InspectRenderOptions {
    json: bool,
    no_color: bool,
    window_seconds: u64,
    window_desc: String,
    samples_limit: usize,
}

const GREEN_BOLD: &str = "\x1b[1;32m";
const GREEN: &str = "\x1b[32m";
const DARK_GREEN: &str = "\x1b[38;5;22m";
const BRIGHT_GREEN: &str = "\x1b[92m";
const YELLOW_BOLD: &str = "\x1b[1;33m";
const RED_BOLD: &str = "\x1b[1;31m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const ORANGE: &str = "\x1b[38;5;208m";
const GRAY: &str = "\x1b[90m";
const MID_GRAY: &str = "\x1b[38;5;245m";
const DARK_GRAY: &str = "\x1b[38;5;242m";
const DEEP_GRAY: &str = "\x1b[38;5;239m";
const WHITE: &str = "\x1b[37m";
const BRIGHT_WHITE: &str = "\x1b[97m";
const DIM_WHITE: &str = "\x1b[2;37m";
const DIM_CYAN: &str = "\x1b[2;36m";
const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy)]
/// Defines alignment values.
enum Alignment {
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy)]
/// Represents column.
struct Column {
    title: &'static str,
    width: usize,
    align: Alignment,
}

/// Fetches a status snapshot from the live supervisor, falling back to the
/// persisted on-disk snapshot when no supervisor is available.
fn fetch_status_snapshot(config_path: &str) -> Result<StatusSnapshot, Box<dyn Error>> {
    match ipc::send_command(&ControlCommand::Status) {
        Ok(ControlResponse::Status(snapshot)) => Ok(snapshot),
        Ok(other) => Err(io::Error::other(format!(
            "unexpected supervisor response: {:?}",
            other
        ))
        .into()),
        Err(ControlError::NotAvailable) => {
            let config = match load_config(Some(config_path)) {
                Ok(config) => Some(config),
                Err(primary_err) => {
                    if let Ok(Some(hint)) = ipc::read_config_hint() {
                        let hint_path = hint.to_string_lossy().to_string();
                        match load_config(Some(&hint_path)) {
                            Ok(config) => Some(config),
                            Err(hint_err) => {
                                return Err(io::Error::other(format!(
                                    "failed to load config '{config_path}' ({primary_err}); fallback config hint '{hint_path}' also failed ({hint_err})"
                                ))
                                .into());
                            }
                        }
                    } else {
                        return Err(primary_err.into());
                    }
                }
            };
            collect_disk_snapshot(config).map_err(|err| Box::new(err) as Box<dyn Error>)
        }
        Err(err) => Err(Box::new(err)),
    }
}

/// Returns the PID for a unit only when its process is currently running.
fn live_unit_pid(unit: &UnitStatus) -> Option<u32> {
    unit.process.as_ref().and_then(|process| {
        if matches!(process.state, ProcessState::Running) {
            Some(process.pid)
        } else {
            None
        }
    })
}

/// Renders logs for a single unit using the same status snapshot data that
/// powers `sysg status` and `sysg inspect`.
fn render_service_logs_from_snapshot(
    manager: &LogManager,
    snapshot: &StatusSnapshot,
    service_name: &str,
    lines: usize,
    kind: &str,
    snapshot_mode: bool,
) -> Result<(), Box<dyn Error>> {
    let unit = snapshot
        .units
        .iter()
        .find(|unit| unit.name == service_name || unit.hash == service_name);

    if let Some(unit) = unit {
        if let Some(process_pid) = live_unit_pid(unit) {
            if snapshot_mode {
                manager.show_log_snapshot(
                    service_name,
                    process_pid,
                    lines,
                    Some(kind),
                )?;
            } else {
                manager.show_log(service_name, process_pid, lines, Some(kind))?;
            }
            return Ok(());
        }

        if snapshot_mode {
            manager.show_inactive_log_snapshot(service_name, lines, Some(kind))?;
        } else {
            manager.show_inactive_log(service_name, lines, Some(kind))?;
        }
        return Ok(());
    }

    let cron_state = CronStateFile::load().unwrap_or_default();
    let stdout_exists = resolve_log_path(service_name, "stdout").exists();
    let stderr_exists = resolve_log_path(service_name, "stderr").exists();

    if cron_state.jobs().contains_key(service_name) || stdout_exists || stderr_exists {
        if snapshot_mode {
            manager.show_inactive_log_snapshot(service_name, lines, Some(kind))?;
        } else {
            manager.show_inactive_log(service_name, lines, Some(kind))?;
        }
    } else {
        warn!("Service '{service_name}' is not currently running");
    }

    Ok(())
}

/// Renders logs for every non-orphaned unit represented in the current status
/// snapshot.
fn render_all_logs_from_snapshot(
    manager: &LogManager,
    snapshot: &StatusSnapshot,
    lines: usize,
    kind: &str,
    snapshot_mode: bool,
) -> Result<(), Box<dyn Error>> {
    let mut unit_names: Vec<&str> = snapshot
        .units
        .iter()
        .filter(|unit| !matches!(unit.kind, UnitKind::Orphaned))
        .map(|unit| unit.name.as_str())
        .collect();
    unit_names.sort_unstable();
    unit_names.dedup();

    if unit_names.is_empty() {
        println!("No active services");
        return Ok(());
    }

    for service_name in unit_names {
        render_service_logs_from_snapshot(
            manager,
            snapshot,
            service_name,
            lines,
            kind,
            snapshot_mode,
        )?;
    }

    Ok(())
}

/// Clears the current terminal contents and repositions the cursor at the
/// origin.
fn clear_terminal_output() -> io::Result<()> {
    print!("\x1B[2J\x1B[H");
    io::stdout().flush()
}

/// Reserves three quarters of the available terminal width for status and
/// inspect tables.
fn target_table_width(terminal_width: usize) -> usize {
    terminal_width.saturating_mul(3).saturating_div(4).max(1)
}

/// Detects the current terminal width and falls back to a caller-provided
/// default when the terminal size is unavailable.
fn detect_target_table_width(default_terminal_width: usize) -> usize {
    let terminal_width = terminal_size::terminal_size()
        .map(|(width, _)| width.0 as usize)
        .unwrap_or(default_terminal_width);
    target_table_width(terminal_width)
}

const STATUS_COLUMN_COUNT: usize = 11;
const STATUS_COL_UNIT: usize = 0;
const STATUS_COL_KIND: usize = 1;
const STATUS_COL_STATE: usize = 2;
const STATUS_COL_USER: usize = 3;
const STATUS_COL_PID: usize = 4;
const STATUS_COL_CPU: usize = 5;
const STATUS_COL_RSS: usize = 6;
const STATUS_COL_UPTIME: usize = 7;
const STATUS_COL_CMD: usize = 8;
const STATUS_COL_LAST_EXIT: usize = 9;
const STATUS_COL_HEALTH: usize = 10;

const STATUS_COLUMN_TITLES: [&str; STATUS_COLUMN_COUNT] = [
    "UNIT",
    "KIND",
    "STATE",
    "USER",
    "PID",
    "CPU",
    "RSS",
    "UPTIME",
    "CMD",
    "LAST_EXIT",
    "HEALTH",
];

const STATUS_COLUMN_ALIGNS: [Alignment; STATUS_COLUMN_COUNT] = [
    Alignment::Left,
    Alignment::Left,
    Alignment::Left,
    Alignment::Left,
    Alignment::Right,
    Alignment::Right,
    Alignment::Right,
    Alignment::Left,
    Alignment::Left,
    Alignment::Left,
    Alignment::Left,
];

const STATUS_SOFT_MIN_WIDTHS: [usize; STATUS_COLUMN_COUNT] =
    [12, 4, 5, 4, 3, 3, 3, 4, 12, 9, 6];
const STATUS_SHRINK_PRIORITY: [usize; STATUS_COLUMN_COUNT] =
    [8, 9, 3, 2, 7, 1, 10, 0, 6, 5, 4];
const STATUS_UNIT_CMD_MAX_DIFF: usize = 4;

#[cfg(test)]
fn status_row_width(content_widths: &[usize; STATUS_COLUMN_COUNT]) -> usize {
    content_widths.iter().sum::<usize>() + (3 * STATUS_COLUMN_COUNT) + 1
}

/// Builds the status information for content budget.
fn status_content_budget(terminal_width: usize) -> usize {
    terminal_width.saturating_sub((3 * STATUS_COLUMN_COUNT) + 1)
}

/// Shrinks status widths to fit.
fn shrink_status_widths_to_fit(
    widths: &mut [usize; STATUS_COLUMN_COUNT],
    terminal_width: usize,
) {
    let budget = status_content_budget(terminal_width);

    if widths.iter().sum::<usize>() <= budget {
        return;
    }

    reduce_status_widths(widths, &STATUS_SOFT_MIN_WIDTHS, budget);

    if widths.iter().sum::<usize>() <= budget {
        rebalance_status_unit_cmd_widths(widths);
        return;
    }

    reduce_status_widths(widths, &[1; STATUS_COLUMN_COUNT], budget);
    rebalance_status_unit_cmd_widths(widths);
}

/// Reduces status widths.
fn reduce_status_widths(
    widths: &mut [usize; STATUS_COLUMN_COUNT],
    min_widths: &[usize; STATUS_COLUMN_COUNT],
    budget: usize,
) {
    loop {
        let mut total = widths.iter().sum::<usize>();
        if total <= budget {
            break;
        }

        let mut changed = false;
        for index in STATUS_SHRINK_PRIORITY {
            if total <= budget {
                break;
            }

            if widths[index] <= min_widths[index] {
                continue;
            }

            let reducible = widths[index] - min_widths[index];
            let needed = total - budget;
            let delta = reducible.min(needed);
            widths[index] -= delta;
            total -= delta;
            changed = true;
        }

        if !changed {
            break;
        }
    }
}

/// Rebalances status table widths so UNIT and CMD stay close in visible width.
fn rebalance_status_unit_cmd_widths(widths: &mut [usize; STATUS_COLUMN_COUNT]) {
    let unit = STATUS_COL_UNIT;
    let cmd = STATUS_COL_CMD;

    if widths[cmd] > widths[unit] + STATUS_UNIT_CMD_MAX_DIFF {
        let diff = widths[cmd] - widths[unit] - STATUS_UNIT_CMD_MAX_DIFF;
        let needed = diff.div_ceil(2);
        let cmd_floor = STATUS_SOFT_MIN_WIDTHS[cmd].max(STATUS_COLUMN_TITLES[cmd].len());
        let transfer = needed.min(widths[cmd].saturating_sub(cmd_floor));
        widths[cmd] -= transfer;
        widths[unit] += transfer;
    } else if widths[unit] > widths[cmd] + STATUS_UNIT_CMD_MAX_DIFF {
        let diff = widths[unit] - widths[cmd] - STATUS_UNIT_CMD_MAX_DIFF;
        let needed = diff.div_ceil(2);
        let unit_floor =
            STATUS_SOFT_MIN_WIDTHS[unit].max(STATUS_COLUMN_TITLES[unit].len());
        let transfer = needed.min(widths[unit].saturating_sub(unit_floor));
        widths[unit] -= transfer;
        widths[cmd] += transfer;
    }
}

/// Computes status preferred widths.
fn compute_status_preferred_widths(
    units: &[UnitStatus],
    no_color: bool,
) -> [usize; STATUS_COLUMN_COUNT] {
    let mut widths = STATUS_COLUMN_TITLES.map(visible_length);

    for unit in units {
        widths[STATUS_COL_UNIT] = widths[STATUS_COL_UNIT].max(visible_length(&unit.name));
        widths[STATUS_COL_KIND] = widths[STATUS_COL_KIND].max(4);
        widths[STATUS_COL_STATE] = widths[STATUS_COL_STATE]
            .max(visible_length(&unit_state_label(unit, no_color)));
        widths[STATUS_COL_USER] = widths[STATUS_COL_USER].max(visible_length(
            unit.process
                .as_ref()
                .and_then(|runtime| runtime.user.as_deref())
                .unwrap_or("-"),
        ));
        widths[STATUS_COL_PID] = widths[STATUS_COL_PID].max(visible_length(
            &unit
                .process
                .as_ref()
                .map(|runtime| runtime.pid.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ));
        widths[STATUS_COL_CPU] = widths[STATUS_COL_CPU]
            .max(visible_length(&format_cpu_column(unit.metrics.as_ref())));
        widths[STATUS_COL_RSS] = widths[STATUS_COL_RSS]
            .max(visible_length(&format_rss_column(unit.metrics.as_ref())));
        widths[STATUS_COL_UPTIME] = widths[STATUS_COL_UPTIME]
            .max(visible_length(&format_uptime_column(unit.uptime.as_ref())));
        widths[STATUS_COL_CMD] = widths[STATUS_COL_CMD].max(visible_length(
            unit.command
                .as_ref()
                .or(unit.runtime_command.as_ref())
                .map(|value| value.as_str())
                .unwrap_or("-"),
        ));
        widths[STATUS_COL_LAST_EXIT] = widths[STATUS_COL_LAST_EXIT].max(visible_length(
            &format_last_exit(unit.last_exit.as_ref(), unit.cron.as_ref()),
        ));
        widths[STATUS_COL_HEALTH] =
            widths[STATUS_COL_HEALTH].max(visible_length(&health_label_extended(unit)));

        visit_spawn_tree(&unit.spawned_children, "", &mut |child, prefix, _| {
            let label = format!("{prefix}{}", child.name);
            widths[STATUS_COL_UNIT] = widths[STATUS_COL_UNIT].max(visible_length(&label));
            widths[STATUS_COL_USER] = widths[STATUS_COL_USER]
                .max(visible_length(child.user.as_deref().unwrap_or("-")));
            widths[STATUS_COL_PID] =
                widths[STATUS_COL_PID].max(visible_length(&child.pid.to_string()));
            let cpu = child
                .cpu_percent
                .map(|value| format!("{value:.1}%"))
                .unwrap_or_else(|| "-".to_string());
            widths[STATUS_COL_CPU] = widths[STATUS_COL_CPU].max(visible_length(&cpu));
            let rss = child
                .rss_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "-".to_string());
            widths[STATUS_COL_RSS] = widths[STATUS_COL_RSS].max(visible_length(&rss));
            widths[STATUS_COL_CMD] =
                widths[STATUS_COL_CMD].max(visible_length(&child.command));
            widths[STATUS_COL_LAST_EXIT] = widths[STATUS_COL_LAST_EXIT]
                .max(visible_length(&format_spawn_exit(child.last_exit.as_ref())));
            let health = if let Some(exit) = &child.last_exit {
                let succeeded = exit.exit_code.map(|code| code == 0).unwrap_or(false)
                    && exit.signal.is_none();
                if succeeded { "Healthy" } else { "Failing" }
            } else {
                "Healthy"
            };
            widths[STATUS_COL_HEALTH] =
                widths[STATUS_COL_HEALTH].max(visible_length(health));
        });
    }

    widths
}

/// Renders the status table in interactive mode with keyboard navigation.
fn render_status_interactive(
    snapshot: &StatusSnapshot,
    opts: &StatusRenderOptions,
    config_path: &str,
) -> Result<OverallHealth, Box<dyn Error>> {
    let mut selected_row: usize = 0;
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

    let is_tty = unsafe {
        libc::isatty(libc::STDIN_FILENO) == 1 && libc::isatty(libc::STDOUT_FILENO) == 1
    };

    if !is_tty {
        return render_status_non_interactive(snapshot, opts, false);
    }

    render_status_table_with_selection(snapshot, &units, opts, selected_row, health)?;

    terminal::enable_raw_mode()?;

    let result = (|| -> Result<OverallHealth, Box<dyn Error>> {
        loop {
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key_event) = event::read()?
            {
                match key_event {
                    KeyEvent {
                        code: KeyCode::Tab, ..
                    } if key_event.modifiers == KeyModifiers::NONE
                        || key_event.modifiers == KeyModifiers::empty() =>
                    {
                        if selected_row < units.len() - 1 {
                            selected_row += 1;
                            terminal::disable_raw_mode()?;
                            clear_terminal_output()?;
                            render_status_table_with_selection(
                                snapshot,
                                &units,
                                opts,
                                selected_row,
                                health,
                            )?;
                            terminal::enable_raw_mode()?;
                        }
                    }
                    KeyEvent {
                        code: KeyCode::Down,
                        ..
                    } => {
                        if selected_row < units.len() - 1 {
                            selected_row += 1;
                            terminal::disable_raw_mode()?;
                            clear_terminal_output()?;
                            render_status_table_with_selection(
                                snapshot,
                                &units,
                                opts,
                                selected_row,
                                health,
                            )?;
                            terminal::enable_raw_mode()?;
                        }
                    }
                    KeyEvent {
                        code: KeyCode::BackTab,
                        ..
                    }
                    | KeyEvent {
                        code: KeyCode::Tab,
                        modifiers: KeyModifiers::SHIFT,
                        ..
                    } => {
                        let new_row = selected_row.saturating_sub(1);
                        if new_row != selected_row {
                            selected_row = new_row;
                            terminal::disable_raw_mode()?;
                            clear_terminal_output()?;
                            render_status_table_with_selection(
                                snapshot,
                                &units,
                                opts,
                                selected_row,
                                health,
                            )?;
                            terminal::enable_raw_mode()?;
                        }
                    }
                    KeyEvent {
                        code: KeyCode::Up, ..
                    } => {
                        let new_row = selected_row.saturating_sub(1);
                        if new_row != selected_row {
                            selected_row = new_row;
                            terminal::disable_raw_mode()?;
                            clear_terminal_output()?;
                            render_status_table_with_selection(
                                snapshot,
                                &units,
                                opts,
                                selected_row,
                                health,
                            )?;
                            terminal::enable_raw_mode()?;
                        }
                    }
                    KeyEvent {
                        code: KeyCode::Enter,
                        ..
                    } => {
                        if !units.is_empty() {
                            let selected_unit = &units[selected_row];
                            terminal::disable_raw_mode()?;

                            let current_exe = std::env::current_exe()
                                .unwrap_or_else(|_| PathBuf::from("sysg"));

                            let _ = process::Command::new(&current_exe)
                                .arg("inspect")
                                .arg("--config")
                                .arg(config_path)
                                .arg("--service")
                                .arg(&selected_unit.name)
                                .stdin(process::Stdio::inherit())
                                .stdout(process::Stdio::inherit())
                                .stderr(process::Stdio::inherit())
                                .status();

                            println!("\n\nPress any key to return to status view...");

                            terminal::enable_raw_mode()?;
                            let _ = event::read();
                            terminal::disable_raw_mode()?;

                            clear_terminal_output()?;
                            render_status_table_with_selection(
                                snapshot,
                                &units,
                                opts,
                                selected_row,
                                health,
                            )?;
                            terminal::enable_raw_mode()?;
                        }
                    }
                    KeyEvent {
                        code: KeyCode::Char('q'),
                        ..
                    }
                    | KeyEvent {
                        code: KeyCode::Esc, ..
                    } => {
                        clear_terminal_output()?;
                        return Ok(health);
                    }
                    _ => {}
                }
            }
        }
    })();

    terminal::disable_raw_mode()?;
    result
}

/// Renders the status table with a selected row highlighted.
fn render_status_table_with_selection(
    _snapshot: &StatusSnapshot,
    units: &[UnitStatus],
    opts: &StatusRenderOptions,
    selected_row: usize,
    health: OverallHealth,
) -> Result<(), Box<dyn Error>> {
    let terminal_width = detect_target_table_width(120);
    let mut widths = compute_status_preferred_widths(units, opts.no_color);
    shrink_status_widths_to_fit(&mut widths, terminal_width);

    let columns_array = [
        Column {
            title: "UNIT",
            width: widths[STATUS_COL_UNIT],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_UNIT],
        },
        Column {
            title: "KIND",
            width: widths[STATUS_COL_KIND],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_KIND],
        },
        Column {
            title: "STATE",
            width: widths[STATUS_COL_STATE],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_STATE],
        },
        Column {
            title: "USER",
            width: widths[STATUS_COL_USER],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_USER],
        },
        Column {
            title: "PID",
            width: widths[STATUS_COL_PID],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_PID],
        },
        Column {
            title: "CPU",
            width: widths[STATUS_COL_CPU],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_CPU],
        },
        Column {
            title: "RSS",
            width: widths[STATUS_COL_RSS],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_RSS],
        },
        Column {
            title: "UPTIME",
            width: widths[STATUS_COL_UPTIME],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_UPTIME],
        },
        Column {
            title: "CMD",
            width: widths[STATUS_COL_CMD],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_CMD],
        },
        Column {
            title: "LAST_EXIT",
            width: widths[STATUS_COL_LAST_EXIT],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_LAST_EXIT],
        },
        Column {
            title: "HEALTH",
            width: widths[STATUS_COL_HEALTH],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_HEALTH],
        },
    ];

    let columns = &columns_array;
    let (state_counts, health_counts) = count_states_and_health(units);
    println!("{}", make_overview_top_border(columns));
    println!("{}", format_overview_line(" Overview", columns));
    println!("{}", make_overview_separator_border(columns));
    println!(
        "{}",
        format_overview_line(
            &format!(
                " Overall Health: {}",
                colorize(
                    overall_health_label(health),
                    overall_health_color(health),
                    opts.no_color
                )
            ),
            columns,
        )
    );
    println!(
        "{}",
        format_overview_line(
            &format!(
                " {}",
                format_flattened_breakdown(&state_counts, &health_counts, opts.no_color)
            ),
            columns,
        )
    );

    println!("{}", make_separator_border(columns));
    println!("{}", format_header_row(columns));
    println!("{}", make_separator_border(columns));

    for (index, unit) in units.iter().enumerate() {
        if index == selected_row {
            print!("\x1b[47m\x1b[30m");
            let row_content = format_unit_row(unit, columns, true);
            print!("{}", row_content);
            println!("\x1b[0m");
        } else {
            let row_content = format_unit_row(unit, columns, opts.no_color);
            println!("{}", row_content);
        }

        if !unit.spawned_children.is_empty() {
            render_spawn_rows(&unit.spawned_children, columns, opts.no_color);
        }
    }

    println!("{}", make_separator_border(columns));
    println!("{}", make_bottom_border(columns));

    println!(
        "\nTab/↓\x1b[41;97m Next \x1b[0m  Shift+Tab/↑\x1b[41;97m Prev \x1b[0m  Enter\x1b[41;97m Inspect \x1b[0m  q/ESC\x1b[41;97m Quit \x1b[0m"
    );

    Ok(())
}

/// Main status rendering function that delegates to interactive or non-interactive mode.
/// Uses interactive mode by default when stdout/stdin are TTYs, otherwise falls back to non-interactive.
fn render_status(
    snapshot: &StatusSnapshot,
    opts: &StatusRenderOptions,
    watch_mode: bool,
    config_path: &str,
) -> Result<OverallHealth, Box<dyn Error>> {
    if watch_mode || opts.json {
        render_status_non_interactive(snapshot, opts, watch_mode)
    } else {
        render_status_interactive(snapshot, opts, config_path)
    }
}

/// Renders the status table in non-interactive mode using standard terminal output.
#[allow(dead_code)]
fn render_status_non_interactive(
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

    let terminal_width = detect_target_table_width(120);
    let mut widths = compute_status_preferred_widths(&units, opts.no_color);
    shrink_status_widths_to_fit(&mut widths, terminal_width);

    let columns_array = [
        Column {
            title: "UNIT",
            width: widths[STATUS_COL_UNIT],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_UNIT],
        },
        Column {
            title: "KIND",
            width: widths[STATUS_COL_KIND],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_KIND],
        },
        Column {
            title: "STATE",
            width: widths[STATUS_COL_STATE],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_STATE],
        },
        Column {
            title: "USER",
            width: widths[STATUS_COL_USER],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_USER],
        },
        Column {
            title: "PID",
            width: widths[STATUS_COL_PID],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_PID],
        },
        Column {
            title: "CPU",
            width: widths[STATUS_COL_CPU],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_CPU],
        },
        Column {
            title: "RSS",
            width: widths[STATUS_COL_RSS],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_RSS],
        },
        Column {
            title: "UPTIME",
            width: widths[STATUS_COL_UPTIME],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_UPTIME],
        },
        Column {
            title: "CMD",
            width: widths[STATUS_COL_CMD],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_CMD],
        },
        Column {
            title: "LAST_EXIT",
            width: widths[STATUS_COL_LAST_EXIT],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_LAST_EXIT],
        },
        Column {
            title: "HEALTH",
            width: widths[STATUS_COL_HEALTH],
            align: STATUS_COLUMN_ALIGNS[STATUS_COL_HEALTH],
        },
    ];

    let columns = &columns_array;
    let (state_counts, health_counts) = count_states_and_health(&units);
    println!("{}", make_overview_top_border(columns));
    println!("{}", format_overview_line(" Overview", columns));
    println!("{}", make_overview_separator_border(columns));
    println!(
        "{}",
        format_overview_line(
            &format!(
                " Overall Health: {}",
                colorize(
                    overall_health_label(health),
                    overall_health_color(health),
                    opts.no_color
                )
            ),
            columns,
        )
    );
    println!(
        "{}",
        format_overview_line(
            &format!(
                " {}",
                format_flattened_breakdown(&state_counts, &health_counts, opts.no_color)
            ),
            columns,
        )
    );

    println!("{}", make_separator_border(columns));
    println!("{}", format_header_row(columns));
    println!("{}", make_separator_border(columns));

    for unit in &units {
        println!("{}", format_unit_row(unit, columns, opts.no_color));
        if !unit.spawned_children.is_empty() {
            render_spawn_rows(&unit.spawned_children, columns, opts.no_color);
        }
    }

    println!("{}", make_separator_border(columns));
    println!("{}", make_bottom_border(columns));

    let _ = io::stdout().flush();
    Ok(health)
}

/// Applies color to this item.
fn colorize(text: &str, color: &str, no_color: bool) -> String {
    if no_color {
        text.to_string()
    } else {
        format!("{}{}{}", color, text, RESET)
    }
}

/// Returns the overall health label.
fn overall_health_label(health: OverallHealth) -> &'static str {
    match health {
        OverallHealth::Healthy => "Healthy",
        OverallHealth::Degraded => "Degraded",
        OverallHealth::Failing => "Failing",
    }
}

/// Returns the overall health color.
fn overall_health_color(health: OverallHealth) -> &'static str {
    match health {
        OverallHealth::Healthy => GREEN_BOLD,
        OverallHealth::Degraded => ORANGE,
        OverallHealth::Failing => RED_BOLD,
    }
}

/// Builds the unit health label.
fn unit_health_label(health: UnitHealth) -> &'static str {
    match health {
        UnitHealth::Healthy => "Healthy",
        UnitHealth::Degraded => "Degraded",
        UnitHealth::Failing => "Failing",
        UnitHealth::Inactive => "Healthy",
    }
}

/// Handles health label extended.
fn health_label_extended(unit: &UnitStatus) -> String {
    unit_health_label(unit.health).to_string()
}

/// Builds the unit health color.
fn unit_health_color(health: UnitHealth) -> &'static str {
    match health {
        UnitHealth::Healthy => GREEN_BOLD,
        UnitHealth::Degraded => ORANGE,
        UnitHealth::Failing => RED_BOLD,
        UnitHealth::Inactive => GREEN_BOLD,
    }
}

/// Builds the unit state label.
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
                    CronExecutionStatus::Success => match last.exit_code {
                        Some(0) => colorize("Idle", GRAY, no_color),
                        Some(_) => colorize("OkWithErr", DARK_GREEN, no_color),
                        None => colorize("Idle", GRAY, no_color),
                    },
                    CronExecutionStatus::Failed(reason) => {
                        if reason.contains("Failed to get PID") {
                            colorize("Idle", GRAY, no_color)
                        } else if let Some(exit_code) = last.exit_code {
                            if exit_code == 0 {
                                colorize("PartialSuccess", DARK_GREEN, no_color)
                            } else {
                                colorize("Failed", RED, no_color)
                            }
                        } else {
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

/// Formats uptime column.
fn format_uptime_column(uptime: Option<&UptimeInfo>) -> String {
    if let Some(info) = uptime {
        format_uptime_short(&info.human)
    } else {
        "-".to_string()
    }
}

/// Formats uptime short.
fn format_uptime_short(uptime: &str) -> String {
    if uptime.contains("secs ago") {
        "< 1m".to_string()
    } else if let Some(mins) = extract_time_value(uptime, "mins ago") {
        if mins < 60 {
            format!("{}m", mins)
        } else {
            format!("{}h", mins / 60)
        }
    } else if let Some(hours) = extract_time_value(uptime, "hours ago") {
        if hours < 24 {
            format!("{}h", hours)
        } else {
            format!("{}d", hours / 24)
        }
    } else if let Some(days) = extract_time_value(uptime, "days ago") {
        format!("{}d", days)
    } else if let Some(weeks) = extract_time_value(uptime, "weeks ago") {
        format!("{}w", weeks)
    } else {
        uptime.to_string()
    }
}

/// Formats inspect elapsed.
fn format_inspect_elapsed(seconds: u64) -> String {
    let rendered = format_elapsed(seconds);
    rendered
        .strip_suffix(" ago")
        .unwrap_or(&rendered)
        .to_string()
}

/// Extracts time value.
fn extract_time_value(uptime: &str, suffix: &str) -> Option<u64> {
    if uptime.ends_with(suffix) {
        let num_str = uptime.trim_end_matches(suffix).trim();
        num_str.parse().ok()
    } else {
        None
    }
}

/// Formats duration.
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

#[allow(dead_code)]
/// Formats relative time.
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

/// Formats last exit.
fn format_last_exit(
    exit: Option<&ExitMetadata>,
    cron: Option<&CronUnitStatus>,
) -> String {
    if let Some(cron) = cron
        && let Some(last) = &cron.last_run
    {
        let time_str = if let Some(completed_at) = last.completed_at {
            format_relative_time_short(completed_at)
        } else if last.status.is_none() {
            "".to_string()
        } else {
            format_relative_time_short(last.started_at)
        };

        return match &last.status {
            Some(CronExecutionStatus::Success) => {
                if let Some(code) = last.exit_code {
                    if time_str.is_empty() {
                        format!("exit {}", code)
                    } else {
                        format!("exit {}; {}", code, time_str)
                    }
                } else if time_str.is_empty() {
                    "ok".to_string()
                } else {
                    format!("ok {}", time_str)
                }
            }
            Some(CronExecutionStatus::Failed(reason)) => {
                if let Some(code) = last.exit_code {
                    if time_str.is_empty() {
                        format!("exit {}", code)
                    } else {
                        format!("exit {}; {}", code, time_str)
                    }
                } else if reason.is_empty() {
                    if time_str.is_empty() {
                        "failed".to_string()
                    } else {
                        format!("fail {}", time_str)
                    }
                } else {
                    let short_reason = if reason.contains("signal") {
                        "sig"
                    } else if reason.contains("Failed to start") {
                        "start"
                    } else if reason.contains("Failed to get PID") {
                        "pid"
                    } else {
                        "err"
                    };
                    if time_str.is_empty() {
                        short_reason.to_string()
                    } else {
                        format!("{} {}", short_reason, time_str)
                    }
                }
            }
            Some(CronExecutionStatus::OverlapError) => {
                if time_str.is_empty() {
                    "overlap".to_string()
                } else {
                    format!("ovlp {}", time_str)
                }
            }
            None => "running".to_string(),
        };
    }

    match exit {
        Some(metadata) => match (metadata.exit_code, metadata.signal) {
            (Some(code), _) => format!("exit {}", code),
            (None, Some(_)) => "exit ?".to_string(),
            _ => "?".to_string(),
        },
        None => "-".to_string(),
    }
}

/// Chooses display color for `LAST_EXIT` based on exit outcome semantics.
fn last_exit_color(
    exit: Option<&ExitMetadata>,
    cron: Option<&CronUnitStatus>,
) -> Option<&'static str> {
    if let Some(cron) = cron
        && let Some(last) = &cron.last_run
    {
        return match &last.status {
            Some(CronExecutionStatus::Success) => last
                .exit_code
                .map(|code| if code == 0 { GREEN_BOLD } else { RED_BOLD }),
            Some(CronExecutionStatus::Failed(_)) => {
                if let Some(code) = last.exit_code {
                    Some(if code == 0 { GREEN_BOLD } else { RED_BOLD })
                } else {
                    Some(RED_BOLD)
                }
            }
            Some(CronExecutionStatus::OverlapError) => Some(RED_BOLD),
            None => None,
        };
    }

    match exit {
        Some(metadata) => match (metadata.exit_code, metadata.signal) {
            (Some(code), _) => Some(if code == 0 { GREEN_BOLD } else { RED_BOLD }),
            (None, Some(_)) => Some(RED_BOLD),
            _ => None,
        },
        None => None,
    }
}

/// Formats relative time short.
fn format_relative_time_short(from: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(from);

    if duration.num_seconds() < 60 {
        "<1m".to_string()
    } else if duration.num_minutes() < 60 {
        format!("{}m", duration.num_minutes())
    } else if duration.num_hours() < 24 {
        format!("{}h", duration.num_hours())
    } else if duration.num_days() < 7 {
        format!("{}d", duration.num_days())
    } else {
        format!("{}w", duration.num_weeks())
    }
}

/// Returns the total inner width.
fn total_inner_width(columns: &[Column]) -> usize {
    let base: usize = columns.iter().map(|c| c.width + 2).sum();
    base + columns.len().saturating_sub(1)
}

/// Builds top border.
fn make_top_border(columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    format!("╭{}╮", "─".repeat(inner_width))
}

/// Builds overview top border.
fn make_overview_top_border(columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    format!("╔{}╗", "═".repeat(inner_width))
}

/// Builds overview separator border.
fn make_overview_separator_border(columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    format!("╟{}╢", "─".repeat(inner_width))
}

/// Formats overview line.
fn format_overview_line(text: &str, columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    let content_width = inner_width.saturating_sub(2);
    format!("║ {} ║", ansi_pad(text, content_width, Alignment::Left))
}

/// Builds bottom border.
fn make_bottom_border(columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    format!("╰{}╯", "─".repeat(inner_width))
}

/// Builds separator border.
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

/// Formats banner.
fn format_banner(text: &str, columns: &[Column]) -> String {
    let inner_width = total_inner_width(columns);
    let content = ansi_pad(text, inner_width, Alignment::Center);
    format!("│{}│", content)
}

/// Counts states and health.
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

#[allow(dead_code)]
/// Formats breakdown banner.
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
            let color = if health.as_str() == "Healthy" {
                GREEN_BOLD
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

/// Formats flattened breakdown.
fn format_flattened_breakdown(
    state_counts: &HashMap<String, usize>,
    health_counts: &HashMap<String, usize>,
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
            format!("{} {}", colorize(state, color, no_color), count)
        })
        .collect::<Vec<_>>()
        .join("  ");

    let mut healths: Vec<_> = health_counts.iter().collect();
    healths.sort_by_key(|(k, _)| k.as_str());
    let health_str = healths
        .iter()
        .map(|(health, count)| {
            let color = if health.as_str() == "Healthy" {
                GREEN_BOLD
            } else if health.as_str() == "Degraded" {
                ORANGE
            } else if health.as_str() == "Failing" {
                RED_BOLD
            } else {
                GRAY
            };
            format!("{} {}", colorize(health, color, no_color), count)
        })
        .collect::<Vec<_>>()
        .join("  ");

    format!("States: {}  |  Health: {}", state_str, health_str)
}

/// Formats header row.
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

/// Formats unit row.
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
    let last_exit_text = format_last_exit(unit.last_exit.as_ref(), unit.cron.as_ref());
    let last_exit = if let Some(color) =
        last_exit_color(unit.last_exit.as_ref(), unit.cron.as_ref())
    {
        colorize(&last_exit_text, color, no_color)
    } else {
        last_exit_text
    };
    let command = unit
        .command
        .as_ref()
        .or(unit.runtime_command.as_ref())
        .cloned()
        .unwrap_or_else(|| "-".to_string());
    let health_label_text = health_label_extended(unit);
    let health_color = unit_health_color(unit.health);
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

/// Handles depth tint color.
fn depth_tint_color(depth: usize) -> &'static str {
    match depth {
        0 => WHITE,
        1 => DIM_WHITE,
        2 => GRAY,
        3 => MID_GRAY,
        4 => DARK_GRAY,
        _ => DEEP_GRAY,
    }
}

/// Handles tint value for depth.
fn tint_value_for_depth(value: String, depth: usize, no_color: bool) -> String {
    if no_color || depth == 0 {
        value
    } else {
        colorize(&value, depth_tint_color(depth), no_color)
    }
}

/// Renders spawn rows.
fn render_spawn_rows(nodes: &[SpawnedProcessNode], columns: &[Column], no_color: bool) {
    visit_spawn_tree(nodes, "", &mut |child, prefix, _| {
        println!(
            "{}",
            format_spawned_child_row(child, columns, no_color, prefix)
        );
    });
}

#[allow(dead_code)]
/// Returns the maximum spawn label width.
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

#[allow(dead_code)]
/// Returns the maximum spawn command width.
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

#[allow(dead_code)]
/// Returns the maximum unit command width.
fn max_unit_command_width(unit: &UnitStatus) -> usize {
    unit.command
        .as_ref()
        .or(unit.runtime_command.as_ref())
        .map(|cmd| visible_length(cmd))
        .unwrap_or(1)
}

/// Counts spawn nodes.
fn count_spawn_nodes(nodes: &[SpawnedProcessNode]) -> usize {
    let mut total = 0;
    visit_spawn_tree(nodes, "", &mut |_, _, _| total += 1);
    total
}

/// Visits spawn tree.
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

/// Formats spawned child row.
fn format_spawned_child_row(
    child: &SpawnedChild,
    columns: &[Column],
    no_color: bool,
    prefix: &str,
) -> String {
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

        let health = if succeeded { "Healthy" } else { "Failing" };

        (
            tint_value_for_depth("Exited".to_string(), child.depth, no_color),
            tint_value_for_depth(health.to_string(), child.depth, no_color),
        )
    } else {
        (
            tint_value_for_depth("Running".to_string(), child.depth, no_color),
            tint_value_for_depth("Healthy".to_string(), child.depth, no_color),
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
        SpawnedChildKind::Spawned => {
            tint_value_for_depth("spwn".to_string(), child.depth, no_color)
        }
        SpawnedChildKind::Peripheral => {
            tint_value_for_depth("peri".to_string(), child.depth, no_color)
        }
    };

    let values = [
        tint_value_for_depth(child_name, child.depth, no_color),
        kind_label,
        state,
        tint_value_for_depth(user, child.depth, no_color),
        tint_value_for_depth(pid, child.depth, no_color),
        tint_value_for_depth(cpu_col, child.depth, no_color),
        tint_value_for_depth(rss_col, child.depth, no_color),
        tint_value_for_depth(uptime, child.depth, no_color),
        tint_value_for_depth(command, child.depth, no_color),
        tint_value_for_depth(last_exit, child.depth, no_color),
        health_label,
    ];

    format_row(&values, columns)
}

/// Formats spawn exit.
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

/// Formats row.
fn format_row(values: &[String; 11], columns: &[Column]) -> String {
    let mut row = String::from('│');
    for (value, column) in values.iter().zip(columns.iter()) {
        let sanitized = sanitize_table_cell(value);
        row.push(' ');
        row.push_str(&ansi_pad(&sanitized, column.width, column.align));
        row.push(' ');
        row.push('│');
    }
    row
}

/// Handles ansi pad.
fn ansi_pad(value: &str, width: usize, align: Alignment) -> String {
    let len = visible_length(value);
    if len > width {
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

/// Ellipsizes ansi aware.
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

/// Strips ansi.
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

/// Returns the leading ansi len.
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

/// Returns the visible length.
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

/// Ellipsizes this item.
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

/// Ellipsizes from front.
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

/// Truncates unit name.
fn truncate_unit_name(name: &str, width: usize) -> String {
    if visible_length(name) <= width {
        return name.to_string();
    }
    if name.contains('/') {
        return ellipsize_from_front(name, width);
    }
    ellipsize(name, width)
}

/// Truncates nested unit label.
fn truncate_nested_unit_label(prefix: &str, name: &str, width: usize) -> String {
    let prefix_len = visible_length(prefix);
    if prefix_len >= width {
        return ellipsize(prefix, width);
    }

    let name_budget = width - prefix_len;
    format!("{}{}", prefix, truncate_unit_name(name, name_budget))
}

/// Formats cpu column.
fn format_cpu_column(metrics: Option<&UnitMetricsSummary>) -> String {
    metrics
        .map(|summary| format!("{:.1}%", summary.latest_cpu_percent))
        .unwrap_or_else(|| "-".to_string())
}

/// Formats rss column.
fn format_rss_column(metrics: Option<&UnitMetricsSummary>) -> String {
    metrics
        .map(|summary| format_bytes(summary.latest_rss_bytes))
        .unwrap_or_else(|| "-".to_string())
}

/// Formats bytes.
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

/// Fetches inspect.
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

/// Pads a string containing ANSI codes to a specific visual width.
fn pad_ansi_str(s: &str, width: usize) -> String {
    let visible_len = visible_length(s);
    if visible_len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - visible_len))
    }
}

/// Wraps plain text to a fixed visible width.
fn wrap_plain_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut wrapped = Vec::new();
    let mut line = String::new();
    let mut line_len = 0usize;

    for word in text.split_whitespace() {
        let word_len = word.chars().count();

        if line_len == 0 && word_len <= width {
            line.push_str(word);
            line_len = word_len;
            continue;
        }

        if line_len > 0 && line_len + 1 + word_len <= width {
            line.push(' ');
            line.push_str(word);
            line_len += 1 + word_len;
            continue;
        }

        if line_len > 0 {
            wrapped.push(line);
            line = String::new();
            line_len = 0;
        }

        if word_len <= width {
            line.push_str(word);
            line_len = word_len;
            continue;
        }

        let mut chunk = String::new();
        let mut chunk_len = 0usize;
        for ch in word.chars() {
            chunk.push(ch);
            chunk_len += 1;
            if chunk_len == width {
                wrapped.push(chunk);
                chunk = String::new();
                chunk_len = 0;
            }
        }
        if !chunk.is_empty() {
            line = chunk;
            line_len = chunk_len;
        }
    }

    if !line.is_empty() {
        wrapped.push(line);
    }

    if wrapped.is_empty() {
        wrapped.push(String::new());
    }

    wrapped
}

/// Formats command value lines.
fn format_command_value_lines(
    field_label: &str,
    command: &str,
    width: usize,
    no_color: bool,
) -> Vec<String> {
    let field = colorize(field_label, WHITE, no_color);
    let prefix = format!("{field}: ");
    let prefix_width = visible_length(&prefix);
    let value_width = width.saturating_sub(prefix_width).max(1);
    let wrapped_values = wrap_plain_text(command, value_width);

    wrapped_values
        .into_iter()
        .enumerate()
        .map(|(idx, segment)| {
            let value = colorize(&segment, GRAY, no_color);
            if idx == 0 {
                format!("{prefix}{value}")
            } else {
                format!("{}{}", " ".repeat(prefix_width), value)
            }
        })
        .collect()
}

#[allow(dead_code)]
/// Formats a single inspect box row with consistent width relative to the border.
fn format_inspect_box_line(content: &str, inner_width: usize) -> String {
    let content_width = inner_width.saturating_sub(3);
    format!("║  {} ║", ansi_pad(content, content_width, Alignment::Left))
}

/// Formats inspect outer line.
fn format_inspect_outer_line(content: &str, outer_inner_width: usize) -> String {
    let content_width = outer_inner_width.saturating_sub(3);
    format!("║  {} ║", ansi_pad(content, content_width, Alignment::Left))
}

/// Strips table edges.
fn strip_table_edges(line: &str) -> String {
    let mut chars = line.chars();
    let _ = chars.next();
    let mut trimmed: String = chars.collect();
    let _ = trimmed.pop();
    trimmed
}

/// Renders section table lines.
fn render_section_table_lines(
    columns: &[Column],
    rows: &[Vec<String>],
    title: Option<String>,
    no_color: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(strip_table_edges(&make_top_border(columns)));
    if let Some(title) = title {
        lines.push(strip_table_edges(&format_banner(&title, columns)));
    }
    lines.push(strip_table_edges(&make_separator_border(columns)));
    lines.push(strip_table_edges(&format_header_row(columns)));
    lines.push(strip_table_edges(&make_separator_border(columns)));
    for row in rows {
        lines.push(strip_table_edges(&format_row_cells(row, columns, no_color)));
    }
    lines.push(strip_table_edges(&make_bottom_border(columns)));
    lines
}

/// Assigns user colors.
fn assign_user_colors(users: &[String]) -> HashMap<String, &'static str> {
    const USER_COLORS: [&str; 12] = [
        "\x1b[38;5;39m",  // Bright blue
        "\x1b[38;5;208m", // Orange
        "\x1b[38;5;46m",  // Bright green
        "\x1b[38;5;201m", // Magenta
        "\x1b[38;5;226m", // Yellow
        "\x1b[38;5;51m",  // Cyan
        "\x1b[38;5;196m", // Red
        "\x1b[38;5;82m",  // Light green
        "\x1b[38;5;135m", // Purple
        "\x1b[38;5;214m", // Gold
        "\x1b[38;5;33m",  // Blue
        "\x1b[38;5;165m", // Light magenta
    ];

    let mut color_map = HashMap::new();
    for (i, user) in users.iter().enumerate() {
        let color = USER_COLORS[i % USER_COLORS.len()];
        color_map.insert(user.clone(), color);
    }
    color_map
}

/// Renders system resource bars (CPU and memory) in a boxed format for inspect output.
#[allow(dead_code)]
fn render_htop_bars_boxed(
    _metrics: Option<&UnitMetricsSummary>,
    no_color: bool,
    inner_width: usize,
) {
    let mut system = System::new();
    system.refresh_cpu_all();
    system.refresh_memory();
    let total_mem = system.total_memory();
    let used_mem = system.used_memory();
    let total_swap = system.total_swap();
    let used_swap = system.used_swap();

    let mem_percentage = if total_mem > 0 {
        (used_mem as f64 / total_mem as f64) * 100.0
    } else {
        0.0
    };

    let swap_percentage = if total_swap > 0 {
        (used_swap as f64 / total_swap as f64) * 100.0
    } else {
        0.0
    };

    let bar_width = 40;

    for (i, cpu) in system.cpus().iter().enumerate() {
        let cpu_usage = cpu.cpu_usage();
        let filled = ((cpu_usage / 100.0) * bar_width as f32) as usize;
        let bar = render_usage_bar(filled, bar_width, cpu_usage as f64, no_color);

        let label = if i < 10 {
            format!("{:2}", i)
        } else {
            format!("{}", i)
        };

        let line = format!("{:3}[{}] {:5.1}%", label, bar, cpu_usage);
        println!("{}", format_inspect_box_line(&line, inner_width));
    }

    let mem_filled = ((mem_percentage / 100.0) * bar_width as f64) as usize;
    let mem_bar = render_usage_bar(mem_filled, bar_width, mem_percentage, no_color);
    let mem_line = format!(
        "Mem[{}] {:5.2}/{:.2}G",
        mem_bar,
        used_mem as f64 / 1024.0 / 1024.0 / 1024.0,
        total_mem as f64 / 1024.0 / 1024.0 / 1024.0
    );
    println!("{}", format_inspect_box_line(&mem_line, inner_width));

    if total_swap > 0 {
        let swap_filled = ((swap_percentage / 100.0) * bar_width as f64) as usize;
        let swap_bar =
            render_usage_bar(swap_filled, bar_width, swap_percentage, no_color);
        let swap_line = format!(
            "Swp[{}] {:5.2}/{:.2}G",
            swap_bar,
            used_swap as f64 / 1024.0 / 1024.0 / 1024.0,
            total_swap as f64 / 1024.0 / 1024.0 / 1024.0
        );
        println!("{}", format_inspect_box_line(&swap_line, inner_width));
    }
}

/// Collects htop bar lines.
fn collect_htop_bar_lines(
    _metrics: Option<&UnitMetricsSummary>,
    no_color: bool,
) -> Vec<String> {
    let mut system = System::new();
    system.refresh_cpu_all();
    system.refresh_memory();
    let total_mem = system.total_memory();
    let used_mem = system.used_memory();
    let total_swap = system.total_swap();
    let used_swap = system.used_swap();

    let mem_percentage = if total_mem > 0 {
        (used_mem as f64 / total_mem as f64) * 100.0
    } else {
        0.0
    };

    let swap_percentage = if total_swap > 0 {
        (used_swap as f64 / total_swap as f64) * 100.0
    } else {
        0.0
    };

    let bar_width = 40;
    let mut lines = Vec::new();

    for (i, cpu) in system.cpus().iter().enumerate() {
        let cpu_usage = cpu.cpu_usage();
        let filled = ((cpu_usage / 100.0) * bar_width as f32) as usize;
        let bar = render_usage_bar(filled, bar_width, cpu_usage as f64, no_color);

        let label = if i < 10 {
            format!("{:2}", i)
        } else {
            format!("{}", i)
        };

        lines.push(format!("{:3}[{}] {:5.1}%", label, bar, cpu_usage));
    }

    let mem_filled = ((mem_percentage / 100.0) * bar_width as f64) as usize;
    let mem_bar = render_usage_bar(mem_filled, bar_width, mem_percentage, no_color);
    lines.push(format!(
        "Mem[{}] {:5.2}/{:.2}G",
        mem_bar,
        used_mem as f64 / 1024.0 / 1024.0 / 1024.0,
        total_mem as f64 / 1024.0 / 1024.0 / 1024.0
    ));

    if total_swap > 0 {
        let swap_filled = ((swap_percentage / 100.0) * bar_width as f64) as usize;
        let swap_bar =
            render_usage_bar(swap_filled, bar_width, swap_percentage, no_color);
        lines.push(format!(
            "Swp[{}] {:5.2}/{:.2}G",
            swap_bar,
            used_swap as f64 / 1024.0 / 1024.0 / 1024.0,
            total_swap as f64 / 1024.0 / 1024.0 / 1024.0
        ));
    }

    lines
}

#[allow(dead_code)]
/// Renders htop bars.
fn render_htop_bars(_metrics: Option<&UnitMetricsSummary>, no_color: bool) {
    let mut system = System::new();
    system.refresh_cpu_all();
    system.refresh_memory();
    let total_mem = system.total_memory();
    let used_mem = system.used_memory();
    let total_swap = system.total_swap();
    let used_swap = system.used_swap();

    let mem_percentage = if total_mem > 0 {
        (used_mem as f64 / total_mem as f64) * 100.0
    } else {
        0.0
    };

    let swap_percentage = if total_swap > 0 {
        (used_swap as f64 / total_swap as f64) * 100.0
    } else {
        0.0
    };

    let bar_width = 40;

    for (i, cpu) in system.cpus().iter().enumerate() {
        let cpu_usage = cpu.cpu_usage();
        let filled = ((cpu_usage / 100.0) * bar_width as f32) as usize;
        let bar = render_usage_bar(filled, bar_width, cpu_usage as f64, no_color);

        let label = if i < 10 {
            format!("{:2}", i)
        } else {
            format!("{}", i)
        };

        println!("{}[{}] {:>5.1}%", label, bar, cpu_usage);
    }

    let mem_filled = ((mem_percentage / 100.0) * bar_width as f64) as usize;
    let mem_bar = render_usage_bar(mem_filled, bar_width, mem_percentage, no_color);
    println!(
        "Mem[{}] {:.2}/{:.2}G",
        mem_bar,
        used_mem as f64 / 1024.0 / 1024.0 / 1024.0,
        total_mem as f64 / 1024.0 / 1024.0 / 1024.0
    );

    if total_swap > 0 {
        let swap_filled = ((swap_percentage / 100.0) * bar_width as f64) as usize;
        let swap_bar =
            render_usage_bar(swap_filled, bar_width, swap_percentage, no_color);
        println!(
            "Swp[{}] {:.2}/{:.2}G",
            swap_bar,
            used_swap as f64 / 1024.0 / 1024.0 / 1024.0,
            total_swap as f64 / 1024.0 / 1024.0 / 1024.0
        );
    }
}

/// Renders usage bar.
fn render_usage_bar(
    filled: usize,
    total_width: usize,
    percentage: f64,
    no_color: bool,
) -> String {
    let mut bar = String::new();

    for i in 0..total_width {
        if i < filled {
            let color = if no_color {
                ""
            } else if percentage > 90.0 {
                RED
            } else if percentage > 70.0 {
                YELLOW
            } else if percentage > 50.0 {
                CYAN
            } else {
                GREEN
            };

            let reset = if no_color { "" } else { RESET };
            bar.push_str(&format!("{}|{}", color, reset));
        } else {
            bar.push(' ');
        }
    }

    bar
}

/// Renders inspect.
fn render_inspect(
    payload: &InspectPayload,
    opts: &InspectRenderOptions,
) -> Result<OverallHealth, Box<dyn Error>> {
    let (health, lines) = collect_inspect_lines(payload, opts)?;
    print_rendered_lines(&lines)?;
    Ok(health)
}

fn print_rendered_lines(lines: &[String]) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    for line in lines {
        writeln!(stdout, "{line}")?;
    }
    stdout.flush()
}

fn collect_inspect_lines(
    payload: &InspectPayload,
    opts: &InspectRenderOptions,
) -> Result<(OverallHealth, Vec<String>), Box<dyn Error>> {
    if payload.unit.is_none() {
        return Ok((
            OverallHealth::Failing,
            vec!["No unit matching the requested identifier.".to_string()],
        ));
    }

    let unit = payload.unit.as_ref().unwrap();
    let health = overall_health_from_unit(unit);

    let filtered_samples = if unit.kind == UnitKind::Cron {
        if let Some(cron_status) = &unit.cron {
            if let Some(last_run) = cron_status.recent_runs.first() {
                if !last_run.metrics.is_empty() {
                    filter_samples(
                        &last_run.metrics,
                        Some(opts.window_seconds),
                        opts.samples_limit,
                    )
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    } else {
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
        return Ok((health, vec![serde_json::to_string_pretty(&json_payload)?]));
    }
    let table_width = compute_inspect_process_table_width(unit);
    let outer_inner_width = table_width.saturating_sub(2);
    let outer_border_line = "═".repeat(outer_inner_width);
    let content_width = outer_inner_width.saturating_sub(3);
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();

    let kind_str = match unit.kind {
        UnitKind::Service => colorize("service", CYAN, opts.no_color),
        UnitKind::Cron => colorize("cron", YELLOW, opts.no_color),
        UnitKind::Orphaned => colorize("orphaned", GRAY, opts.no_color),
    };
    let health_str = colorize(
        overall_health_label(health),
        overall_health_color(health),
        opts.no_color,
    );
    let pid_str = if let Some(process) = &unit.process {
        colorize(&process.pid.to_string(), BRIGHT_WHITE, opts.no_color)
    } else {
        colorize("-", GRAY, opts.no_color)
    };
    let uptime_str = if let Some(uptime) = unit.uptime.as_ref() {
        format!("{} ({}s)", uptime.human, uptime.seconds)
    } else {
        "-".to_string()
    };
    let exit_text = format_last_exit(unit.last_exit.as_ref(), unit.cron.as_ref());
    let exit_str = if let Some(color) =
        last_exit_color(unit.last_exit.as_ref(), unit.cron.as_ref())
    {
        colorize(&exit_text, color, opts.no_color)
    } else {
        exit_text
    };
    let label_width = 10;
    let data_width = content_width.saturating_sub(label_width + 2);
    let status_label = colorize("Status", DIM_CYAN, opts.no_color);
    let kind_label = colorize("Kind", DIM_WHITE, opts.no_color);
    let health_label = colorize("Health", DIM_WHITE, opts.no_color);
    let pid_label = colorize("PID", DIM_WHITE, opts.no_color);
    let uptime_label = colorize("Uptime", DIM_WHITE, opts.no_color);
    let exit_label = colorize("Exit", DIM_WHITE, opts.no_color);
    let half_width = (data_width.saturating_sub(3)) / 2;
    let second_half_width = data_width.saturating_sub(half_width + 3);
    let empty_label = pad_ansi_str("", label_width);
    let mut overview_lines = vec![
        colorize(&format!("Unit: {}", unit.name), CYAN, opts.no_color),
        format!(
            "{} │ {}",
            pad_ansi_str(&status_label, label_width),
            pad_ansi_str(
                &format!(
                    "{} │ {}",
                    pad_ansi_str(&format!("{}: {}", kind_label, kind_str), half_width),
                    pad_ansi_str(
                        &format!("{}: {}", health_label, health_str),
                        second_half_width
                    )
                ),
                data_width
            )
        ),
        format!(
            "{} │ {}",
            empty_label,
            pad_ansi_str(
                &format!(
                    "{} │ {}",
                    pad_ansi_str(&format!("{}: {}", pid_label, pid_str), half_width),
                    pad_ansi_str(
                        &format!("{}: {}", uptime_label, uptime_str),
                        second_half_width
                    )
                ),
                data_width
            )
        ),
        format!(
            "{} │ {}",
            empty_label,
            pad_ansi_str(&format!("{}: {}", exit_label, exit_str), data_width)
        ),
    ];

    if unit.command.is_some() || unit.runtime_command.is_some() {
        if let Some(command) = &unit.command {
            let cmd_label = colorize("Command", WHITE, opts.no_color);
            let cmd_label_padded = pad_ansi_str(&cmd_label, label_width);
            for (idx, cmd_line) in format_command_value_lines(
                "Configured",
                command,
                data_width,
                opts.no_color,
            )
            .iter()
            .enumerate()
            {
                let label = if idx == 0 {
                    &cmd_label_padded
                } else {
                    &empty_label
                };
                overview_lines.push(format!(
                    "{} │ {}",
                    label,
                    pad_ansi_str(cmd_line, data_width)
                ));
            }
        }
        if let Some(runtime_command) = &unit.runtime_command {
            let prefix_str = if unit.command.is_some() {
                String::new()
            } else {
                colorize("Command", WHITE, opts.no_color)
            };
            let prefix_padded = pad_ansi_str(&prefix_str, label_width);
            for (idx, runtime_line) in format_command_value_lines(
                "Runtime",
                runtime_command,
                data_width,
                opts.no_color,
            )
            .iter()
            .enumerate()
            {
                let label = if idx == 0 {
                    &prefix_padded
                } else {
                    &empty_label
                };
                overview_lines.push(format!(
                    "{} │ {}",
                    label,
                    pad_ansi_str(runtime_line, data_width)
                ));
            }
        }
    }
    sections.push(("Overview".to_string(), overview_lines));

    if !unit.spawned_children.is_empty() {
        let mut lines = Vec::new();
        visit_spawn_tree(&unit.spawned_children, "", &mut |child, prefix, _| {
            let uptime = child
                .started_at
                .elapsed()
                .map(|d| format_duration(d.as_secs()))
                .unwrap_or_else(|_| "0s".to_string());
            let depth_info = if child.depth > 0 {
                format!(
                    ", {}: {}",
                    colorize("depth", DIM_WHITE, opts.no_color),
                    colorize(&child.depth.to_string(), BRIGHT_WHITE, opts.no_color)
                )
            } else {
                String::new()
            };
            lines.push(format!(
                "{}{} [{}: {}, {}: {}{}]",
                colorize(prefix, DIM_WHITE, opts.no_color),
                colorize(&child.name, WHITE, opts.no_color),
                colorize("PID", DIM_WHITE, opts.no_color),
                colorize(&child.pid.to_string(), BRIGHT_WHITE, opts.no_color),
                colorize("Up", DIM_WHITE, opts.no_color),
                colorize(&uptime, GRAY, opts.no_color),
                depth_info
            ));
        });
        sections.push((
            format!(
                "Process Tree ({} total)",
                colorize(
                    &count_spawn_nodes(&unit.spawned_children).to_string(),
                    BRIGHT_WHITE,
                    opts.no_color
                )
            ),
            lines,
        ));
    }

    let mut resource_metrics_lines = Vec::new();
    if let Some(metrics) = unit.metrics.as_ref() {
        let cpu_color = if metrics.latest_cpu_percent > 80.0 {
            RED
        } else if metrics.latest_cpu_percent > 50.0 {
            YELLOW
        } else {
            GREEN
        };
        let mem_color = if metrics.latest_rss_bytes > 8 * 1024 * 1024 * 1024 {
            YELLOW
        } else {
            WHITE
        };
        resource_metrics_lines.push(format!(
            "{}: {} CPU | {} RSS",
            colorize("Latest", DIM_WHITE, opts.no_color),
            colorize(
                &format!("{:.2}%", metrics.latest_cpu_percent),
                cpu_color,
                opts.no_color
            ),
            colorize(
                &format_bytes(metrics.latest_rss_bytes),
                mem_color,
                opts.no_color
            )
        ));
        resource_metrics_lines.push(format!(
            "{}: {} CPU | {} RSS",
            colorize("Average", DIM_WHITE, opts.no_color),
            colorize(
                &format!("{:.2}%", metrics.average_cpu_percent),
                WHITE,
                opts.no_color
            ),
            colorize(
                &format_bytes(metrics.latest_rss_bytes),
                WHITE,
                opts.no_color
            )
        ));
        resource_metrics_lines.push(format!(
            "{}: {} CPU | {} RSS",
            colorize("Maximum", DIM_WHITE, opts.no_color),
            colorize(
                &format!("{:.2}%", metrics.max_cpu_percent),
                WHITE,
                opts.no_color
            ),
            colorize(
                &format_bytes(metrics.latest_rss_bytes),
                WHITE,
                opts.no_color
            )
        ));
        resource_metrics_lines.push(format!(
            "{}: {}",
            colorize("Samples", DIM_WHITE, opts.no_color),
            colorize(&metrics.samples.to_string(), BRIGHT_WHITE, opts.no_color)
        ));
    } else if unit.kind == UnitKind::Cron {
        resource_metrics_lines.push(colorize(
            "Awaiting next cron execution",
            GRAY,
            opts.no_color,
        ));
    } else if unit.process.is_some() {
        resource_metrics_lines.push(colorize(
            "Collector initializing (may take a few seconds)",
            GRAY,
            opts.no_color,
        ));
    } else {
        resource_metrics_lines.push(colorize(
            "Not available (service not running)",
            GRAY,
            opts.no_color,
        ));
    }
    sections.push(("Resource Metrics".to_string(), resource_metrics_lines));

    sections.push((
        "System Resources".to_string(),
        collect_htop_bar_lines(unit.metrics.as_ref(), opts.no_color),
    ));

    if !filtered_samples.is_empty() {
        let mut chart_lines = Vec::new();
        if unit.kind == UnitKind::Cron
            && let Some(cron_status) = &unit.cron
            && let Some(last_run) = cron_status.recent_runs.first()
        {
            chart_lines.push(format!(
                "{}: {}",
                colorize("Data from last run", DIM_WHITE, opts.no_color),
                colorize(
                    &last_run
                        .started_at
                        .with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string(),
                    GRAY,
                    opts.no_color
                )
            ));
            chart_lines.push(String::new());
        }

        let chart_config = ChartConfig {
            no_color: opts.no_color,
            window_desc: opts.window_desc.clone(),
            max_width: Some(content_width),
        };
        match charting::render_metrics_chart_lines(&filtered_samples, &chart_config) {
            Ok(lines) => chart_lines.extend(lines),
            Err(e) => {
                warn!("Failed to render chart: {}", e);
                chart_lines.push(colorize("Failed to render chart", GRAY, opts.no_color));
            }
        }
        sections.push(("Time Series Charts".to_string(), chart_lines));
    } else {
        sections.push((
            "Time Series Charts".to_string(),
            vec![colorize(
                "No metrics available for the specified window",
                GRAY,
                opts.no_color,
            )],
        ));
    }

    if unit.kind != UnitKind::Cron {
        sections.push((
            "Process Details Table".to_string(),
            collect_inspect_process_table_lines(unit, opts.no_color, content_width),
        ));
    }

    if unit.kind == UnitKind::Cron
        && let Some(cron_status) = &unit.cron
        && !cron_status.recent_runs.is_empty()
    {
        let rows: Vec<InspectCronRunRow> = cron_status
            .recent_runs
            .iter()
            .take(INSPECT_CRON_HISTORY_LIMIT)
            .enumerate()
            .map(|(index, run)| InspectCronRunRow {
                run: (index + 1).to_string(),
                time: run
                    .started_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
                user: run.user.clone().unwrap_or_else(|| "-".to_string()),
                pid: run
                    .pid
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                command: run.command.clone().unwrap_or_else(|| "-".to_string()),
            })
            .collect();

        let mut widths = compute_inspect_cron_preferred_widths(&rows);
        shrink_inspect_cron_widths_to_fit(&mut widths, content_width);
        let columns = [
            Column {
                title: INSPECT_CRON_COLUMN_TITLES[0],
                width: widths[0],
                align: INSPECT_CRON_COLUMN_ALIGNS[0],
            },
            Column {
                title: INSPECT_CRON_COLUMN_TITLES[1],
                width: widths[1],
                align: INSPECT_CRON_COLUMN_ALIGNS[1],
            },
            Column {
                title: INSPECT_CRON_COLUMN_TITLES[2],
                width: widths[2],
                align: INSPECT_CRON_COLUMN_ALIGNS[2],
            },
            Column {
                title: INSPECT_CRON_COLUMN_TITLES[3],
                width: widths[3],
                align: INSPECT_CRON_COLUMN_ALIGNS[3],
            },
            Column {
                title: INSPECT_CRON_COLUMN_TITLES[4],
                width: widths[4],
                align: INSPECT_CRON_COLUMN_ALIGNS[4],
            },
        ];
        let row_values: Vec<Vec<String>> = rows
            .iter()
            .map(|row| {
                vec![
                    row.run.clone(),
                    row.time.clone(),
                    row.user.clone(),
                    row.pid.clone(),
                    row.command.clone(),
                ]
            })
            .collect();
        sections.push((
            format!("Cron Run History (last {})", INSPECT_CRON_HISTORY_LIMIT),
            render_section_table_lines(&columns, &row_values, None, opts.no_color),
        ));
    }

    let mut rendered_lines = Vec::new();
    rendered_lines.push(String::new());
    rendered_lines.push(format!("╔{}╗", outer_border_line));
    for (index, (title, lines)) in sections.iter().enumerate() {
        if index > 0 {
            rendered_lines.push(format!("╠{}╣", outer_border_line));
        }
        rendered_lines.push(format_inspect_outer_line(
            &colorize(title, CYAN, opts.no_color),
            outer_inner_width,
        ));
        if !lines.is_empty() {
            rendered_lines.push(format!("╟{}╢", "─".repeat(outer_inner_width)));
            for line in lines {
                rendered_lines
                    .push(format_inspect_outer_line(line, outer_inner_width));
            }
        }
    }
    rendered_lines.push(format!("╚{}╝", outer_border_line));

    Ok((health, rendered_lines))
}

#[derive(Clone)]
/// Represents inspect process row.
struct InspectProcessRow {
    tree_label: String,
    is_root: bool,
    depth: usize,
    pid: u32,
    ppid: Option<u32>,
    user: String,
    pri: Option<i64>,
    nice: Option<i64>,
    virt_bytes: u64,
    res_bytes: u64,
    shared_bytes: Option<u64>,
    state: String,
    cpu_percent: f32,
    mem_percent: f64,
    cpu_time: String,
    command: String,
}

/// Represents inspect cron run row.
struct InspectCronRunRow {
    run: String,
    time: String,
    user: String,
    pid: String,
    command: String,
}

#[derive(Default)]
/// Represents linux proc stats.
struct LinuxProcStats {
    ppid: Option<u32>,
    priority: Option<i64>,
    nice: Option<i64>,
    cpu_ticks: Option<u64>,
    shared_bytes: Option<u64>,
}

/// Represents inspect process context.
struct InspectProcessContext<'a> {
    system: &'a System,
    users: &'a Users,
    children_by_parent: &'a HashMap<u32, Vec<u32>>,
    total_memory: f64,
}

const INSPECT_PROCESS_COLUMN_COUNT: usize = 14;
const INSPECT_COL_PROC: usize = 0;
const INSPECT_COL_PID: usize = 1;
const INSPECT_COL_PPID: usize = 2;
const INSPECT_COL_USER: usize = 3;
const INSPECT_COL_PRI: usize = 4;
const INSPECT_COL_NI: usize = 5;
const INSPECT_COL_VIRT: usize = 6;
const INSPECT_COL_RES: usize = 7;
const INSPECT_COL_SHR: usize = 8;
const INSPECT_COL_STATE: usize = 9;
const INSPECT_COL_CPU: usize = 10;
const INSPECT_COL_MEM: usize = 11;
const INSPECT_COL_TIME: usize = 12;
const INSPECT_COL_CMD: usize = 13;

const INSPECT_PROCESS_COLUMN_TITLES: [&str; INSPECT_PROCESS_COLUMN_COUNT] = [
    "PROC", "PID", "PPID", "USER", "PRI", "NI", "VIRT", "RES", "SHR", "S", "CPU%",
    "MEM%", "TIME+", "CMD",
];

const INSPECT_PROCESS_COLUMN_ALIGNS: [Alignment; INSPECT_PROCESS_COLUMN_COUNT] = [
    Alignment::Left,
    Alignment::Right,
    Alignment::Right,
    Alignment::Left,
    Alignment::Right,
    Alignment::Right,
    Alignment::Right,
    Alignment::Right,
    Alignment::Right,
    Alignment::Left,
    Alignment::Right,
    Alignment::Right,
    Alignment::Right,
    Alignment::Left,
];

const INSPECT_PROCESS_SOFT_MIN_WIDTHS: [usize; INSPECT_PROCESS_COLUMN_COUNT] =
    [8, 3, 4, 4, 3, 2, 4, 4, 4, 1, 4, 4, 5, 10];
const INSPECT_PROCESS_SHRINK_PRIORITY: [usize; INSPECT_PROCESS_COLUMN_COUNT] =
    [0, 13, 3, 12, 6, 7, 8, 9, 4, 5, 2, 11, 10, 1];
const INSPECT_PROC_CMD_MAX_DIFF: usize = 4;

const INSPECT_CRON_COLUMN_COUNT: usize = 5;
const INSPECT_CRON_COLUMN_TITLES: [&str; INSPECT_CRON_COLUMN_COUNT] =
    ["RUN", "TIME", "USER", "PID", "CMD"];
const INSPECT_CRON_COLUMN_ALIGNS: [Alignment; INSPECT_CRON_COLUMN_COUNT] = [
    Alignment::Left,
    Alignment::Left,
    Alignment::Left,
    Alignment::Right,
    Alignment::Left,
];
const INSPECT_CRON_SOFT_MIN_WIDTHS: [usize; INSPECT_CRON_COLUMN_COUNT] =
    [3, 19, 4, 3, 18];
const INSPECT_CRON_SHRINK_PRIORITY: [usize; INSPECT_CRON_COLUMN_COUNT] = [4, 2, 1, 0, 3];

/// Handles inspect process content budget.
fn inspect_process_content_budget(terminal_width: usize) -> usize {
    terminal_width.saturating_sub((3 * INSPECT_PROCESS_COLUMN_COUNT) + 1)
}

/// Handles inspect process row width.
fn inspect_process_row_width(
    content_widths: &[usize; INSPECT_PROCESS_COLUMN_COUNT],
) -> usize {
    content_widths.iter().sum::<usize>() + (3 * INSPECT_PROCESS_COLUMN_COUNT) + 1
}

/// Handles inspect cron content budget.
fn inspect_cron_content_budget(terminal_width: usize) -> usize {
    terminal_width.saturating_sub((3 * INSPECT_CRON_COLUMN_COUNT) + 1)
}

#[cfg(test)]
fn inspect_cron_row_width(content_widths: &[usize; INSPECT_CRON_COLUMN_COUNT]) -> usize {
    content_widths.iter().sum::<usize>() + (3 * INSPECT_CRON_COLUMN_COUNT) + 1
}

/// Reduces inspect process widths.
fn reduce_inspect_process_widths(
    widths: &mut [usize; INSPECT_PROCESS_COLUMN_COUNT],
    min_widths: &[usize; INSPECT_PROCESS_COLUMN_COUNT],
    budget: usize,
) {
    loop {
        let mut total = widths.iter().sum::<usize>();
        if total <= budget {
            break;
        }

        let mut changed = false;
        for index in INSPECT_PROCESS_SHRINK_PRIORITY {
            if total <= budget {
                break;
            }

            if widths[index] <= min_widths[index] {
                continue;
            }

            let reducible = widths[index] - min_widths[index];
            let needed = total - budget;
            let delta = reducible.min(needed);
            widths[index] -= delta;
            total -= delta;
            changed = true;
        }

        if !changed {
            break;
        }
    }
}

/// Reduces inspect cron widths.
fn reduce_inspect_cron_widths(
    widths: &mut [usize; INSPECT_CRON_COLUMN_COUNT],
    min_widths: &[usize; INSPECT_CRON_COLUMN_COUNT],
    budget: usize,
) {
    loop {
        let mut total = widths.iter().sum::<usize>();
        if total <= budget {
            break;
        }

        let mut changed = false;
        for index in INSPECT_CRON_SHRINK_PRIORITY {
            if total <= budget {
                break;
            }

            if widths[index] <= min_widths[index] {
                continue;
            }

            let reducible = widths[index] - min_widths[index];
            let needed = total - budget;
            let delta = reducible.min(needed);
            widths[index] -= delta;
            total -= delta;
            changed = true;
        }

        if !changed {
            break;
        }
    }
}

/// Shrinks inspect process widths to fit.
fn shrink_inspect_process_widths_to_fit(
    widths: &mut [usize; INSPECT_PROCESS_COLUMN_COUNT],
    terminal_width: usize,
) {
    let budget = inspect_process_content_budget(terminal_width);
    if widths.iter().sum::<usize>() <= budget {
        return;
    }

    reduce_inspect_process_widths(widths, &INSPECT_PROCESS_SOFT_MIN_WIDTHS, budget);

    if widths.iter().sum::<usize>() <= budget {
        rebalance_inspect_process_proc_cmd_widths(widths);
        return;
    }

    reduce_inspect_process_widths(widths, &[1; INSPECT_PROCESS_COLUMN_COUNT], budget);
    rebalance_inspect_process_proc_cmd_widths(widths);
}

/// Rebalances inspect process table widths so PROC and CMD stay close in visible width.
fn rebalance_inspect_process_proc_cmd_widths(
    widths: &mut [usize; INSPECT_PROCESS_COLUMN_COUNT],
) {
    let proc = INSPECT_COL_PROC;
    let cmd = INSPECT_COL_CMD;

    if widths[cmd] > widths[proc] + INSPECT_PROC_CMD_MAX_DIFF {
        let diff = widths[cmd] - widths[proc] - INSPECT_PROC_CMD_MAX_DIFF;
        let needed = diff.div_ceil(2);
        let cmd_floor = INSPECT_PROCESS_SOFT_MIN_WIDTHS[cmd]
            .max(INSPECT_PROCESS_COLUMN_TITLES[cmd].len());
        let transfer = needed.min(widths[cmd].saturating_sub(cmd_floor));
        widths[cmd] -= transfer;
        widths[proc] += transfer;
    } else if widths[proc] > widths[cmd] + INSPECT_PROC_CMD_MAX_DIFF {
        let diff = widths[proc] - widths[cmd] - INSPECT_PROC_CMD_MAX_DIFF;
        let needed = diff.div_ceil(2);
        let proc_floor = INSPECT_PROCESS_SOFT_MIN_WIDTHS[proc]
            .max(INSPECT_PROCESS_COLUMN_TITLES[proc].len());
        let transfer = needed.min(widths[proc].saturating_sub(proc_floor));
        widths[proc] -= transfer;
        widths[cmd] += transfer;
    }
}

/// Shrinks inspect cron widths to fit.
fn shrink_inspect_cron_widths_to_fit(
    widths: &mut [usize; INSPECT_CRON_COLUMN_COUNT],
    terminal_width: usize,
) {
    let budget = inspect_cron_content_budget(terminal_width);
    if widths.iter().sum::<usize>() <= budget {
        return;
    }

    reduce_inspect_cron_widths(widths, &INSPECT_CRON_SOFT_MIN_WIDTHS, budget);

    if widths.iter().sum::<usize>() <= budget {
        return;
    }

    reduce_inspect_cron_widths(widths, &[1; INSPECT_CRON_COLUMN_COUNT], budget);
}

/// Computes inspect process preferred widths.
fn compute_inspect_process_preferred_widths(
    rows: &[InspectProcessRow],
) -> [usize; INSPECT_PROCESS_COLUMN_COUNT] {
    let mut widths = INSPECT_PROCESS_COLUMN_TITLES.map(visible_length);

    for row in rows {
        widths[INSPECT_COL_PROC] =
            widths[INSPECT_COL_PROC].max(visible_length(&row.tree_label));
        widths[INSPECT_COL_PID] =
            widths[INSPECT_COL_PID].max(visible_length(&row.pid.to_string()));
        widths[INSPECT_COL_PPID] = widths[INSPECT_COL_PPID].max(visible_length(
            &row.ppid
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ));
        widths[INSPECT_COL_USER] =
            widths[INSPECT_COL_USER].max(visible_length(&row.user));
        widths[INSPECT_COL_PRI] = widths[INSPECT_COL_PRI].max(visible_length(
            &row.pri
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ));
        widths[INSPECT_COL_NI] = widths[INSPECT_COL_NI].max(visible_length(
            &row.nice
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ));
        widths[INSPECT_COL_VIRT] =
            widths[INSPECT_COL_VIRT].max(visible_length(&format_bytes(row.virt_bytes)));
        widths[INSPECT_COL_RES] =
            widths[INSPECT_COL_RES].max(visible_length(&format_bytes(row.res_bytes)));
        widths[INSPECT_COL_SHR] = widths[INSPECT_COL_SHR].max(visible_length(
            &row.shared_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "-".to_string()),
        ));
        widths[INSPECT_COL_STATE] =
            widths[INSPECT_COL_STATE].max(visible_length(&row.state));
        widths[INSPECT_COL_CPU] = widths[INSPECT_COL_CPU]
            .max(visible_length(&format!("{:.1}", row.cpu_percent)));
        widths[INSPECT_COL_MEM] = widths[INSPECT_COL_MEM]
            .max(visible_length(&format!("{:.1}", row.mem_percent)));
        widths[INSPECT_COL_TIME] =
            widths[INSPECT_COL_TIME].max(visible_length(&row.cpu_time));
        widths[INSPECT_COL_CMD] =
            widths[INSPECT_COL_CMD].max(visible_length(&row.command));
    }

    widths
}

/// Computes inspect cron preferred widths.
fn compute_inspect_cron_preferred_widths(
    rows: &[InspectCronRunRow],
) -> [usize; INSPECT_CRON_COLUMN_COUNT] {
    let mut widths = INSPECT_CRON_COLUMN_TITLES.map(visible_length);
    for row in rows {
        widths[0] = widths[0].max(visible_length(&row.run));
        widths[1] = widths[1].max(visible_length(&row.time));
        widths[2] = widths[2].max(visible_length(&row.user));
        widths[3] = widths[3].max(visible_length(&row.pid));
        widths[4] = widths[4].max(visible_length(&row.command));
    }
    widths
}

/// Computes inspect process table width.
fn compute_inspect_process_table_width(unit: &UnitStatus) -> usize {
    let table_width = detect_target_table_width(120);
    let Some(root_runtime) = unit.process.as_ref() else {
        return table_width;
    };

    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );

    let tracked_root_pid = root_runtime.pid;
    let root_pid = if system.process(SysPid::from_u32(tracked_root_pid)).is_some() {
        tracked_root_pid
    } else if let Some(live_descendant_pid) =
        find_live_spawn_root_pid(&unit.spawned_children, &system)
    {
        live_descendant_pid
    } else {
        return table_width;
    };

    let users = Users::new_with_refreshed_list();
    let total_memory = system.total_memory() as f64;

    let mut children_by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children_by_parent
                .entry(parent.as_u32())
                .or_default()
                .push(pid.as_u32());
        }
    }
    for children in children_by_parent.values_mut() {
        children.sort_unstable();
    }

    let context = InspectProcessContext {
        system: &system,
        users: &users,
        children_by_parent: &children_by_parent,
        total_memory,
    };

    let mut rows = Vec::new();
    append_inspect_process_rows(&context, root_pid, "", "", true, &mut rows);
    if rows.is_empty() {
        return table_width;
    }

    let mut widths = compute_inspect_process_preferred_widths(&rows);
    shrink_inspect_process_widths_to_fit(&mut widths, table_width);
    inspect_process_row_width(&widths)
}

/// Handles inspect process row values.
fn inspect_process_row_values(
    row: &InspectProcessRow,
    user_colors: &HashMap<String, &'static str>,
    no_color: bool,
) -> Vec<String> {
    let virt_plain = format_bytes(row.virt_bytes);
    let virt_colored = if no_color {
        format_bytes(row.virt_bytes)
    } else {
        format!("{}{}{}", GREEN, virt_plain, RESET)
    };

    let user_colored = if no_color || row.user == "-" {
        row.user.clone()
    } else {
        let color = user_colors.get(&row.user).unwrap_or(&"");
        format!("{}{}{}", color, row.user, RESET)
    };

    let values = vec![
        if row.is_root {
            row.tree_label.clone()
        } else {
            tint_value_for_depth(row.tree_label.clone(), row.depth, no_color)
        },
        if row.is_root {
            row.pid.to_string()
        } else {
            tint_value_for_depth(row.pid.to_string(), row.depth, no_color)
        },
        if row.is_root {
            row.ppid
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        } else {
            tint_value_for_depth(
                row.ppid
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                row.depth,
                no_color,
            )
        },
        if row.is_root {
            user_colored
        } else {
            tint_value_for_depth(row.user.clone(), row.depth, no_color)
        },
        if row.is_root {
            row.pri
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        } else {
            tint_value_for_depth(
                row.pri
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                row.depth,
                no_color,
            )
        },
        if row.is_root {
            row.nice
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        } else {
            tint_value_for_depth(
                row.nice
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                row.depth,
                no_color,
            )
        },
        if row.is_root {
            virt_colored
        } else {
            tint_value_for_depth(virt_plain, row.depth, no_color)
        },
        if row.is_root {
            format_bytes(row.res_bytes)
        } else {
            tint_value_for_depth(format_bytes(row.res_bytes), row.depth, no_color)
        },
        if row.is_root {
            row.shared_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "-".to_string())
        } else {
            tint_value_for_depth(
                row.shared_bytes
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".to_string()),
                row.depth,
                no_color,
            )
        },
        if row.is_root {
            row.state.clone()
        } else {
            tint_value_for_depth(row.state.clone(), row.depth, no_color)
        },
        if row.is_root {
            format!("{:.1}", row.cpu_percent)
        } else {
            tint_value_for_depth(format!("{:.1}", row.cpu_percent), row.depth, no_color)
        },
        if row.is_root {
            format!("{:.1}", row.mem_percent)
        } else {
            tint_value_for_depth(format!("{:.1}", row.mem_percent), row.depth, no_color)
        },
        if row.is_root {
            row.cpu_time.clone()
        } else {
            tint_value_for_depth(row.cpu_time.clone(), row.depth, no_color)
        },
        if row.is_root {
            row.command.clone()
        } else {
            tint_value_for_depth(row.command.clone(), row.depth, no_color)
        },
    ];

    values
}

/// Renders a process table for the inspected unit and all discovered descendants.
fn collect_inspect_process_table_lines(
    unit: &UnitStatus,
    no_color: bool,
    table_width: usize,
) -> Vec<String> {
    let Some(root_runtime) = unit.process.as_ref() else {
        return vec![colorize("Unit is not currently running", GRAY, no_color)];
    };

    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything(),
    );

    let tracked_root_pid = root_runtime.pid;
    let root_pid = if system.process(SysPid::from_u32(tracked_root_pid)).is_some() {
        tracked_root_pid
    } else if let Some(live_descendant_pid) =
        find_live_spawn_root_pid(&unit.spawned_children, &system)
    {
        let msg = format!(
            "{}: {} -> {}",
            colorize(
                "Tracked root missing; falling back to descendant",
                YELLOW_BOLD,
                no_color
            ),
            tracked_root_pid,
            live_descendant_pid
        );
        let mut lines = vec![msg];
        lines.extend(collect_inspect_process_table_lines_from_root(
            unit,
            no_color,
            table_width,
            &system,
            live_descendant_pid,
        ));
        return lines;
    } else {
        return vec![format!(
            "{}: {}",
            colorize("Root process no longer available", GRAY, no_color),
            tracked_root_pid
        )];
    };

    collect_inspect_process_table_lines_from_root(
        unit,
        no_color,
        table_width,
        &system,
        root_pid,
    )
}

/// Collects inspect process table lines from root.
fn collect_inspect_process_table_lines_from_root(
    _unit: &UnitStatus,
    no_color: bool,
    table_width: usize,
    system: &System,
    root_pid: u32,
) -> Vec<String> {
    let mut lines = Vec::new();

    let users = Users::new_with_refreshed_list();
    let total_memory = system.total_memory() as f64;

    let mut children_by_parent: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children_by_parent
                .entry(parent.as_u32())
                .or_default()
                .push(pid.as_u32());
        }
    }
    for children in children_by_parent.values_mut() {
        children.sort_unstable();
    }

    let context = InspectProcessContext {
        system,
        users: &users,
        children_by_parent: &children_by_parent,
        total_memory,
    };

    let mut rows = Vec::new();
    append_inspect_process_rows(&context, root_pid, "", "", true, &mut rows);

    let mut distinct_users: Vec<String> = rows
        .iter()
        .map(|row| row.user.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    distinct_users.sort();

    let user_colors = assign_user_colors(&distinct_users);

    if rows.is_empty() {
        return vec![colorize(
            "No running process rows collected",
            GRAY,
            no_color,
        )];
    }

    let mut widths = compute_inspect_process_preferred_widths(&rows);
    shrink_inspect_process_widths_to_fit(&mut widths, table_width);

    let columns = [
        Column {
            title: "PROC",
            width: widths[INSPECT_COL_PROC],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_PROC],
        },
        Column {
            title: "PID",
            width: widths[INSPECT_COL_PID],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_PID],
        },
        Column {
            title: "PPID",
            width: widths[INSPECT_COL_PPID],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_PPID],
        },
        Column {
            title: "USER",
            width: widths[INSPECT_COL_USER],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_USER],
        },
        Column {
            title: "PRI",
            width: widths[INSPECT_COL_PRI],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_PRI],
        },
        Column {
            title: "NI",
            width: widths[INSPECT_COL_NI],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_NI],
        },
        Column {
            title: "VIRT",
            width: widths[INSPECT_COL_VIRT],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_VIRT],
        },
        Column {
            title: "RES",
            width: widths[INSPECT_COL_RES],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_RES],
        },
        Column {
            title: "SHR",
            width: widths[INSPECT_COL_SHR],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_SHR],
        },
        Column {
            title: "S",
            width: widths[INSPECT_COL_STATE],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_STATE],
        },
        Column {
            title: "CPU%",
            width: widths[INSPECT_COL_CPU],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_CPU],
        },
        Column {
            title: "MEM%",
            width: widths[INSPECT_COL_MEM],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_MEM],
        },
        Column {
            title: "TIME+",
            width: widths[INSPECT_COL_TIME],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_TIME],
        },
        Column {
            title: "CMD",
            width: widths[INSPECT_COL_CMD],
            align: INSPECT_PROCESS_COLUMN_ALIGNS[INSPECT_COL_CMD],
        },
    ];

    let hierarchy_msg = format!(
        "{} (root PID {} with descendants)",
        colorize("Process hierarchy", DIM_WHITE, no_color),
        colorize(&root_pid.to_string(), BRIGHT_WHITE, no_color)
    );
    lines.push(hierarchy_msg);
    lines.push(String::new());
    lines.push(strip_table_edges(&make_top_border(&columns)));
    lines.push(strip_table_edges(&format_header_row(&columns)));
    lines.push(strip_table_edges(&make_separator_border(&columns)));
    for row in &rows {
        let values = inspect_process_row_values(row, &user_colors, no_color);
        lines.push(strip_table_edges(&format_row_cells(
            &values, &columns, no_color,
        )));
    }
    lines.push(strip_table_edges(&make_bottom_border(&columns)));
    lines
}

/// Finds the first live spawned descendant PID to use as inspect-table fallback root.
fn find_live_spawn_root_pid(
    nodes: &[SpawnedProcessNode],
    system: &System,
) -> Option<u32> {
    for node in nodes {
        if system.process(SysPid::from_u32(node.child.pid)).is_some() {
            return Some(node.child.pid);
        }
        if let Some(pid) = find_live_spawn_root_pid(&node.children, system) {
            return Some(pid);
        }
    }
    None
}

/// Walks the process tree rooted at `pid` and appends formatted table rows in tree order.
fn append_inspect_process_rows(
    context: &InspectProcessContext<'_>,
    pid: u32,
    display_prefix: &str,
    child_indent: &str,
    is_root: bool,
    rows: &mut Vec<InspectProcessRow>,
) {
    let Some(process) = context.system.process(SysPid::from_u32(pid)) else {
        return;
    };

    let name = process_display_name(process);
    let tree_label = if is_root {
        name.clone()
    } else {
        format!("{display_prefix}{name}")
    };
    let user = process
        .user_id()
        .and_then(|uid| context.users.get_user_by_id(uid))
        .map(|entry| entry.name().to_string())
        .unwrap_or_else(|| "-".to_string());
    let virt_bytes = process.virtual_memory().saturating_mul(1024);
    let res_bytes = process.memory().saturating_mul(1024);
    let mem_percent = if context.total_memory > 0.0 {
        (process.memory() as f64 / context.total_memory) * 100.0
    } else {
        0.0
    };
    let linux_stats = read_linux_proc_stats(pid);
    let ppid = process
        .parent()
        .map(|parent| parent.as_u32())
        .or(linux_stats.ppid);
    let state = process_status_code(process.status());
    let cpu_time = linux_stats
        .cpu_ticks
        .map(format_cpu_time_from_ticks)
        .unwrap_or_else(|| format_inspect_elapsed(process.run_time()));
    let command = process_command_line(process);

    rows.push(InspectProcessRow {
        tree_label,
        is_root,
        depth: child_indent.len() / 3,
        pid,
        ppid,
        user,
        pri: linux_stats.priority,
        nice: linux_stats.nice,
        virt_bytes,
        res_bytes,
        shared_bytes: linux_stats.shared_bytes,
        state,
        cpu_percent: process.cpu_usage(),
        mem_percent,
        cpu_time,
        command,
    });

    if let Some(children) = context.children_by_parent.get(&pid) {
        for (index, child_pid) in children.iter().enumerate() {
            let is_last = index + 1 == children.len();
            let branch = if is_last { "└─ " } else { "├─ " };
            let child_display_prefix = format!("{child_indent}{branch}");
            let next_child_indent =
                format!("{}{}", child_indent, if is_last { "   " } else { "│  " });
            append_inspect_process_rows(
                context,
                *child_pid,
                &child_display_prefix,
                &next_child_indent,
                false,
                rows,
            );
        }
    }
}

/// Formats a generic table row using dynamic columns with control-character sanitization.
fn format_row_cells(values: &[String], columns: &[Column], _no_color: bool) -> String {
    let mut row = String::from('│');
    for (value, column) in values.iter().zip(columns.iter()) {
        let sanitized = sanitize_table_cell(value);
        row.push(' ');
        row.push_str(&ansi_pad(&sanitized, column.width, column.align));
        row.push(' ');
        row.push('│');
    }
    row
}

/// Normalizes cell text to a single printable line while preserving ANSI color escape sequences.
fn sanitize_table_cell(value: &str) -> String {
    let mut collapsed = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            collapsed.push(ch);
            for next in chars.by_ref() {
                collapsed.push(next);
                if next == 'm' {
                    break;
                }
            }
            continue;
        }

        if matches!(ch, '\n' | '\r' | '\t') {
            collapsed.push(' ');
            continue;
        }

        if ch.is_control() {
            collapsed.push(' ');
            continue;
        }

        collapsed.push(ch);
    }

    collapsed.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Returns a display-friendly process name from sysinfo process metadata.
fn process_display_name(process: &sysinfo::Process) -> String {
    process.name().to_string_lossy().into_owned()
}

/// Returns the full command line when available, otherwise falls back to process display name.
fn process_command_line(process: &sysinfo::Process) -> String {
    if process.cmd().is_empty() {
        process_display_name(process)
    } else {
        process
            .cmd()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Converts sysinfo's status enum into a compact single-letter process state marker.
fn process_status_code(status: sysinfo::ProcessStatus) -> String {
    format!("{status:?}")
        .chars()
        .next()
        .map(|ch| ch.to_string())
        .unwrap_or_else(|| "?".to_string())
}

#[cfg(target_os = "linux")]
/// Reads Linux `/proc` process stats used by inspect table columns (PPID, PRI/NI, CPU ticks, SHR).
fn read_linux_proc_stats(pid: u32) -> LinuxProcStats {
    let stat_path = format!("/proc/{pid}/stat");
    let statm_path = format!("/proc/{pid}/statm");
    let mut stats = LinuxProcStats::default();

    if let Ok(contents) = fs::read_to_string(stat_path)
        && let Some(parsed) = parse_proc_stat_line(&contents)
    {
        stats.ppid = parsed.ppid;
        stats.priority = parsed.priority;
        stats.nice = parsed.nice;
        stats.cpu_ticks = parsed.cpu_ticks;
    }

    if let Ok(contents) = fs::read_to_string(statm_path) {
        let mut fields = contents.split_whitespace();
        let _size = fields.next();
        let _resident = fields.next();
        if let Some(shared_pages) = fields.next()
            && let Ok(pages) = shared_pages.parse::<u64>()
        {
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            let page_size = if page_size > 0 {
                page_size as u64
            } else {
                4096
            };
            stats.shared_bytes = Some(pages.saturating_mul(page_size));
        }
    }

    stats
}

#[cfg(not(target_os = "linux"))]
/// Non-Linux stub returning empty Linux-specific process stats.
fn read_linux_proc_stats(_pid: u32) -> LinuxProcStats {
    LinuxProcStats::default()
}

#[cfg(target_os = "linux")]
/// Parses a `/proc/<pid>/stat` line into selected fields needed for inspect table rendering.
fn parse_proc_stat_line(contents: &str) -> Option<LinuxProcStats> {
    let closing_paren = contents.rfind(')')?;
    let remainder = contents.get((closing_paren + 1)..)?.trim();
    let fields: Vec<&str> = remainder.split_whitespace().collect();
    if fields.len() <= 16 {
        return None;
    }

    let ppid = fields.get(1)?.parse::<u32>().ok();
    let utime = fields.get(11)?.parse::<u64>().ok();
    let stime = fields.get(12)?.parse::<u64>().ok();
    let priority = fields.get(15)?.parse::<i64>().ok();
    let nice = fields.get(16)?.parse::<i64>().ok();

    Some(LinuxProcStats {
        ppid,
        priority,
        nice,
        cpu_ticks: match (utime, stime) {
            (Some(u), Some(s)) => Some(u.saturating_add(s)),
            _ => None,
        },
        shared_bytes: None,
    })
}

/// Formats CPU clock ticks as `MM:SS.CC` time display.
fn format_cpu_time_from_ticks(ticks: u64) -> String {
    #[cfg(target_os = "linux")]
    let hz = {
        let raw = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if raw > 0 { raw as u64 } else { 100 }
    };

    #[cfg(not(target_os = "linux"))]
    let hz = 100;

    let hundredths = ticks.saturating_mul(100) / hz.max(1);
    let secs = hundredths / 100;
    let centis = hundredths % 100;
    let mins = secs / 60;
    let rem_secs = secs % 60;
    format!("{mins:02}:{rem_secs:02}.{centis:02}")
}

/// Filters samples.
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

/// Returns the overall health from unit.
fn overall_health_from_unit(unit: &UnitStatus) -> OverallHealth {
    match unit.health {
        UnitHealth::Healthy => OverallHealth::Healthy,
        UnitHealth::Degraded => OverallHealth::Degraded,
        UnitHealth::Failing => OverallHealth::Failing,
        UnitHealth::Inactive => OverallHealth::Healthy,
    }
}
