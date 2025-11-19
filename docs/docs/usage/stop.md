---
sidebar_position: 2
title: stop
---

## Overview

The `stop` command terminates running services managed by systemg. It can stop a specific service or all services, and works with both foreground and daemonized supervisors.

## Usage

### Stop All Services

Stop all running services managed by `systemg`:

```sh
$ sysg stop
```

### Stop a Specific Service

Stop a specific service by name:

```sh
$ sysg stop --service myapp
```

## How It Works

### Supervisor Detection

The `stop` command first determines whether a daemonized supervisor is running:

1. **Supervisor Check**: 
   - Reads the supervisor PID from `~/.local/share/systemg/supervisor.pid`
   - Sends a null signal (`kill(pid, 0)`) to verify the process exists
   - If the supervisor is running, commands are sent via Unix domain socket IPC
   - If no supervisor is found, services are stopped directly

### Stopping via Supervisor (Daemon Mode)

When a supervisor is running:

1. **Command Construction**: 
   - If `--service` is specified, creates a `ControlCommand::Stop { service: Some(name) }`
   - If no service is specified, creates a `ControlCommand::Shutdown` to stop all services

2. **IPC Communication**:
   - Connects to the Unix domain socket at `~/.local/share/systemg/supervisor.sock`
   - Sends the command as serialized JSON
   - Waits for a response from the supervisor

3. **Supervisor Handling**:
   - The supervisor receives the command in its event loop
   - For a specific service: calls `daemon.stop_service(&service)`
   - For shutdown: calls `daemon.stop_services()` and then shuts down the supervisor itself

### Direct Service Stopping (Foreground Mode)

When no supervisor is running:

1. **Daemon Construction**: 
   - Loads the configuration file
   - Creates a `Daemon` instance from the config
   - Loads the PID file to find running services

2. **Service Stopping**:
   - If `--service` is specified: calls `daemon.stop_service(&service)`
   - If no service is specified: calls `daemon.stop_services()`

### Service Termination Process

For each service to be stopped:

#### PID Lookup

1. **PID File Access**: 
   - Locks the PID file (`~/.local/share/systemg/pid.json`)
   - Retrieves the service's PID from the file
   - If the service is not found in the PID file, logs a warning and continues

#### Process Verification

2. **Process Existence Check**:
   - Sends a null signal (`kill(pid, None)`) to verify the process exists
   - If the process doesn't exist (`ESRCH`), logs a debug message and continues
   - If another error occurs, returns an error

#### Graceful Termination

3. **SIGTERM Signal**:
   - Retrieves the supervisor's process group ID
   - On Linux: Sends `SIGTERM` to the service's process group using `killpg()`
   - On macOS/Unix: Uses `killpg()` to signal the entire process group
   - This allows the service to perform cleanup and exit gracefully

4. **Process Group Handling**:
   - Each service runs in its own process group (created during startup)
   - Signaling the process group ensures all child processes are terminated
   - This prevents orphaned child processes

#### Force Termination

5. **SIGKILL Fallback** (if needed):
   - If the process doesn't terminate after a grace period, `SIGKILL` may be sent
   - `SIGKILL` cannot be caught and forces immediate termination
   - This is a last resort for unresponsive services

#### Cleanup

6. **Resource Cleanup**:
   - Removes the service from the process map
   - Removes the service entry from the PID file
   - Saves the updated PID file to disk

### Stopping All Services

When stopping all services (no `--service` flag):

1. **Iteration**: Iterates over all services in the PID file
2. **Sequential Stopping**: Stops each service using the same termination process
3. **Error Handling**: Continues stopping remaining services even if one fails
4. **Supervisor Shutdown**: If stopping via supervisor, the supervisor itself shuts down after all services are stopped

### Cascading Stops

When a service is stopped, systemg does **not** automatically stop dependent services. However:

- If a service crashes during normal operation, dependent services are automatically stopped (handled by the monitor thread)
- Manual stops via `sysg stop` only affect the specified service(s)

## Error Handling

### Service Not Found

If a specified service is not running:
- The command completes successfully (no error)
- A warning may be logged if the service is not in the PID file

### Process Already Terminated

If a service's process has already exited:
- The PID file entry is cleaned up
- No error is returned

### Permission Errors

If systemg lacks permission to signal a process:
- An error is logged
- The command may fail or continue depending on the error type

## Command Options

```
$ sysg stop --help
Stop the currently running process manager

Usage: sysg stop [OPTIONS]

Options:
  -c, --config <CONFIG>    Path to the configuration file (defaults to `systemg.yaml`) [default: systemg.yaml]
      --log-level <LEVEL>  Override the logging verbosity for this invocation only
  -s, --service <SERVICE>  Name of service to stop (optional)
  -h, --help               Print help
```

**Note**: The `--config` option is used when no supervisor is running to load the configuration and locate services. When a supervisor is running, the supervisor's configuration is used.
