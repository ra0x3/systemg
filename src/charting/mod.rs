//! ASCII charting for metrics visualization using our custom rasciigraph.

use std::{env, error::Error};

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
    /// Optional max render width for chart output.
    pub max_width: Option<usize>,
}

/// Render metrics as ASCII charts using rasciigraph.
pub fn render_metrics_chart(
    samples: &[MetricSample],
    config: &ChartConfig,
) -> Result<(), Box<dyn Error>> {
    for line in render_metrics_chart_lines(samples, config)? {
        println!("{line}");
    }
    Ok(())
}

/// Render metrics charts into lines for embedding in higher-level layouts.
pub fn render_metrics_chart_lines(
    samples: &[MetricSample],
    config: &ChartConfig,
) -> Result<Vec<String>, Box<dyn Error>> {
    if samples.is_empty() {
        return Ok(vec![
            "No data available for the specified time window.".to_string(),
        ]);
    }

    let cpu_values: Vec<f64> = samples.iter().map(|s| s.cpu_percent as f64).collect();
    let mem_gb_values: Vec<f64> = samples
        .iter()
        .map(|s| s.rss_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        .collect();

    let chart_width = 48usize;
    let cpu_resampled = resample_to_width(&cpu_values, chart_width);
    let mem_resampled = resample_to_width(&mem_gb_values, chart_width);

    let cpu_resampled: Vec<f64> = cpu_resampled
        .iter()
        .map(|&v| if v.is_finite() { v } else { 0.0 })
        .collect();
    let mem_resampled: Vec<f64> = mem_resampled
        .iter()
        .map(|&v| if v.is_finite() { v } else { 0.0 })
        .collect();

    let cpu_final = cpu_resampled;
    let mem_final = mem_resampled;

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

    let mut output = vec![String::new()];

    let x_axis_label = format!(
        "X-axis: Time ({first_time} -> {last_time}) | Window: {}",
        config.window_desc
    );

    let available_columns = effective_chart_columns(config);
    let chart_width = compute_chart_width(available_columns);

    let cpu_graph = plot(
        cpu_final,
        Config::default()
            .with_width(chart_width as u32)
            .with_height(10)
            .with_y_precision(4),
    );
    let cpu_card = build_chart_card(
        ChartCardSpec {
            title: "CPU Usage".to_string(),
            legend: "Legend: CPU (%)".to_string(),
            y_axis_label: "Y-axis: CPU (%)".to_string(),
        },
        &cpu_graph,
        config.no_color,
        Some("\x1b[36m"),
    );

    let mem_max_for_percent = mem_final
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max)
        .max(0.0);
    let mem_graph = plot(
        mem_final,
        Config::default()
            .with_width(chart_width as u32)
            .with_height(10)
            .with_y_precision(4),
    );
    let mem_dual_axis =
        add_right_axis_percentage_labels(&mem_graph, mem_max_for_percent, "(%)", true);
    let mem_card = build_chart_card(
        ChartCardSpec {
            title: "Memory Usage (Dual Y-axis)".to_string(),
            legend: "Legend: RSS (GB); right axis shows normalized percent".to_string(),
            y_axis_label: "Y-axis Left: RSS (GB) | Right: RSS (%)".to_string(),
        },
        &mem_dual_axis,
        config.no_color,
        Some("\x1b[35m"),
    );

    let cards = vec![cpu_card, mem_card];
    let rendered_rows = layout_cards_with_wrapping(&cards, available_columns);
    for (idx, row) in rendered_rows.iter().enumerate() {
        for line in row {
            output.push(line.clone());
        }
        output.push(String::new());
        output.push(x_axis_label.clone());
        if idx + 1 < rendered_rows.len() {
            output.push(String::new());
        }
    }

    output.push(String::new());
    output.push("Summary Statistics:".to_string());

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

    output.push(format!(
        "  CPU:     min={:.1}% avg={:.1}% max={:.1}%",
        if cpu_min.is_finite() { cpu_min } else { 0.0 },
        cpu_avg,
        if cpu_max.is_finite() { cpu_max } else { 0.0 }
    ));
    output.push(format!(
        "  Memory:  min={:.4}GB avg={:.4}GB max={:.4}GB",
        if mem_min.is_finite() { mem_min } else { 0.0 },
        mem_avg,
        if mem_max.is_finite() { mem_max } else { 0.0 }
    ));
    output.push(format!("  Samples: {}", samples.len()));

    Ok(output)
}

