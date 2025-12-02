---
sidebar_position: 4
title: logs
---

## Overview

The `logs` command displays log output from running services. It reads log files that are automatically created when services are started, showing both standard output and standard error streams. The command supports viewing logs for a specific service or all services, and can follow logs in real-time.

## Usage

### View All Service Logs

View the last 50 lines of logs for all services:

```sh
$ sysg logs
```

### View Specific Service Logs

View logs for a specific service:

```sh
$ sysg logs api-service
```

### Custom Line Count

View a custom number of log lines:

```sh
$ sysg logs api-service --lines 100
```

### Filter by Log Kind

View only specific types of logs using the `--kind` or `-k` flag:

```sh
# View only stdout logs
$ sysg logs api-service --kind stdout

# View only stderr logs
$ sysg logs api-service --kind stderr

# View only supervisor logs (systemg's own operational logs)
$ sysg logs --kind supervisor
```

When no `--kind` flag is provided, all logs are displayed in the following order:
1. Supervisor logs (if no service is specified)
2. Service stdout logs
3. Service stderr logs

## How It Works

### Log File Location

Systemg stores log files in a standardized location:

- **Directory**: `~/.local/share/systemg/logs/`
- **Naming Pattern**: Service name followed by `_stdout.log` or `_stderr.log`
- **Example**: For a service named `api-service`:
  - Standard output: `~/.local/share/systemg/logs/api-service_stdout.log`
  - Standard error: `~/.local/share/systemg/logs/api-service_stderr.log`

### Log File Creation

When a service is started:

1. **Pipe Creation**: 
   - The service's stdout and stderr are captured via pipes
   - These pipes are created before the process is spawned

2. **Log Writer Threads**: 
   - Separate threads are spawned for stdout and stderr
   - Each thread reads from its pipe and writes to the corresponding log file
   - Logs are written asynchronously to avoid blocking the service

3. **Directory Creation**: 
   - The log directory is created if it doesn't exist
   - Uses `fs::create_dir_all()` to create parent directories as needed

4. **File Handling**: 
   - Log files are opened in append mode
   - New log entries are appended to existing files
   - Each line is written with a newline character

### PID File Loading

1. **PID File Access**: 
   - Loads the PID file from `~/.local/share/systemg/pid.json`
   - If the file doesn't exist, creates an empty PID file

2. **Service Lookup**: 
   - For a specific service: looks up the service name and retrieves its PID
   - For all services: iterates over all entries in the PID file

### Log Display Process

#### Single Service Logs

When a service name is provided:

1. **PID Verification**: 
   - Retrieves the service's PID from the PID file
   - If the service is not found, displays a warning and exits

2. **Header Display**: 
   - Prints a formatted header showing the service name and PID:
     ```
     +---------------------------------+
     |         arb-rs (138246)         |
     +---------------------------------+
     ```

3. **Platform-Specific Log Reading**:
   - **Linux**: 
     - Checks if `/proc/` followed by the process ID `/fd/1` and `/fd/2` exist (stdout/stderr file descriptors)
     - If they don't exist, returns an error (process may have exited)
     - Uses `tail -n` followed by the line count `-f` followed by the stdout and stderr log file paths to follow both log files
   - **macOS**: 
     - Directly uses log files (no `/proc` filesystem)
     - Uses `tail -n` followed by the line count `-f` followed by the stdout and stderr log file paths to follow both log files

4. **Real-Time Following**: 
   - The `tail -f` command follows the log files in real-time
   - New log entries are displayed as they're written
   - The command blocks until interrupted (Ctrl+C)

#### All Services Logs

When no service name is provided:

1. **Service Iteration**: 
   - Iterates over all services in the PID file
   - For each service, retrieves its PID

2. **Per-Service Display**: 
   - Displays the same header and log output for each service
   - Logs from multiple services are interleaved as they're written

3. **Empty State**: 
   - If no services are running, displays "No active services"

### Log File Format

Log files contain:

- **Line-by-Line Output**: Each line from stdout/stderr is written as a separate line
- **No Timestamps**: Systemg doesn't add timestamps; services should include their own if needed
- **Raw Output**: Logs contain exactly what the service writes to stdout/stderr
- **Append-Only**: Logs are never truncated; they grow over time

### Platform Differences

#### Linux

- Can check process file descriptors via `/proc/` followed by the process ID `/fd/`
- More efficient process verification
- Log files are always available even if the process has exited (if file descriptors are still open)

#### macOS

- No `/proc` filesystem
- Relies solely on log files
- Log files may not exist if the service was started before log capture was implemented

## Error Handling

### Service Not Found

If a specified service is not in the PID file:
- Displays: "Service" followed by the service name in quotes "is not currently running"
- Exits with a warning

### Log Files Not Found

If log files don't exist:
- `tail` command will fail
- An error may be displayed depending on the platform
- This can happen if:
  - The service was started before log capture was implemented
  - The log directory was deleted
  - The service hasn't written any output yet

### Process File Descriptors Unavailable (Linux)

If `/proc/` followed by the process ID `/fd/1` or `/fd/2` don't exist:
- Returns `LogsManagerError::LogUnavailable` with the process ID
- Indicates the process may have exited or file descriptors were closed

## Command Options

```
$ sysg logs --help
Show logs for a specific service

Usage: sysg logs [OPTIONS] [SERVICE]

Arguments:
  [SERVICE]  The name of the service whose logs should be displayed (optional)

Options:
  -l, --lines <LINES>      Number of lines to show (default: 50) [default: 50]
  -k, --kind <KIND>        Kind of logs to show: stdout, stderr, or supervisor (default: all)
      --log-level <LEVEL>  Override the logging verbosity for this invocation only
  -h, --help               Print help
```

## Example Output

```sh
$ sysg logs

+---------------------------------+
|         arb-rs (138246)         |
+---------------------------------+

==> /home/ubuntu/.local/share/systemg/logs/arb-rs_stdout.log <==
2025-11-06T15:21:42.341828Z DEBUG request{method=GET uri=/api/v1/stadiums?cache=true&league=nfl version=HTTP/1.1}: hyper_util::client::legacy::connect::http: connecting to 146.75.78.132:443
2025-11-06T15:21:42.343746Z DEBUG request{method=GET uri=/api/v1/scores?cache=true&date=2025-11-06&league=nfl version=HTTP/1.1}: hyper_util::client::legacy::connect::http: connecting to 146.75.78.132:443
2025-11-06T15:21:42.344003Z  INFO request{method=GET uri=/api/v1/team-profile?cache=true&league=nfl version=HTTP/1.1}: arb_rs::uses::sportradar: Making real API request for NFL team profile: https://api.sportsdata.io/v3/nfl/scores/json/AllTeams
```

## Notes

- **Real-Time Following**: The command follows logs in real-time using `tail -f`. Press Ctrl+C to stop.
- **Log Rotation**: Systemg doesn't perform log rotation. Consider using external tools like `logrotate` to manage log file sizes.
- **Performance**: Reading logs for many services simultaneously may impact performance due to multiple `tail -f` processes.
- **Log Persistence**: Logs persist even after services are stopped, allowing you to review historical output.
