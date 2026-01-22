//! ASCII charting for metrics visualization using our custom rasciigraph.

use std::error::Error;

use chrono::Local;

use crate::metrics::MetricSample;

pub mod rasciigraph;
use self::rasciigraph::{Config, plot};

/// Configuration for chart rendering.
pub struct ChartConfig {
    /// Whether to disable colors in output.
    pub no_color: bool,
    /// Time window description for the title.
    pub window_desc: String,
}

/// Render metrics as ASCII charts using rasciigraph.
pub fn render_metrics_chart(
    samples: &[MetricSample],
    config: &ChartConfig,
) -> Result<(), Box<dyn Error>> {
    if samples.is_empty() {
        println!("No data available for the specified time window.");
        return Ok(());
    }

    let cpu_values: Vec<f64> = samples.iter().map(|s| s.cpu_percent as f64).collect();
    let mem_gb_values: Vec<f64> = samples
        .iter()
        .map(|s| s.rss_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        .collect();

    let cpu_resampled = resample_to_width(&cpu_values, 50);
    let mem_resampled = resample_to_width(&mem_gb_values, 50);

    let cpu_resampled: Vec<f64> = cpu_resampled
        .iter()
        .map(|&v| if v.is_finite() { v } else { 0.0 })
        .collect();
    let mem_resampled: Vec<f64> = mem_resampled
        .iter()
        .map(|&v| if v.is_finite() { v } else { 0.0 })
        .collect();

    let cpu_min = cpu_resampled.iter().cloned().fold(f64::INFINITY, f64::min);
    let cpu_max = cpu_resampled
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    let cpu_final = if (cpu_max - cpu_min).abs() < 0.00001 {
        cpu_resampled
            .iter()
            .enumerate()
            .map(|(i, &v)| v + (i as f64 * 0.00001))
            .collect()
    } else {
        cpu_resampled
    };

    let mem_min = mem_resampled.iter().cloned().fold(f64::INFINITY, f64::min);
    let mem_max = mem_resampled
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    let mem_final = if (mem_max - mem_min).abs() < 0.00001 {
        mem_resampled
            .iter()
            .enumerate()
            .map(|(i, &v)| v + (i as f64 * 0.00001))
            .collect()
    } else {
        mem_resampled
    };

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

    println!("CPU Usage (%):");
    let cpu_caption = format!("{} - {} to {}", config.window_desc, first_time, last_time);

    let cpu_graph = plot(
        cpu_final,
        Config::default()
            .with_height(10)
            .with_y_precision(4)
            .with_caption(cpu_caption),
    );

    if !config.no_color {
        for line in cpu_graph.lines() {
            println!("\x1b[36m{}\x1b[0m", line);
        }
    } else {
        println!("{}", cpu_graph);
    }

    println!();

    println!("Memory Usage (GB):");
    let mem_caption = format!("{} - {} to {}", config.window_desc, first_time, last_time);

    let mem_graph = plot(
        mem_final,
        Config::default()
            .with_height(10)
            .with_y_precision(4)
            .with_caption(mem_caption),
    );

    if !config.no_color {
        for line in mem_graph.lines() {
            println!("\x1b[35m{}\x1b[0m", line);
        }
    } else {
        println!("{}", mem_graph);
    }

    println!();
    println!("Summary Statistics:");

    let cpu_avg = if !cpu_values.is_empty() {
        cpu_values.iter().sum::<f64>() / cpu_values.len() as f64
    } else {
        0.0
    };
    let cpu_max = cpu_values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let cpu_min = cpu_values.iter().cloned().fold(f64::INFINITY, f64::min);

    let mem_avg = if !mem_gb_values.is_empty() {
        mem_gb_values.iter().sum::<f64>() / mem_gb_values.len() as f64
    } else {
        0.0
    };
    let mem_max = mem_gb_values
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, f64::max);
    let mem_min = mem_gb_values.iter().cloned().fold(f64::INFINITY, f64::min);

    println!(
        "  CPU:     min={:.1}% avg={:.1}% max={:.1}%",
        if cpu_min.is_finite() { cpu_min } else { 0.0 },
        cpu_avg,
        if cpu_max.is_finite() { cpu_max } else { 0.0 }
    );
    println!(
        "  Memory:  min={:.4}GB avg={:.4}GB max={:.4}GB",
        if mem_min.is_finite() { mem_min } else { 0.0 },
        mem_avg,
        if mem_max.is_finite() { mem_max } else { 0.0 }
    );
    println!("  Samples: {}", samples.len());

    Ok(())
}

/// Resample data to a specific width by interpolation or repetition
fn resample_to_width(data: &[f64], target_width: usize) -> Vec<f64> {
    if data.is_empty() {
        return vec![0.0; target_width];
    }

    if data.len() == 1 {
        // If we have only one sample, repeat it
        return vec![data[0]; target_width];
    }

    if data.len() >= target_width {
        // If we have more samples than needed, downsample
        let step = data.len() as f64 / target_width as f64;
        return (0..target_width)
            .map(|i| {
                let idx = (i as f64 * step) as usize;
                data[idx.min(data.len() - 1)]
            })
            .collect();
    }

    // If we have fewer samples, interpolate
    let mut result = Vec::with_capacity(target_width);
    let scale = (data.len() - 1) as f64 / (target_width - 1) as f64;

    for i in 0..target_width {
        let pos = i as f64 * scale;
        let idx = pos.floor() as usize;
        let frac = pos - idx as f64;

        if idx + 1 < data.len() {
            let val = data[idx] * (1.0 - frac) + data[idx + 1] * frac;
            result.push(val);
        } else {
            result.push(data[idx]);
        }
    }

    result
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_window_duration() {
        assert_eq!(parse_window_duration("5s").unwrap(), 5);
        assert_eq!(parse_window_duration("2m").unwrap(), 120);
        assert_eq!(parse_window_duration("1h").unwrap(), 3600);
        assert_eq!(parse_window_duration("1d").unwrap(), 86400);
        assert_eq!(parse_window_duration("1w").unwrap(), 604800);

        assert!(parse_window_duration("").is_err());
        assert!(parse_window_duration("-5s").is_err());
        assert!(parse_window_duration("invalid").is_err());
    }

    #[test]
    fn test_resample_to_width() {
        // Test with single value
        assert_eq!(resample_to_width(&[5.0], 3), vec![5.0, 5.0, 5.0]);

        // Test with exact match
        assert_eq!(resample_to_width(&[1.0, 2.0, 3.0], 3), vec![1.0, 2.0, 3.0]);

        // Test upsampling
        let upsampled = resample_to_width(&[0.0, 10.0], 5);
        assert_eq!(upsampled.len(), 5);
        assert_eq!(upsampled[0], 0.0);
        assert_eq!(upsampled[4], 10.0);

        // Test downsampling
        let downsampled = resample_to_width(&[1.0, 2.0, 3.0, 4.0, 5.0], 3);
        assert_eq!(downsampled.len(), 3);
    }
}
