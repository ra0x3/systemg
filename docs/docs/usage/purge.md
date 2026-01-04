---
sidebar_position: 6
title: purge
---

## Overview

The `purge` command completely removes all systemg state and runtime files, giving you a fresh start. This is useful when you need to clear corrupted state, remove stale history, or reset systemg to a clean state after ungraceful shutdowns.

**⚠️ Warning**: This command permanently deletes all service status history, cron execution history, logs, and runtime files. Make sure to backup any logs you need before running this command.

## Usage

### Purge All State

Remove all systemg state and runtime files:

```sh
$ sysg purge
All systemg state has been purged
```

### With Log Level

View detailed information about what's being removed:

```sh
$ sysg purge --log-level debug
```

## What Gets Removed

The purge command removes the entire `~/.local/share/systemg` directory, which contains:

1. **State Files**:
   - `state.json` - Service lifecycle states (Stopped, Running, ExitedSuccessfully, etc.)
   - `cron_state.json` - Cron execution history with timestamps and exit codes

2. **Runtime Files**:
   - `pid.json` - Active PIDs for running services
   - `pid.json.lock` - Lock file for PID file operations
   - `control.sock` - Unix socket for IPC (when supervisor is running)
   - `sysg.pid` - Supervisor process PID (when supervisor is running)
   - [`config_hint`](../state.md#config_hint) - Last used config path (enables config-free commands)

3. **Logs**:
   - `logs/supervisor.log` - Supervisor operational logs
   - `logs/` - Directory containing all service stdout/stderr logs

## When to Use Purge

### After Ungraceful Shutdown

If you've killed the supervisor with `kill -9` or encountered a crash, you may have stale state showing incorrect service statuses:

```sh
$ sysg status
● echo_lines - Process 41520 not found
● py_size - Stopped
```

Even though no services are actually running, the status shows ghost entries. Use purge to clear this:

```sh
$ sysg purge
$ sysg status
# No services found - clean slate
```

### Clearing Service History

If you want to remove all historical information about service exits and cron executions:

```sh
$ sysg purge
$ sysg start --config myapp.yaml --daemonize
# Fresh start with no history
```

### Troubleshooting State Corruption

If `sysg` commands are behaving unexpectedly due to corrupted state files:

```sh
$ sysg stop
# Error: unexpected state...

$ sysg purge
# Removes all potentially corrupted state

$ sysg start --config myapp.yaml --daemonize
# Fresh start
```

## Important Notes

### Stop Services First

While `purge` can be run with services running, it's recommended to stop all services first:

```sh
$ sysg stop
$ sysg purge
```

This ensures clean shutdown before clearing state.

### No Confirmation Prompt

The purge command does **not** ask for confirmation. Once executed, all state is immediately deleted. Make sure you really want to purge before running this command.

### Config Files Are Safe

Your systemg configuration files (e.g., `systemg.yaml`) are **not** affected by purge. Only runtime state and logs in `~/.local/share/systemg` are removed.

## Examples

### Complete Reset After Testing

```sh
# Stop all services
$ sysg stop

# Clear all state and logs
$ sysg purge
All systemg state has been purged

# Start fresh
$ sysg start --config production.yaml --daemonize

# Verify clean state
$ sysg status
# Shows only currently running services
```

### Debug Purge Operation

```sh
$ sysg purge --log-level debug
# Detailed output showing exactly what's being removed
```

## See Also

- [stop](./stop.md) - Stop services before purging
- [status](./status.md) - View service status
- [start](./start.md) - Start services after purging
