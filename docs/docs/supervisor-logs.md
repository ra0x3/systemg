---
sidebar_position: 6
title: Supervisor Logs
---

# Supervisor Logs

Systemg maintains its own operational logs separate from service logs. These supervisor logs capture systemg's internal operations, including service lifecycle events, configuration changes, and error conditions.

## Overview

Supervisor logs contain:
- **Service lifecycle events** - When services start, stop, restart, or crash
- **Cron job execution** - When scheduled tasks run and their outcomes
- **Configuration changes** - When configuration is reloaded
- **System events** - Supervisor startup, shutdown, and error conditions
- **Debug information** - Detailed operational data when running with `--log-level debug`

## Log Location

The supervisor log file is stored at:

- **Userspace mode**: `~/.local/share/systemg/logs/supervisor.log`
- **System mode (`sysg --sys`)**: `/var/log/systemg/supervisor.log`

The log file is created automatically when systemg starts and is appended to for all subsequent operations.

## Viewing Supervisor Logs

### Using the logs command

View supervisor logs using the `--kind supervisor` flag:

```sh
# View last 50 lines of supervisor logs
$ sysg logs --kind supervisor

# View last 100 lines
$ sysg logs --kind supervisor --lines 100

# View all logs (supervisor + service logs)
$ sysg logs
```

### Using standard tools

You can also view supervisor logs directly:

```sh
# View the entire log file
$ cat ~/.local/share/systemg/logs/supervisor.log

# Follow logs in real-time
$ tail -f ~/.local/share/systemg/logs/supervisor.log

# View last 100 lines
$ tail -n 100 ~/.local/share/systemg/logs/supervisor.log

# Search for specific events
$ grep "Starting service" ~/.local/share/systemg/logs/supervisor.log
```

## Log Levels

Control the verbosity of supervisor logs using the `--log-level` flag when starting systemg:

```sh
# Info level (default) - Shows important operational events
$ sysg start --daemonize --log-level info

# Debug level - Shows detailed operational information
$ sysg start --daemonize --log-level debug

# Warn level - Shows only warnings and errors
$ sysg start --daemonize --log-level warn

# Error level - Shows only error conditions
$ sysg start --daemonize --log-level error
```

Available log levels (from most to least verbose):
- `trace` (5) - Extremely detailed tracing information
- `debug` (4) - Detailed debugging information
- `info` (3) - General operational information (default)
- `warn` (2) - Warning messages
- `error` (1) - Error conditions only
- `off` (0) - No logging

You can also use numeric values:
```sh
$ sysg start --daemonize --log-level 4  # Same as debug
```

## Common Log Messages

### Service Lifecycle

#### Service Starting
```
2025-12-02T10:30:15.123456Z  INFO systemg::daemon: Starting service: api-server
2025-12-02T10:30:15.125432Z DEBUG systemg::daemon: Starting service thread for api-server
```

#### Service Restart
```
2025-12-02T10:35:20.654321Z  INFO systemg::daemon: Performing immediate restart for service: api-server
2025-12-02T10:35:20.656789Z DEBUG systemg::daemon: Stopping service for restart: api-server
```

#### Service Stopped
```
2025-12-02T10:40:10.987654Z  INFO systemg::daemon: Stopping service: api-server
2025-12-02T10:40:11.123456Z DEBUG systemg::daemon: Service api-server stopped successfully
```

### Supervisor Operations

#### Supervisor Starting
```
2025-12-02T10:30:00.000000Z  INFO systemg::supervisor: systemg supervisor listening on "/Users/username/.local/share/systemg/supervisor.sock"
2025-12-02T10:30:00.000000Z  INFO systemg::supervisor: Registered cron job for service 'backup'
```

#### Supervisor Shutdown
```
2025-12-02T18:00:00.000000Z  INFO systemg::supervisor: Supervisor shutting down
2025-12-02T18:00:00.100000Z  INFO systemg::daemon: All services stopped
```

### Cron Jobs

