---
sidebar_position: 5
title: inspect
---

## Overview

The `inspect` command provides deep visibility into individual programs within your composed system. It displays runtime metrics, execution history, and resource usage patterns that help you understand how each program contributes to the overall system behavior. Each invocation renders a single snapshot, making it easy to track how your composed system evolves over time.

## Usage

### Basic Inspection

Inspect a program within your composed system by name or hash:

```sh
# Inspect by service name
$ sysg inspect myservice

# Inspect by hash (useful for cron units)
$ sysg inspect 3abad7ffa39c
```

### Output

#### Chart View (Default)

By default, `inspect` displays an ASCII chart showing CPU and memory usage over time:

```sh
$ sysg inspect myservice
```

The chart includes:
- **Combined plot**: CPU (`x`) and memory (`o`) samples are drawn on the same grid, with `*` indicating overlap.
- **Dual Y-axes**: CPU percentage on the left, memory (RSS) in GB on the right.
- **Time labels**: The bottom axis shows the oldest and newest timestamps rendered.
- **Legend**: A legend beneath the graph recaps marker meanings and axis units.

#### JSON Output

For programmatic access or integration with other tools:

```sh
$ sysg inspect myservice --json
```

Returns a structured JSON object with:
- Unit details (name, hash, type)
- Current status and health
- Metrics samples array
- Execution history (for cron units)

### Window Selection

Choose how much history to render with the `--window` flag. Durations mirror the suffixes accepted elsewhere in Systemg (`s`, `m`, `h`, `d`, `w`).

```sh
# Inspect just the last 30 seconds
$ sysg inspect myservice --window 30s

# Inspect the last 2 hours
$ sysg inspect myservice --window 2h
```

Values up to 60 seconds are rendered as the same single snapshot—`inspect` never enters a watch loop—so you can repeatedly invoke the command to capture consecutive states if needed.

### Display Options

Disable ANSI colors when working in limited terminals or copying output into logs:

```sh
$ sysg inspect myservice --no-color
```

## How It Works

### Metrics Collection

Systemg collects metrics through several mechanisms:

1. **Sampling Interval**:
   - Metrics are sampled every second while services are running
   - Each sample captures CPU percentage and RSS memory usage

2. **Data Storage**:
   - Metrics are stored in a SQLite database per service
   - Database location: `~/.local/share/systemg/metrics/<hash>.db`
   - Data persists across restarts for historical analysis

3. **IPC Communication**:
   - The `inspect` command sends a request to the running supervisor
   - Supervisor queries the metrics database and returns the data
   - If no supervisor is running, falls back to reading state files

### Service Information

For regular services, the inspect output includes:

- **Identity**: Service name and hash
- **Status**: Current lifecycle state (Running, Stopped, etc.)
- **Health**: Overall health assessment (healthy, degraded, failing)
- **Process Info**: PID, uptime, last exit code
- **Resource Usage**: Current and historical CPU/memory metrics

### Cron Unit Information

For cron units, additional information is displayed:

- **Schedule**: Cron expression and timezone
- **Next Execution**: When the job will run next
- **Execution History**: Last 10 runs with start/end times, exit codes, and status
- **Overlap Detection**: Shows if executions were skipped due to overlap

## Chart Visualization

The ASCII chart provides an intuitive view of resource usage:

1. **Y-Axis Scaling**:
   - **CPU**: Scales from the observed minimum (never above 0) to the observed maximum
   - **Memory**: Scales from 0GB to the observed maximum plus a small margin

2. **Data Point Plotting**:
   - CPU values grow from bottom up (higher CPU = higher on chart)
   - Memory values follow the same orientation on the shared grid
   - When CPU and memory overlap, a combined `*` marker is shown

3. **Time Progression**:
   - Chart reads left to right (oldest to newest)
   - Downsampling occurs if more samples than chart width
   - Time labels show start and end times with timezone

4. **Legend and Axes**:
   - Legend under the chart summarizes symbol usage
   - Left axis displays CPU percentages; right axis shows RSS in gigabytes

## Command Options

```
$ sysg inspect --help
Inspect a single service or cron unit in detail

Usage: sysg inspect [OPTIONS] <UNIT>

Arguments:
  <UNIT>  Name or hash of the unit to inspect

Options:
  -c, --config <CONFIG>    Path to the configuration file (defaults to `systemg.yaml`) [default: systemg.yaml]
      --json               Emit machine-readable JSON output instead of a report
      --no-color           Disable ANSI colors in output
      --window <WINDOW>    Time window to display (e.g., "5s" or "12h") [default: 5s]
      --log-level <LEVEL>  Override the logging verbosity for this invocation only
  -h, --help               Print help
```

## Notes

- **Metrics Persistence**: Historical metrics are preserved across supervisor restarts
- **Database Growth**: Metrics databases grow over time; consider periodic cleanup for long-running services
- **Sampling Rate**: Fixed at 1 sample per second; not configurable in current version
- **Memory Calculation**: RSS (Resident Set Size) includes shared libraries and may appear higher than expected
- **Chart Resolution**: Limited to 80 columns for consistent terminal display
