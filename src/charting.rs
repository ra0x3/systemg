//! ASCII charting for metrics visualization.

use std::error::Error;

use crate::metrics::MetricSample;
use chrono::Local;

/// Configuration for chart rendering.
pub struct ChartConfig {
    /// Whether to disable colors in output.
    pub no_color: bool,
    /// Time window description for the title.
    pub window_desc: String,
}

/// Render metrics as ASCII charts.
pub fn render_metrics_chart(
    samples: &[MetricSample],
    config: &ChartConfig,
) -> Result<(), Box<dyn Error>> {
    if samples.is_empty() {
        println!("No data available for the specified time window.");
        return Ok(());
    }

    const CHART_HEIGHT: usize = 18;
    const CHART_WIDTH: usize = CHART_HEIGHT + 25;
    const INDENT: usize = 2;
    const LEFT_LABEL_WIDTH: usize = 7;
    const RIGHT_LABEL_WIDTH: usize = 7;

    let cpu_values: Vec<f64> = samples.iter().map(|s| s.cpu_percent as f64).collect();
    let mem_values: Vec<f64> = samples
        .iter()
        .map(|s| s.rss_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        .collect();

    let (mut cpu_min, mut cpu_max) = compute_range(&cpu_values);
    if cpu_min.is_finite() {
        cpu_min = cpu_min.min(0.0);
    }
    let cpu_span = (cpu_max - cpu_min).abs();
    if cpu_span < f64::EPSILON {
        cpu_max = cpu_min + 1.0;
    } else {
        let padding = (cpu_span * 0.05).max(1.0);
        cpu_min = (cpu_min - padding).min(0.0);
        cpu_max += padding;
    }

    let (mut mem_min, mut mem_max) = compute_range(&mem_values);
    if mem_min.is_finite() {
        mem_min = mem_min.min(0.0);
    }
    let mem_span = (mem_max - mem_min).abs();
    if mem_span < f64::EPSILON {
        let adjustment = if mem_max.abs() < 1.0 {
            0.1
        } else {
            mem_max.abs() * 0.05
        };
        mem_max = mem_min + adjustment;
    } else {
        let padding = (mem_span * 0.05).max(0.05);
        mem_min = (mem_min - padding).min(0.0);
        mem_max += padding;
    }

    let cpu_series = resample_series(&cpu_values, CHART_WIDTH);
    let mem_series = resample_series(&mem_values, CHART_WIDTH);

    let cpu_rows = map_series_to_rows(&cpu_series, cpu_min, cpu_max, CHART_HEIGHT);
    let mem_rows = map_series_to_rows(&mem_series, mem_min, mem_max, CHART_HEIGHT);

    let mut grid = vec![vec![Cell::Empty; CHART_WIDTH]; CHART_HEIGHT];

    draw_series(&mut grid, &cpu_rows, Series::Cpu);
    draw_series(&mut grid, &mem_rows, Series::Mem);

    let baseline_row = row_for_value(0.0, cpu_min, cpu_max, CHART_HEIGHT);

    const CPU_COLOR: &str = "\x1b[36m";
    const MEM_COLOR: &str = "\x1b[35m";
    const BOTH_COLOR: &str = "\x1b[33m";
    const RESET: &str = "\x1b[0m";

    let title = format!("Resource Usage ({})", config.window_desc);
    println!();
    let title_padding =
        INDENT + LEFT_LABEL_WIDTH + 2 + (CHART_WIDTH.saturating_sub(title.len()) / 2);
    println!("{:width$}{}", "", title, width = title_padding);
    println!();

    for (row, cells) in grid.iter().enumerate() {
        let cpu_label = format_cpu_axis_label(row, CHART_HEIGHT, cpu_min, cpu_max);
        let mem_label = format_mem_axis_label(row, CHART_HEIGHT, mem_min, mem_max);
        let is_baseline = baseline_row.is_some_and(|r| r == row);
        let left_axis = if row == CHART_HEIGHT - 1 {
            if is_baseline { '┴' } else { '└' }
        } else if is_baseline {
            '├'
        } else {
            '│'
        };
        let right_axis = if row == CHART_HEIGHT - 1 {
            if is_baseline { '┴' } else { '┘' }
        } else if is_baseline {
            '┤'
        } else {
            '│'
        };

        let mut line = String::with_capacity(CHART_WIDTH * 3);
        let mut active_color: Option<&str> = None;

        for cell in cells {
            let (mut ch, color) = match cell {
                Cell::Empty => (' ', None),
                Cell::Cpu => ('x', Some(CPU_COLOR)),
                Cell::Mem => ('o', Some(MEM_COLOR)),
                Cell::Both => ('*', Some(BOTH_COLOR)),
            };

            if ch == ' ' && is_baseline {
                ch = '─';
            }

            if ch == ' ' {
                if !config.no_color && active_color.is_some() {
                    line.push_str(RESET);
                    active_color = None;
                }
                line.push(' ');
                continue;
            }

            if config.no_color {
                line.push(ch);
                continue;
            }

            if active_color != color {
                if active_color.is_some() {
                    line.push_str(RESET);
                }
                if let Some(c) = color {
                    line.push_str(c);
                }
                active_color = color;
            }

            line.push(ch);
        }

        if !config.no_color && active_color.is_some() {
            line.push_str(RESET);
        }

        println!(
            "{:indent$}{:>lwidth$} {}{}{} {:<rwidth$}",
            "",
            cpu_label,
            left_axis,
            line,
            right_axis,
            mem_label,
            indent = INDENT,
            lwidth = LEFT_LABEL_WIDTH,
            rwidth = RIGHT_LABEL_WIDTH,
        );
    }

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
    let axis_indent = INDENT + LEFT_LABEL_WIDTH + 2;
    let spacing = CHART_WIDTH
        .saturating_sub(first_time.len() + last_time.len())
        .max(1);
    print!("{:width$}{}", "", first_time, width = axis_indent);
    print!("{:width$}", "", width = spacing);
    println!("{}", last_time);

    let time_label = "Time";
    let time_padding = axis_indent + CHART_WIDTH.saturating_sub(time_label.len()) / 2;
    println!("{:width$}{}", "", time_label, width = time_padding);

    println!();
    print!("{:indent$}Legend: ", "", indent = INDENT);
    if config.no_color {
        println!("x CPU%   o RSS (GB)   * overlap");
    } else {
        println!(
            "{}x{} CPU%   {}o{} RSS (GB)   {}*{} overlap",
            CPU_COLOR, RESET, MEM_COLOR, RESET, BOTH_COLOR, RESET
        );
    }
    println!(
        "{:indent$}Left axis: CPU %, Right axis: RSS (GB)",
        "",
        indent = INDENT
    );

    Ok(())
}

fn compute_range(values: &[f64]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &value in values {
        if value.is_finite() {
            min = min.min(value);
            max = max.max(value);
        }
    }

    if !min.is_finite() || !max.is_finite() {
        return (0.0, 0.0);
    }

    if (max - min).abs() < f64::EPSILON {
        if min == 0.0 {
            (0.0, 1.0)
        } else {
            let delta = (min.abs() * 0.05).max(0.05);
            (min - delta, max + delta)
        }
    } else {
        (min, max)
    }
}

fn map_series_to_rows(series: &[f64], min: f64, max: f64, height: usize) -> Vec<usize> {
    let mut rows = Vec::with_capacity(series.len());
    let mut last_row = height.saturating_sub(1);
    for &value in series {
        if !value.is_finite() || max <= min || height == 0 {
            rows.push(last_row);
            continue;
        }

        let ratio = (value - min) / (max - min);
        let clamped = ratio.clamp(0.0, 1.0);
        let y = ((1.0 - clamped) * (height as f64 - 1.0)).round() as isize;
        let y = y.clamp(0, height as isize - 1) as usize;
        rows.push(y);
        last_row = y;
    }

    rows
}

fn draw_series(grid: &mut [Vec<Cell>], rows: &[usize], series: Series) {
    if grid.is_empty() || grid[0].is_empty() || rows.is_empty() {
        return;
    }

    let mut prev: Option<(usize, usize)> = None;
    for (x, &row) in rows.iter().enumerate() {
        if let Some((px, py)) = prev {
            draw_segment(grid, px, py, x, row, series);
        }
        set_cell(&mut grid[row][x], series);
        prev = Some((x, row));
    }
}

fn draw_segment(
    grid: &mut [Vec<Cell>],
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    series: Series,
) {
    let height = grid.len();
    let width = grid[0].len();

    let dx = x1 as isize - x0 as isize;
    let dy = y1 as isize - y0 as isize;
    let steps = dx.abs().max(dy.abs()).max(1);

    for step in 0..=steps {
        let t = step as f64 / steps as f64;
        let x = x0 as f64 + dx as f64 * t;
        let y = y0 as f64 + dy as f64 * t;
        let xi = x.round() as isize;
        let yi = y.round() as isize;

        if xi < 0 || yi < 0 {
            continue;
        }
        let xi = xi as usize;
        let yi = yi as usize;
        if xi >= width || yi >= height {
            continue;
        }

        set_cell(&mut grid[yi][xi], series);
    }
}

fn set_cell(cell: &mut Cell, series: Series) {
    *cell = match (*cell, series) {
        (Cell::Empty, Series::Cpu) => Cell::Cpu,
        (Cell::Empty, Series::Mem) => Cell::Mem,
        (Cell::Cpu, Series::Cpu) => Cell::Cpu,
        (Cell::Mem, Series::Mem) => Cell::Mem,
        (Cell::Cpu, Series::Mem) | (Cell::Mem, Series::Cpu) | (Cell::Both, _) => {
            Cell::Both
        }
    };
}

fn format_cpu_axis_label(row: usize, height: usize, min: f64, max: f64) -> String {
    axis_value(row, height, min, max)
        .map(format_cpu_value)
        .unwrap_or_default()
}

fn format_mem_axis_label(row: usize, height: usize, min: f64, max: f64) -> String {
    axis_value(row, height, min, max)
        .map(format_mem_value)
        .unwrap_or_default()
}

fn axis_value(row: usize, height: usize, min: f64, max: f64) -> Option<f64> {
    if height == 0 {
        return None;
    }

    if (max - min).abs() < f64::EPSILON {
        if row == 0 || row == height / 2 || row == height - 1 {
            return Some(max);
        }
        return None;
    }

    if row == 0 {
        Some(max)
    } else if row == height / 2 {
        Some((max + min) / 2.0)
    } else if row == height - 1 {
        Some(min)
    } else {
        None
    }
}

fn format_cpu_value(value: f64) -> String {
    let abs = value.abs();
    if abs >= 100.0 {
        format!("{value:.0}%")
    } else if abs >= 10.0 {
        format!("{value:.1}%")
    } else {
        format!("{value:.2}%")
    }
}

fn format_mem_value(value: f64) -> String {
    let abs = value.abs();
    if abs >= 100.0 {
        format!("{value:.0}G")
    } else if abs >= 10.0 {
        format!("{value:.1}G")
    } else {
        format!("{value:.2}G")
    }
}

#[derive(Clone, Copy)]
enum Cell {
    Empty,
    Cpu,
    Mem,
    Both,
}

#[derive(Clone, Copy)]
enum Series {
    Cpu,
    Mem,
}

fn row_for_value(value: f64, min: f64, max: f64, height: usize) -> Option<usize> {
    if height == 0 || max <= min || !value.is_finite() {
        return None;
    }

    let ratio = (value - min) / (max - min);
    let clamped = ratio.clamp(0.0, 1.0);
    let y = ((1.0 - clamped) * (height as f64 - 1.0)).round() as isize;
    Some(y.clamp(0, height as isize - 1) as usize)
}

fn resample_series(series: &[f64], width: usize) -> Vec<f64> {
    if width == 0 {
        return Vec::new();
    }
    if series.is_empty() {
        return vec![0.0; width];
    }
    if series.len() == 1 {
        return vec![series[0]; width];
    }
    if width == 1 {
        return vec![series[0]];
    }

    let step = (series.len() - 1) as f64 / (width - 1) as f64;
    (0..width)
        .map(|i| {
            let pos = i as f64 * step;
            let idx = pos.floor() as usize;
            let frac = pos - idx as f64;
            if idx + 1 < series.len() {
                let a = series[idx];
                let b = series[idx + 1];
                a + (b - a) * frac
            } else {
                series[idx]
            }
        })
        .collect()
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
    fn resample_handles_single_value() {
        let series = vec![5.0];
        let result = resample_series(&series, 5);
        assert_eq!(result, vec![5.0; 5]);
    }

    #[test]
    fn compute_range_expands_zero_delta() {
        let (min, max) = compute_range(&[3.0, 3.0, 3.0]);
        assert!(max > min);
    }

    #[test]
    fn axis_labels_present_for_key_rows() {
        let labels: Vec<String> = (0..5)
            .map(|row| format_cpu_axis_label(row, 5, -2.0, 8.0))
            .collect();
        assert!(!labels[0].is_empty());
        assert!(!labels[4].is_empty());
    }

    #[test]
    fn baseline_row_exists_when_zero_in_span() {
        let row = row_for_value(0.0, -5.0, 15.0, 10);
        assert!(row.is_some());
    }
}
