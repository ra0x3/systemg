---
sidebar_position: 5
title: inspect
---

## Overview

The `inspect` command provides detailed information about a specific service or cron unit. It displays runtime metrics, execution history, and resource usage statistics. The command supports both real-time monitoring with live charts and historical analysis with configurable time windows.

## Usage

### Basic Inspection

Inspect a service or cron unit by name or hash:

```sh
# Inspect by service name
$ sysg inspect myservice

# Inspect by hash (useful for cron units)
$ sysg inspect 3abad7ffa39c
```

### Output Formats

#### Chart View (Default)

By default, `inspect` displays an ASCII chart showing CPU and memory usage over time:

```sh
$ sysg inspect myservice
```

The chart includes:
- **Dual Y-axes**: CPU percentage on the left, memory (RSS) in GB on the right
- **Color-coded data points**: Green asterisks (*) for CPU, yellow dots (•) for RSS
- **Time labels**: Shows the time range at the bottom with timezone
- **Legend**: Visual key in the top-right corner

#### Table View

For a traditional tabular display of metrics:

```sh
$ sysg inspect myservice --table
```

This shows a table with columns for:
- Timestamp
- CPU percentage
- RSS memory usage

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

### Time Window Control

#### Filter by Time

View only recent metrics using the `--since` parameter:

```sh
# Show metrics from the last 6 hours (21600 seconds)
$ sysg inspect myservice --since 21600

# Show metrics from the last 24 hours
$ sysg inspect myservice --since 86400

# Default is 12 hours (43200 seconds)
$ sysg inspect myservice
```

#### Limit Sample Count

Control the maximum number of metric samples displayed:

```sh
# Show only the last 100 samples
$ sysg inspect myservice --samples 100

# Default is 720 samples (about 12 minutes at 1 sample/sec)
$ sysg inspect myservice
```

### Live Monitoring

#### Live Tail Mode

Enable real-time monitoring that continuously updates the display:

```sh
# Start live tailing with default 5-second window
$ sysg inspect myservice --tail

# Live tail with custom time window (10 seconds)
$ sysg inspect myservice --tail --tail-window 10

# Maximum window is 60 seconds
$ sysg inspect myservice --tail --tail-window 60
```

In live tail mode:
- **Auto-refresh**: Chart updates every second
- **In-place updates**: Terminal clears and redraws for smooth animation
- **Time labels include seconds**: Shows precise timestamps (e.g., "4:19:45PM EST")
- **Status indicator**: Bottom of screen shows "Live tail mode (Ns window) - Press Ctrl+C to stop"
- **Graceful exit**: Ctrl+C cleanly exits live mode

> **Note**: Live tail mode is disabled when using `--json` output format.

### Display Options

#### Disable Colors

For terminals that don't support ANSI colors or for cleaner output:

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
- **Execution History**: Last 10 runs with:
  - Start and end times
  - Exit codes
  - Status (success/failure)
- **Overlap Detection**: Shows if executions were skipped due to overlap

### Chart Visualization

The ASCII chart provides an intuitive view of resource usage:

1. **Y-Axis Scaling**:
   - **CPU**: Scales from 0% at bottom to max observed value at top
   - **Memory**: Scales from 0GB at bottom to max observed value + margin at top
   - Automatic margin added to ensure values don't touch axis limits

2. **Data Point Plotting**:
   - CPU values grow from bottom up (higher CPU = higher on chart)
   - Memory values also grow from bottom up
   - When CPU and memory overlap, a combined symbol (✦) is shown

3. **Time Progression**:
   - Chart reads left to right (oldest to newest)
   - Downsampling occurs if more samples than chart width
   - Time labels show start and end times with timezone

## Command Options

```
$ sysg inspect --help
Inspect a single service or cron unit in detail

Usage: sysg inspect [OPTIONS] <UNIT>

Arguments:
  <UNIT>  Name or hash of the unit to inspect

Options:
  -c, --config <CONFIG>           Path to the configuration file (defaults to `systemg.yaml`) [default: systemg.yaml]
      --json                      Emit machine-readable JSON output instead of a report
      --no-color                  Disable ANSI colors in output
      --since <SECONDS>           Only include samples captured in the last N seconds (default: 43200 = 12 hours) [default: 43200]
      --samples <COUNT>           Maximum number of metric samples to display (default: 720) [default: 720]
      --table                     Display metrics in table format instead of chart visualization
      --tail                      Enable live tailing mode to show real-time updates
      --tail-window <SECONDS>     Time window for tail mode in seconds (default: 5, max: 60) [default: 5]
      --log-level <LEVEL>         Override the logging verbosity for this invocation only
  -h, --help                      Print help
```

## Example Output

### Service Inspection

```
Unit: webserver (hash: 62768842fd60dbc9, type: service)
Status: Running, PID: 45231, Health: healthy
Uptime: 2 days, 14:32:15
Metrics: latest 15.2% CPU, avg 12.3% CPU, max 45.1% CPU, RSS 256MB across 43200 samples

Resource Usage Over Time

  45.1% ┤                    ┌─────────────────┐                                        ┤ 1.55GB
        │                    │ * CPU %         │           *                            │
        │                    │ • RSS GB        │          * *                          │
  22.5% ┤                    └─────────────────┘         *   *                          ┤ 0.78GB
        │                                              *      *   *                     │
        │                              •••••         **        * * **                   │
   0.0% ┤**************••••••••••••••••     •••••••**           *    ******************┤ 0.00GB
        └────────────────────────────────────────────────────────────────────────────┘
         7:00AM EST                                                         7:12PM EST
```

### Cron Unit Inspection

```
Unit: backup-job (hash: 3abad7ffa39c, type: cron)
Status: Scheduled, Health: healthy
Schedule: 0 0 * * * * (At second 0 of every minute)
Next execution: in 42 seconds
Execution history (last 10):
  - 2026-01-06 13:42:00: Ok (exit 0) after 2s
  - 2026-01-06 13:41:00: Ok (exit 0) after 3s
  - 2026-01-06 13:40:00: OkWithErr (exit 1) after 1s
  - 2026-01-06 13:39:00: Ok (exit 0) after 2s
```

## Live Tail Mode Example

```
$ sysg inspect webserver --tail --tail-window 10

[Chart updates in place every second]

Live - 10s window

  25.3% ┤                    ┌─────────────────┐                                        ┤ 0.52GB
        │                    │ * CPU %         │                      *                 │
        │                    │ • RSS GB        │                    **                  │
  12.6% ┤                    └─────────────────┘                  **  *                 ┤ 0.26GB
        │                                                       **     *                │
        │                                         •••••••••••••         *               │
   0.0% ┤                             ••••••••••••             •         ***************┤ 0.00GB
        └────────────────────────────────────────────────────────────────────────────┘
         4:19:45PM EST                                                    4:19:55PM EST

Live tail mode (10s window) - Press Ctrl+C to stop
```

## Notes

- **Metrics Persistence**: Historical metrics are preserved across supervisor restarts
- **Database Growth**: Metrics databases grow over time; consider periodic cleanup for long-running services
- **Sampling Rate**: Fixed at 1 sample per second; not configurable in current version
- **Memory Calculation**: RSS (Resident Set Size) includes shared libraries and may appear higher than expected
- **Chart Resolution**: Limited to 80 characters width for consistent terminal display