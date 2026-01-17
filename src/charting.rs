//! ASCII charting for metrics visualization using rasciichart.

use std::error::Error;

use chrono::Local;
use rasciichart::plot_sized;

use crate::metrics::MetricSample;

/// Configuration for chart rendering.
pub struct ChartConfig {
    /// Whether to disable colors in output.
    pub no_color: bool,
    /// Whether this is for live/tail mode.
    pub is_live: bool,
    /// Time window description for the title.
    pub window_desc: String,
}

/// Render metrics as ASCII charts using rasciichart.
pub fn render_metrics_chart(
    samples: &[MetricSample],
    config: &ChartConfig,
) -> Result<(), Box<dyn Error>> {
    if samples.is_empty() {
        println!("No data available for the specified time window.");
        return Ok(());
    }

    const CHART_WIDTH: usize = 78;
    const CHART_HEIGHT: usize = 20;

    // Prepare data
    let cpu_values: Vec<f64> = samples.iter().map(|s| s.cpu_percent as f64).collect();
    let mem_values: Vec<f64> = samples
        .iter()
        .map(|s| s.rss_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        .collect();

    // Title
    let title = if config.is_live {
        format!("Resource Usage - Live ({})", config.window_desc)
    } else {
        format!("Resource Usage - Past {}", config.window_desc)
    };

    // Colors  - cyan for CPU, magenta for Memory
    let (cpu_color, mem_color, reset) = if config.no_color {
        ("", "", "")
    } else {
        ("\x1b[36m", "\x1b[35m", "\x1b[0m")
    };

    // Print title
    println!();
    println!("{:^78}", title);
    println!();

    // Plot CPU usage with color
    println!("{}CPU Usage (%):{}", cpu_color, reset);
    let cpu_chart = plot_sized(&cpu_values, CHART_HEIGHT, CHART_WIDTH);
    // Color each line of the CPU chart
    for line in cpu_chart.lines() {
        println!("{}{}{}", cpu_color, line, reset);
    }

    // Plot Memory usage with color
    println!();
    println!("{}Memory Usage (GB):{}", mem_color, reset);
    let mem_chart = plot_sized(&mem_values, CHART_HEIGHT, CHART_WIDTH);
    // Color each line of the memory chart
    for line in mem_chart.lines() {
        println!("{}{}{}", mem_color, line, reset);
    }

    // Time labels
    if !samples.is_empty() {
        let first_time = samples
            .first()
            .unwrap()
            .timestamp
            .with_timezone(&Local)
            .format("%H:%M:%S")
            .to_string();
        let last_time = samples
            .last()
            .unwrap()
            .timestamp
            .with_timezone(&Local)
            .format("%H:%M:%S")
            .to_string();

        println!();
        println!("       {:<38}{:>38}", first_time, last_time);
        println!("{:^78}", "Time");
    }

    println!();

    Ok(())
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