struct ChartCardSpec {
    title: String,
    legend: String,
    y_axis_label: String,
}

fn build_chart_card(
    spec: ChartCardSpec,
    graph: &str,
    no_color: bool,
    graph_color: Option<&str>,
) -> Vec<String> {
    let mut lines = vec![spec.title, spec.legend, spec.y_axis_label, String::new()];

    for line in graph.lines() {
        let colored = if no_color {
            line.to_string()
        } else if let Some(color) = graph_color {
            format!("{color}{line}\x1b[0m")
        } else {
            line.to_string()
        };
        lines.push(colored);
    }

    lines
}

fn add_right_axis_percentage_labels(
    graph: &str,
    raw_max: f64,
    suffix: &str,
    clamp_to_hundred: bool,
) -> String {
    let axis_rows = extract_axis_rows(graph);
    let scale_max = axis_rows
        .iter()
        .map(|(raw, _)| raw.abs())
        .fold(raw_max.max(0.0), f64::max)
        .max(0.0);
    let mut out = Vec::new();
    for line in graph.lines() {
        let axis_index = line.find('┤').or_else(|| line.find('┼'));
        if let Some(axis_index) = axis_index {
            let raw_label = line[..axis_index].trim();
            let percent_label = raw_label.parse::<f64>().ok().map_or(0.0, |raw| {
                if scale_max <= 0.0 {
                    0.0
                } else {
                    let pct = (raw / scale_max * 100.0).max(0.0);
                    if clamp_to_hundred {
                        pct.min(100.0)
                    } else {
                        pct
                    }
                }
            });
            out.push(format!(
                "{line}  {right:>7.2}{suffix}",
                right = percent_label
            ));
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n")
}

fn extract_axis_rows(graph: &str) -> Vec<(f64, usize)> {
    graph
        .lines()
        .filter_map(|line| {
            let axis_index = line.find('┤').or_else(|| line.find('┼'))?;
            let raw = line[..axis_index].trim().parse::<f64>().ok()?;
            Some((raw, axis_index))
        })
        .collect()
}

fn layout_cards_with_wrapping(
    cards: &[Vec<String>],
    max_width: usize,
) -> Vec<Vec<String>> {
    if cards.is_empty() {
        return Vec::new();
    }

    let mut output: Vec<Vec<String>> = Vec::new();
    let mut row_cards: Vec<&Vec<String>> = Vec::new();
    let mut row_width = 0usize;
    let gap = 4usize;

    for card in cards {
        let width = card_width(card);
        let projected_width = if row_cards.is_empty() {
            width
        } else {
            row_width + gap + width
        };
        if !row_cards.is_empty() && projected_width > max_width {
            output.push(render_card_row(&row_cards, gap));
            row_cards.clear();
            row_width = 0;
        }

        if row_cards.is_empty() {
            row_width = width;
        } else {
            row_width += gap + width;
        }
        row_cards.push(card);
    }

    if !row_cards.is_empty() {
        output.push(render_card_row(&row_cards, gap));
    }

    output
}

fn render_card_row(cards: &[&Vec<String>], gap: usize) -> Vec<String> {
    let row_height = cards.iter().map(|card| card.len()).max().unwrap_or(0);
    let card_widths: Vec<usize> = cards.iter().map(|card| card_width(card)).collect();
    let spacer = " ".repeat(gap);
    let mut row_lines = Vec::new();

    for line_idx in 0..row_height {
        let mut assembled = String::new();
        for (card_idx, card) in cards.iter().enumerate() {
            if card_idx > 0 {
                assembled.push_str(&spacer);
            }
            let line = card.get(line_idx).map(String::as_str).unwrap_or("");
            assembled.push_str(line);
            let visible_pad = card_widths[card_idx].saturating_sub(visible_width(line));
            if visible_pad > 0 {
                assembled.push_str(&" ".repeat(visible_pad));
            }
        }
        row_lines.push(assembled.trim_end().to_string());
    }

    row_lines
}

fn card_width(card: &[String]) -> usize {
    card.iter()
        .map(|line| visible_width(line))
        .max()
        .unwrap_or(0)
}

fn terminal_columns() -> usize {
    env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&width| width >= 60)
        .unwrap_or(120)
}

fn effective_chart_columns(config: &ChartConfig) -> usize {
    match config.max_width {
        Some(width) => width.max(1),
        None => terminal_columns(),
    }
}

fn compute_chart_width(columns: usize) -> usize {
    let min_width = 24usize;
    let preferred = 40usize;
    let gap = 4usize;
    let estimated_cpu_overhead = 10usize;
    let estimated_mem_overhead = 22usize;
    let total_overhead = estimated_cpu_overhead + estimated_mem_overhead + gap;

    if columns <= total_overhead + (min_width * 2) {
        return min_width;
    }

    let available_plot_space = columns - total_overhead;
    let per_chart = available_plot_space / 2;
    per_chart.clamp(min_width, preferred)
}

fn visible_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
            continue;
        }
        width += 1;
    }
    width
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

