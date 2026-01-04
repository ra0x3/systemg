---
sidebar_position: 5
title: State
---

# State Management

Systemg maintains its operational state in the `~/.local/share/systemg/` directory. This directory contains various files that track service PIDs, configuration hints, locks, and logs. Understanding these files helps with debugging, monitoring, and manual intervention when necessary.

## State Directory Structure

```
~/.local/share/systemg/
├── sysg.pid              # Supervisor process ID
├── control.sock          # Unix socket for IPC
├── config_hint           # Last used configuration path
├── pid.json              # Service PID registry
├── pid.json.lock         # Lock file for PID registry updates
├── state.json            # Service state tracking
└── logs/
    ├── supervisor.log     # Supervisor operational logs
    ├── service1_stdout.log
    ├── service1_stderr.log
    ├── service2_stdout.log
    └── service2_stderr.log
```

When systemg runs with elevated privileges (`sysg --sys`), the entire tree is relocated under `/var/lib/systemg`, and supervisor logs are written to `/var/log/systemg` instead of the per-user directory.

## State Files

### `sysg.pid`

**Purpose**: Stores the process ID of the running systemg supervisor.

**Format**: Plain text file containing a single integer (the PID).

**Example**:
```
67234
```

**Usage**:
- Created when the supervisor starts
- Used by CLI commands to detect if a supervisor is running
- Removed when the supervisor shuts down cleanly
- If this file exists but the process is not running, it indicates an unclean shutdown

### `control.sock`

**Purpose**: Unix domain socket used for inter-process communication between the CLI and supervisor.

**Format**: Socket file (not human-readable).

**Usage**:
- Created when the supervisor starts
- CLI commands connect to this socket to send control messages
- Removed when the supervisor shuts down
- If this file exists but the supervisor isn't running, CLI commands may fail with connection errors

### `config_hint`

**Purpose**: Stores the path to the configuration file used by the currently running supervisor.

**Format**: Plain text file containing an absolute path.

**Example**:
```
/Users/username/project/systemg.yaml
```

**Usage**:
- Written when the supervisor starts
- Allows CLI commands to operate without specifying `--config` when the supervisor is running
- Used as a fallback when the supervisor isn't reachable but state cleanup is needed

### `pid.json`

**Purpose**: Registry of all service process IDs managed by the supervisor.

**Format**: JSON file mapping service names to PIDs.

**Example**:
```json
{
  "services": {
    "web_server": 67235,
    "database": 67236,
    "worker": 67237
  }
}
```

**Usage**:
- Updated when services start or stop
- Used by the supervisor to track running processes
- Essential for proper service lifecycle management
- Persists across supervisor restarts to maintain service continuity

### `pid.json.lock`

**Purpose**: Ensures atomic updates to the PID registry.

**Format**: Lock file (typically empty or contains lock metadata).

**Usage**:
- Created when updating `pid.json`
- Prevents race conditions during concurrent PID registry updates
- Automatically released after updates complete
- If this file persists, it may indicate a crashed update operation

### `state.json`

**Purpose**: Tracks additional service state beyond just PIDs.

**Format**: JSON file containing service metadata.

**Example**:
```json
{
  "services": {
    "web_server": {
      "status": "running",
      "started_at": "2025-12-31T15:30:00Z",
      "restart_count": 0,
      "exit_code": null
    },
    "database": {
      "status": "running",
      "started_at": "2025-12-31T15:30:01Z",
      "restart_count": 2,
      "exit_code": null
    }
  }
}
```

**Usage**:
- Maintains service status information
- Tracks restart attempts for services with restart policies
- Records exit codes for debugging
- Persists across supervisor restarts

### `logs/supervisor.log`

**Purpose**: Contains operational logs from the supervisor itself.

**Format**: Plain text log file with timestamped entries.

