//! Gnuplot-based charting for metrics visualization.

use chrono::Local;
use std::error::Error;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::metrics::MetricSample;

/// Check if gnuplot is available on the system.
pub fn gnuplot_available() -> bool {
    Command::new("gnuplot")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Configuration for chart rendering.
pub struct ChartConfig {
    /// Whether to disable colors in output.
    pub no_color: bool,
    /// Whether this is for live/tail mode.
    pub is_live: bool,
    /// Time window description for the title.
    pub window_desc: String,
}

/// Render metrics using gnuplot.
///
/// Falls back to simple text output if gnuplot is not available.
pub fn render_metrics_chart(
    samples: &[MetricSample],
    config: &ChartConfig,
) -> Result<(), Box<dyn Error>> {
    if samples.is_empty() {
        println!("No data available for the specified time window.");
        return Ok(());
    }

    if !gnuplot_available() {
        eprintln!("Note: gnuplot not found. Install it for better chart visualizations:");
        eprintln!("  macOS:    brew install gnuplot");
        eprintln!("  Ubuntu:   apt-get install gnuplot");
        eprintln!("  Fedora:   dnf install gnuplot");
        eprintln!("  Arch:     pacman -S gnuplot");
        eprintln!("\nFalling back to text table output.\n");

        render_text_fallback(samples, config)?;
        return Ok(());
    }

    // Prepare data for gnuplot
    let mut data_points = String::new();
    for sample in samples {
        let timestamp = sample.timestamp.timestamp();
        let cpu = sample.cpu_percent;
        let rss_gb = sample.rss_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        let _ = writeln!(&mut data_points, "{} {} {}", timestamp, cpu, rss_gb);
    }

    // Calculate Y-axis ranges
    let max_cpu = samples
        .iter()
        .map(|s| s.cpu_percent as f64)
        .fold(0.0, f64::max)
        .max(10.0); // Minimum 10% scale

    let max_rss_gb = samples
        .iter()
        .map(|s| s.rss_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        .fold(0.0, f64::max);
    let max_rss_gb = if max_rss_gb < 0.1 {
        0.1
    } else {
        max_rss_gb * 1.5 // Add 50% headroom
    };

    // Get timezone offset for display
    let local_tz = Local::now().format("%Z").to_string();

    // Build gnuplot script
    let terminal = if config.no_color {
        "dumb size 80,24"
    } else {
        "dumb size 80,24 ansirgb"
    };

    let title = if config.is_live {
        format!("Resource Usage - Live ({})", config.window_desc)
    } else {
        format!("Resource Usage - Past {}", config.window_desc)
    };

    let time_format = if config.is_live {
        "%H:%M:%S"
    } else if samples.len() > 100 {
        "%m/%d %H:%M"
    } else {
        "%H:%M"
    };

    let gnuplot_script = format!(
        r#"
set terminal {}
set title "{}"
set xlabel "Time ({})"
set ylabel "CPU (%)"
set y2label "Memory (GB)"
set ytics nomirror
set y2tics
set yrange [0:{}]
set y2range [0:{}]
set grid
set key top right box
set xdata time
set timefmt "%s"
set format x "{}"
set datafile separator " "
plot "-" using 1:2 title "CPU %" with linespoints axes x1y1 lc rgb "green", \
     "-" using 1:3 title "RSS (GB)" with linespoints axes x1y2 lc rgb "yellow"
"#,
        terminal,
        title,
        local_tz,
        max_cpu * 1.1,
        max_rss_gb,
        time_format
    );

    // Execute gnuplot
    let mut gnuplot = Command::new("gnuplot")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn()?;

    if let Some(stdin) = gnuplot.stdin.as_mut() {
        // Send script
        stdin.write_all(gnuplot_script.as_bytes())?;
        // Send data for CPU plot
        stdin.write_all(data_points.as_bytes())?;
        stdin.write_all(b"e\n")?;
        // Send data again for RSS plot
        stdin.write_all(data_points.as_bytes())?;
        stdin.write_all(b"e\n")?;
    }

    gnuplot.wait()?;

    Ok(())
}

/// Simple text fallback when gnuplot is not available.
fn render_text_fallback(
    samples: &[MetricSample],
    config: &ChartConfig,
) -> Result<(), Box<dyn Error>> {
    println!();
    if config.is_live {
        println!("Resource Usage - Live ({}):", config.window_desc);
    } else {
        println!("Resource Usage - Past {}:", config.window_desc);
    }
    println!();
    println!("{:<24} {:>8} {:>10}", "TIMESTAMP", "CPU %", "RSS");
    println!("{:-<24} {:-<8} {:-<10}", "", "", "");

    // Show first 10 and last 10 samples if more than 20
    let samples_to_show: Vec<&MetricSample> = if samples.len() > 20 {
        let mut shown = Vec::new();
        shown.extend(&samples[..10]);
        shown.push(&samples[samples.len() / 2]); // Middle sample
        shown.extend(&samples[samples.len() - 10..]);
        shown
    } else {
        samples.iter().collect()
    };

    for (i, sample) in samples_to_show.iter().enumerate() {
        // Add separator for skipped samples
        if samples.len() > 20 && i == 10 {
            println!("{:<24} {:>8} {:>10}", "...", "...", "...");
        }

        let timestamp = sample
            .timestamp
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let rss_str = format_bytes(sample.rss_bytes);
        println!(
            "{:<24} {:>7.1}% {:>10}",
            timestamp, sample.cpu_percent, rss_str
        );
    }

    if samples.len() > 20 {
        println!();
        println!(
            "(Showing {} of {} total samples)",
            samples_to_show.len(),
            samples.len()
        );
    }

    Ok(())
}

/// Format bytes as human-readable string.
fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit_idx = 0;

    while value >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{:.0}{}", value, UNITS[unit_idx])
    } else {
        format!("{:.1}{}", value, UNITS[unit_idx])
    }
}

/// Parse a duration string like "5s", "12h", "7d" into seconds.
pub fn parse_window_duration(window: &str) -> Result<u64, String> {
    let window = window.trim();
    if window.is_empty() {
        return Err("Window duration cannot be empty".to_string());
    }

    // Extract numeric part and unit
    let (num_str, unit) = window
        .chars()
        .position(|c| c.is_alphabetic())
        .map(|pos| window.split_at(pos))
        .ok_or_else(|| format!("Invalid window format: {}", window))?;

    let value: f64 = num_str
        .parse()
        .map_err(|_| format!("Invalid number in window: {}", num_str))?;

    if value <= 0.0 {
        return Err("Window duration must be positive".to_string());
    }

    let seconds = match unit.to_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => value,
        "m" | "min" | "mins" | "minute" | "minutes" => value * 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => value * 3600.0,
        "d" | "day" | "days" => value * 86400.0,
        "w" | "week" | "weeks" => value * 604800.0,
        _ => return Err(format!("Unknown time unit: {}", unit)),
    };

    Ok(seconds as u64)
}

/// Determine if a window duration should use live mode.
pub fn is_live_window(seconds: u64) -> bool {
    seconds <= 60
}