#### Cron Job Execution
```
2025-12-02T12:00:00.000000Z  INFO systemg::supervisor: Running cron job 'backup'
2025-12-02T12:00:15.123456Z  INFO systemg::supervisor: Cron job 'backup' completed successfully
```

#### Cron Job Failure
```
2025-12-02T12:00:00.000000Z  INFO systemg::supervisor: Running cron job 'backup'
2025-12-02T12:00:15.123456Z  WARN systemg::supervisor: Cron job 'backup' exited with non-zero status
```

### Configuration

#### Config Reload
```
2025-12-02T11:00:00.000000Z  INFO systemg::supervisor: Reloading configuration from "/path/to/systemg.yaml"
2025-12-02T11:00:00.123456Z  INFO systemg::supervisor: All services restarted
```

#### Skip Flag
```
2025-12-02T10:30:15.123456Z  INFO systemg::supervisor: Skipping service 'optional-service' due to skip flag
2025-12-02T10:30:15.123456Z  INFO systemg::supervisor: Skipping service 'conditional-service' due to skip condition
```

## Log Format

Supervisor logs use a structured format:

```
[TIMESTAMP] [LEVEL] [MODULE]: [MESSAGE]
```

Components:
- **TIMESTAMP**: ISO 8601 format with microsecond precision
- **LEVEL**: Log level (INFO, DEBUG, WARN, ERROR, TRACE)
- **MODULE**: Source module within systemg (e.g., `systemg::daemon`, `systemg::supervisor`)
- **MESSAGE**: Human-readable log message

Example:
```
2025-12-02T10:30:15.123456Z  INFO systemg::daemon: Starting service: api-server
```

## Log Management

### Log Rotation

Systemg doesn't automatically rotate supervisor logs. For production use, consider setting up log rotation:

**Using logrotate on Linux:**

Create `/etc/logrotate.d/systemg`:

```
~/.local/share/systemg/logs/supervisor.log {
    daily
    rotate 7
    compress
    missingok
    notifempty
    create 0644
}
```

**Manual rotation:**

```sh
# Archive current log
$ mv ~/.local/share/systemg/logs/supervisor.log ~/.local/share/systemg/logs/supervisor.log.old

# Systemg will create a new log file on next write
# No need to restart systemg
```

### Cleaning Logs

To clear supervisor logs:

```sh
# Truncate the log file
$ truncate -s 0 ~/.local/share/systemg/logs/supervisor.log

# Or delete it entirely
$ rm ~/.local/share/systemg/logs/supervisor.log
```

The log file will be recreated automatically when systemg next starts or writes a log entry.

## Troubleshooting

### Log file not created

If the supervisor log file doesn't exist:
1. Check that systemg has been started at least once
2. Verify the `~/.local/share/systemg/` directory exists
3. Check file permissions on the directory

### Empty log file

If the log file exists but is empty:
1. Check the log level - it may be set too high (`error` or `off`)
2. Verify services are actually starting/running
3. Try running with `--log-level debug` for more output

### Logs growing too large

If supervisor logs are consuming too much disk space:
1. Implement log rotation (see Log Management section)
2. Adjust log level to `warn` or `error` to reduce verbosity
3. Archive old logs periodically

## Best Practices

1. **Use appropriate log levels**:
   - Development: `debug` for detailed troubleshooting
   - Production: `info` for normal operations
   - Critical systems: `warn` for minimal logging

2. **Monitor supervisor logs regularly**:
   - Check for unexpected restarts
   - Watch for error patterns
   - Monitor cron job success rates

3. **Implement log rotation**:
   - Prevent disk space issues
   - Archive historical logs
   - Automate cleanup

4. **Combine with service logs**:
   - Use `sysg logs` without flags to see both supervisor and service logs
   - Correlate supervisor events with service output
   - Debug timing issues by comparing timestamps

## Related Commands

- [`sysg logs`](./usage/logs.md) - View logs with filtering options
- [`sysg status`](./usage/status.md) - Check service status
- [`sysg start`](./usage/start.md) - Start services with log level control