**Example**:
```
2025-12-31T15:30:00.123Z INFO  systemg supervisor starting
2025-12-31T15:30:00.234Z INFO  loading configuration from /path/to/systemg.yaml
2025-12-31T15:30:00.345Z INFO  starting service: web_server
2025-12-31T15:30:00.456Z INFO  starting service: database
2025-12-31T15:30:00.567Z INFO  systemg supervisor listening on control.sock
```

**Location**:
- Userspace mode: `~/.local/share/systemg/logs/supervisor.log`
- System mode (`sysg --sys`): `/var/log/systemg/supervisor.log`

**Usage**:
- Records supervisor lifecycle events
- Logs service start/stop operations
- Captures errors and warnings
- Useful for debugging supervisor issues

### `logs/` Directory

**Purpose**: Contains stdout and stderr logs for each service.

**Format**: Plain text files named `{service_name}_{stdout|stderr}.log`.

**Example Files**:
- `web_server_stdout.log` - Standard output from web_server service
- `web_server_stderr.log` - Standard error from web_server service
- `database_stdout.log` - Standard output from database service
- `database_stderr.log` - Standard error from database service

**Usage**:
- Created when services start
- Continuously appended while services run
- Preserved across service restarts
- Viewable via `sysg logs <service_name>` command
- Cleared by `sysg purge` command

## State Persistence

### Across Supervisor Restarts

The following state persists when the supervisor is restarted:
- Service PID registry (`pid.json`)
- Service state information (`state.json`)
- Service logs (`logs/` directory)
- Supervisor logs (`logs/supervisor.log`)

This persistence ensures that:
- Running services continue without interruption
- Service history is maintained
- Logs are not lost

### Clean Shutdown

During a clean shutdown (`sysg stop`), the supervisor:
1. Stops all running services gracefully
2. Updates `pid.json` to remove service PIDs
3. Updates `state.json` with final service states
4. Removes `sysg.pid` file
5. Removes `control.sock` socket
6. Preserves logs and configuration hint

### Crash Recovery

If the supervisor crashes unexpectedly:
- `sysg.pid` and `control.sock` may remain as stale files
- The next supervisor start will detect and clean up stale files
- Services listed in `pid.json` may still be running as orphans
- Use `sysg purge` to perform a complete state cleanup

## Managing State

### Viewing State

Check the current state files:
```bash
$ ls -la ~/.local/share/systemg/
```

View the PID registry:
```bash
$ cat ~/.local/share/systemg/pid.json | jq
```

Check supervisor PID:
```bash
$ cat ~/.local/share/systemg/sysg.pid
```

### Cleaning State

Remove all state (stops supervisor if running):
```bash
$ sysg purge
```

Manually clean stale state (use with caution):
```bash
$ rm -rf ~/.local/share/systemg/
```

### Debugging State Issues

Common state-related issues and solutions:

**"Supervisor already running" but no supervisor active**:
- Stale `sysg.pid` file exists
- Solution: Remove the file or run `sysg purge`

**"Cannot connect to supervisor" errors**:
- Stale `control.sock` exists without running supervisor
- Solution: Remove the socket file or run `sysg purge`

**Services shown as running but processes don't exist**:
- Stale entries in `pid.json`
- Solution: Run `sysg purge` to clean state

**Lock file persists (`pid.json.lock`)**:
- Previous operation crashed during PID update
- Solution: Remove the lock file manually if no operations are running

## Best Practices

1. **Regular Monitoring**: Check state files when debugging issues
2. **Clean Shutdowns**: Always use `sysg stop` for graceful shutdown
3. **State Cleanup**: Use `sysg purge` after crashes or issues
4. **Log Rotation**: Implement external log rotation for long-running services
5. **Backup State**: Consider backing up state files before major changes
6. **Permission Management**: Ensure proper permissions on state directory

## Security Considerations

- State files contain sensitive operational information
- The state directory is created with user-only permissions (700)
- Socket files are created with restrictive permissions
- Never share state files publicly as they may contain PIDs and paths
- Consider encrypting backups of state directories