/// Parse a stream interval into seconds.
///
/// Accepts either unit-qualified durations like "1s" / "2m" or a bare number
/// of seconds like "5".
pub fn parse_stream_duration(input: &str) -> Result<u64, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Stream duration cannot be empty".to_string());
    }

    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        let seconds: u64 = trimmed
            .parse()
            .map_err(|_| format!("Invalid stream duration: {trimmed}"))?;
        if seconds == 0 {
            return Err("Stream duration must be positive".to_string());
        }
        return Ok(seconds);
    }

    parse_window_duration(trimmed)
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
    fn test_parse_stream_duration() {
        assert_eq!(parse_stream_duration("5").unwrap(), 5);
        assert_eq!(parse_stream_duration("1s").unwrap(), 1);
        assert_eq!(parse_stream_duration("2m").unwrap(), 120);
        assert_eq!(parse_stream_duration("1second").unwrap(), 1);

        assert!(parse_stream_duration("").is_err());
        assert!(parse_stream_duration("0").is_err());
        assert!(parse_stream_duration("invalid").is_err());
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

    #[test]
    fn test_add_right_axis_percentage_labels() {
        let graph = " 2.0000┤╭─\n 1.0000┤│ \n 0.0000┼─ ";
        let with_right = add_right_axis_percentage_labels(graph, 2.0, "(%)", true);
        assert!(with_right.contains("100.00(%)"));
        assert!(with_right.contains(" 50.00(%)"));
        assert!(with_right.contains("  0.00(%)"));
    }

    #[test]
    fn test_layout_cards_wraps_when_width_small() {
        let cards = vec![
            vec!["card1".to_string(), "line2".to_string()],
            vec!["card2".to_string(), "line2".to_string()],
            vec!["card3".to_string(), "line2".to_string()],
        ];
        let rendered = layout_cards_with_wrapping(&cards, 15);
        let joined = rendered
            .iter()
            .map(|row| row.join("\n"))
            .collect::<Vec<String>>()
            .join("\n\n");
        assert!(joined.contains("card1"));
        assert!(joined.contains("card2"));
        assert!(joined.contains("card3"));
        assert!(joined.contains("\n\n"));
    }

    #[test]
    fn test_compute_chart_width_prefers_inline_layout() {
        assert_eq!(compute_chart_width(120), 40);
        assert_eq!(compute_chart_width(80), 24);
    }

    #[test]
    fn test_effective_chart_columns_uses_max_width_override() {
        let config = ChartConfig {
            no_color: true,
            window_desc: "5m".to_string(),
            max_width: Some(90),
        };
        assert_eq!(effective_chart_columns(&config), 90);
    }
}
