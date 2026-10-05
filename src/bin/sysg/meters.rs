/// htop's default refresh delay (`DEFAULT_DELAY` = 15 tenths of a second).
///
/// CPU usage is the delta between two cumulative counter reads, so the window
/// between them is what the percentage averages over.
const METER_INTERVAL: Duration = Duration::from_millis(1500);

/// Smallest bar interior that still reads as a bar.
const METER_MIN_BAR: usize = 8;

/// Holds the host counters between CPU reads.
struct MeterSampler {
    system: System,
    primed_at: Instant,
}

/// One reading of the host, taken by [`MeterSampler::sample`].
struct HostMeters {
    cpus: Vec<f32>,
    mem_used: u64,
    mem_total: u64,
    swap_used: u64,
    swap_total: u64,
    tasks: TaskCounts,
    load: [f64; 3],
    uptime_secs: u64,
}

/// htop's `Tasks:` meter. Thread and running counts are only trustworthy on Linux.
struct TaskCounts {
    procs: usize,
    threads: Option<usize>,
    kthreads: Option<usize>,
    running: Option<usize>,
}

impl MeterSampler {
    /// Takes the first CPU read. Call before slow work so it overlaps the window.
    fn prime() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        Self {
            system,
            primed_at: Instant::now(),
        }
    }

    /// Waits out the rest of the window, then reads every meter.
    fn sample(&mut self) -> HostMeters {
        if let Some(rest) = METER_INTERVAL.checked_sub(self.primed_at.elapsed()) {
            thread::sleep(rest);
        }
        self.system.refresh_cpu_usage();
        self.primed_at = Instant::now();
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );

        let load = System::load_average();
        HostMeters {
            cpus: self.system.cpus().iter().map(|cpu| cpu.cpu_usage()).collect(),
            mem_used: self.system.used_memory(),
            mem_total: self.system.total_memory(),
            swap_used: self.system.used_swap(),
            swap_total: self.system.total_swap(),
            tasks: count_tasks(&self.system),
            load: [load.one, load.five, load.fifteen],
            uptime_secs: System::uptime(),
        }
    }
}

fn count_tasks(system: &System) -> TaskCounts {
    let mut procs = 0;
    let mut threads = 0;
    let mut kthreads = 0;
    let mut running = 0;
    for process in system.processes().values() {
        match process.thread_kind() {
            Some(sysinfo::ThreadKind::Kernel) => kthreads += 1,
            Some(sysinfo::ThreadKind::Userland) => threads += 1,
            None => procs += 1,
        }
        if process.status() == ProcessStatus::Run {
            running += 1;
        }
    }
    let linux = cfg!(target_os = "linux");
    TaskCounts {
        procs,
        threads: linux.then_some(threads),
        kthreads: linux.then_some(kthreads),
        running: linux.then_some(running),
    }
}

/// Renders the meters as plain-width lines exactly `width` columns wide.
fn meter_lines(meters: &HostMeters, width: usize, no_color: bool) -> Vec<String> {
    if width < 2 * (4 + 2 + METER_MIN_BAR) + 2 {
        return Vec::new();
    }
    let mut lines = cpu_meter_lines(&meters.cpus, width, no_color);

    let left_width = width / 2;
    let right_width = width.saturating_sub(left_width + 2);
    let mem = meter_bar(
        "Mem",
        percent(meters.mem_used, meters.mem_total),
        &format!(
            "{}/{}",
            human_bytes(meters.mem_used),
            human_bytes(meters.mem_total)
        ),
        left_width,
        no_color,
    );
    let swp = meter_bar(
        "Swp",
        percent(meters.swap_used, meters.swap_total),
        &format!(
            "{}/{}",
            human_bytes(meters.swap_used),
            human_bytes(meters.swap_total)
        ),
        left_width,
        no_color,
    );
    let side = [
        format_tasks(&meters.tasks),
        format!(
            "Load average: {:.2} {:.2} {:.2}",
            meters.load[0], meters.load[1], meters.load[2]
        ),
        format!("Uptime: {}", format_host_uptime(meters.uptime_secs)),
    ];
    let left = [mem, swp, " ".repeat(left_width)];

    for (left, right) in left.iter().zip(side.iter()) {
        let right: String = right.chars().take(right_width).collect();
        lines.push(format!("{left}  {right:<right_width$}"));
    }
    lines
}

