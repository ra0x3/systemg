---
sidebar_position: 5
title: status
---

## Overview

The `status` command displays detailed information about running services, including process details, resource usage, and process trees. It reads service information from the PID file and queries the operating system for real-time process data.

## Usage

### Show All Services

Show the status of all currently running services (uses default `systemg.yaml` config):

```sh
$ sysg status
```

Show status with a specific configuration file:

```sh
$ sysg status --config myapp.yaml
```

### Show Specific Service

Show the status of a specific service:

```sh
$ sysg status --service arb-rs
```

With a custom configuration file:

```sh
$ sysg status --config myapp.yaml --service arb-rs
```

## How It Works

### PID File Loading

1. **PID File Access**: 
   - Loads the PID file from `~/.local/share/systemg/pid.json`
   - If the file doesn't exist, creates an empty PID file
   - The PID file maps service names to their process IDs

2. **Service Lookup**: 
   - For a specific service: looks up the service name in the PID file
   - For all services: iterates over all entries in the PID file

### Process Verification

For each service:

1. **PID Retrieval**: Gets the process ID from the PID file
2. **Process Existence Check**: 
   - **Linux**: Checks if `/proc/` followed by the process ID exists
   - **macOS**: Runs `ps -p` followed by the process ID to verify the process is running
3. **Not Running Handling**: 
   - If the process doesn't exist, displays "Not running" or "Process" followed by the PID "not found"
   - Continues to the next service (for `status` without `--service`)

### Status Information Collection

For running services, systemg collects the following information:

#### Process Uptime

- **Linux**: Reads the process start time from `/proc/` followed by the process ID metadata and formats it as "Day YYYY-MM-DD HH:MM:SS UTC"
- **macOS**: Runs `ps -p` followed by the process ID `-o etime=` to get elapsed time in "DD-HH:MM:SS" or "HH:MM:SS" format
- **Human-Readable Format**: Converts elapsed time to:
  - "X secs ago" (0-59 seconds)
  - "X mins ago" (1-59 minutes)
  - "X hours ago" (1-23 hours)
  - "X days ago" (1-6 days)
  - "X weeks ago" (7+ days)

#### Task Count (Threads)

- Runs `ps -p` followed by the process ID `-o thcount=` to get the number of threads
- Displays as "Tasks: X (limit: N/A)"

#### Memory Usage

- Runs `ps -p` followed by the process ID `-o rss=` to get resident set size in kilobytes
- Converts to megabytes (KB / 1024)
- Displays as "Memory: X.XM"

#### CPU Time

- Runs `ps -p` followed by the process ID `-o time=` to get CPU time in "MM:SS" format
- Parses and converts to total seconds
- Displays as "CPU: X.XXXs"

#### Process Group ID

- Uses `getpgid()` to get the process group ID
- Displays as "Process Group:" followed by the process group ID
- This is the same ID used for signaling the service and its children

#### Command Line

- Runs `ps -p` followed by the process ID `-o command=` to get the full command line
- Displays the command that was executed to start the service

#### Process Tree

- Uses the `sysinfo` crate to build a process tree
- Recursively finds all child processes of the main service process
- Displays children with indentation to show hierarchy:
  - Main process: `` `|-` `` followed by the process ID and command
  - Direct children: `` `├─` `` followed by the process ID and command
  - Grandchildren: Further indented with `` `├─` `` prefix
- Shows the complete process tree for the service

### Output Format

#### Single Service Status

When `--service` is specified, displays detailed information:

```
● - arb-rs Running
   Active: active (running) since Tue 2025-11-04 11:30:52 UTC; 2 hours ago
 Main PID: 138246
    Tasks: 1 (limit: N/A)
   Memory: 1.6M
      CPU: 0.000s
 Process Group: 138246
     |-138246 sh -c ./target/release/arb-rs -c config.toml
       ├─138253 ./target/release/arb-rs -c config.toml
           ├─138254
           ├─138255
```

#### All Services Status

When no `--service` is specified:

1. **Header**: Displays "Active services:" if services are found
2. **Service List**: Iterates through all services in the PID file
3. **Per-Service Details**: Shows the same detailed information for each service
4. **Empty State**: If no services are running, displays "No active services."

### Platform Differences

#### Linux

- Uses `/proc` filesystem for process information
- More efficient process existence checks
- Process start time from file metadata

#### macOS

- Uses `ps` command for all process queries
- Slightly slower due to process spawning
- Elapsed time format differs from Linux

## Error Handling

### Missing PID File

If the PID file doesn't exist:
- Creates an empty PID file
- Displays "No active services." for `status` without `--service`
- Displays "●" followed by the service name "- Not running" for a specific service

### Process Not Found

If a PID exists in the file but the process is not running:
- Displays "●" followed by the service name "- Process" followed by the PID "not found"
- The PID file entry is not automatically cleaned up (this happens during stop/restart)

### Permission Errors

If systemg lacks permission to query a process:
- May fail to retrieve some information
- Continues with available information

## Command Options

```
$ sysg status --help
Show the status of currently running services

Usage: sysg status [OPTIONS]

Options:
  -c, --config <CONFIG>      Path to the configuration file (defaults to `systemg.yaml`)
      --log-level <LEVEL>    Override the logging verbosity for this invocation only
  -s, --service <SERVICE>    Optionally specify a service name to check its status
      --all                  Show all services including orphaned state (services not in current config)
  -h, --help                 Print help
```

## Example Output

```sh
$ sysg status -s arb-rs

Active services:
● - arb-rs Running
   Active: active (running) since Tue 2025-11-04 11:30:52 UTC; 2 hours ago
 Main PID: 138246
    Tasks: 1 (limit: N/A)
   Memory: 1.6M
      CPU: 0.000s
 Process Group: 138246
     |-138246 sh -c ./target/release/arb-rs -c config.toml
       ├─138253 ./target/release/arb-rs -c config.toml
           ├─138254
           ├─138255
```