/// Lays CPU bars out column-major across up to four columns, like htop.
fn cpu_meter_lines(cpus: &[f32], width: usize, no_color: bool) -> Vec<String> {
    if cpus.is_empty() {
        return Vec::new();
    }
    let min_cell = 4 + 2 + METER_MIN_BAR;
    let fit = (width + 1) / (min_cell + 1);
    let cols = cpus.len().min(4).min(fit.max(1));
    let rows = cpus.len().div_ceil(cols);
    let cell = (width + 1) / cols - 1;
    let tall = cpus.len() % cols;

    let mut starts = Vec::with_capacity(cols);
    let mut next = 0;
    for col in 0..cols {
        starts.push(next);
        next += if tall == 0 || col < tall { rows } else { rows - 1 };
    }

    (0..rows)
        .map(|row| {
            let cells: Vec<String> = (0..cols)
                .map(|col| {
                    let index = starts[col] + row;
                    let end = starts.get(col + 1).copied().unwrap_or(cpus.len());
                    if index < end {
                        let usage = cpus[index];
                        meter_bar(
                            &index.to_string(),
                            f64::from(usage),
                            &format!("{usage:.1}%"),
                            cell,
                            no_color,
                        )
                    } else {
                        " ".repeat(cell)
                    }
                })
                .collect();
            let line = cells.join(" ");
            let used = cols * cell + cols - 1;
            format!("{line}{}", " ".repeat(width.saturating_sub(used)))
        })
        .collect()
}

/// Draws `label[|||||    text]` in exactly `width` columns, text over the bar like htop.
fn meter_bar(label: &str, percent: f64, text: &str, width: usize, no_color: bool) -> String {
    let inner = width.saturating_sub(4 + 2);
    let text: String = text.chars().take(inner).collect();
    let track = inner - text.len();
    let bars = ((percent.clamp(0.0, 100.0) / 100.0) * track as f64).round() as usize;

    let mut bar = String::new();
    if no_color {
        bar.push_str(&"|".repeat(bars));
    } else {
        let truecolor = std::env::var("COLORTERM")
            .is_ok_and(|value| matches!(value.as_str(), "truecolor" | "24bit"));
        let mut last = String::new();
        for i in 0..bars {
            let code = gradient_code(gradient_at(i as f64 / track.saturating_sub(1).max(1) as f64), truecolor);
            if code != last {
                bar.push_str(&code);
                last = code;
            }
            bar.push('|');
        }
        if bars > 0 {
            bar.push_str(RESET);
        }
    }
    format!(
        "{label:>4}[{bar}{}{text}]",
        " ".repeat(track - bars)
    )
}

/// Green at the empty end of a bar, yellow halfway, red at the full end.
fn gradient_at(t: f64) -> (u8, u8, u8) {
    const GREEN: (f64, f64, f64) = (80.0, 200.0, 80.0);
    const YELLOW: (f64, f64, f64) = (230.0, 200.0, 40.0);
    const RED: (f64, f64, f64) = (230.0, 60.0, 60.0);
    let t = t.clamp(0.0, 1.0);
    let (from, to, k) = if t < 0.5 {
        (GREEN, YELLOW, t * 2.0)
    } else {
        (YELLOW, RED, (t - 0.5) * 2.0)
    };
    let mix = |a: f64, b: f64| (a + (b - a) * k).round() as u8;
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2))
}

/// Foreground escape for a color, falling back to the nearest xterm-256 cube entry.
fn gradient_code((r, g, b): (u8, u8, u8), truecolor: bool) -> String {
    if truecolor {
        return format!("\x1b[38;2;{r};{g};{b}m");
    }
    let level = |c: u8| (u16::from(c) * 5 + 127) / 255;
    format!(
        "\x1b[38;5;{}m",
        16 + 36 * level(r) + 6 * level(g) + level(b)
    )
}

fn percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 / total as f64 * 100.0
    }
}

/// Formats bytes the way htop's meters do: `25.2G`, `8.36G`, `512M`.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["K", "M", "G", "T", "P"];
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let suffix = UNITS[unit];
    if value < 10.0 {
        format!("{value:.2}{suffix}")
    } else if value < 100.0 {
        format!("{value:.1}{suffix}")
    } else {
        format!("{value:.0}{suffix}")
    }
}

fn format_tasks(tasks: &TaskCounts) -> String {
    let mut text = format!("Tasks: {}", tasks.procs);
    if let Some(threads) = tasks.threads {
        text.push_str(&format!(", {threads} thr"));
    }
    if let Some(kthreads) = tasks.kthreads {
        text.push_str(&format!(", {kthreads} kthr"));
    }
    if let Some(running) = tasks.running {
        text.push_str(&format!("; {running} running"));
    }
    text
}

/// Formats host uptime like htop: `7 days, 01:14:54`.
fn format_host_uptime(secs: u64) -> String {
    let days = secs / 86_400;
    let clock = format!(
        "{:02}:{:02}:{:02}",
        (secs % 86_400) / 3600,
        (secs % 3600) / 60,
        secs % 60
    );
    match days {
        0 => clock,
        1 => format!("1 day, {clock}"),
        _ => format!("{days} days, {clock}"),
    }
}
