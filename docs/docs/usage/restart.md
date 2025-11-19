---
sidebar_position: 3
title: restart
---

## Overview

The `restart` command restarts services managed by systemg, optionally reloading the configuration file. It supports both immediate and rolling restart strategies, and can restart a single service or all services.

## Usage

### Restart All Services

Restart all services using the current configuration:

```sh
$ sysg restart
```

### Restart with New Configuration

Restart all services using a different configuration file:

```sh
$ sysg restart --config new-config.yaml
```

### Restart a Single Service

Restart only a specific service:

```sh
$ sysg restart --service myapp
```

### Start Supervisor if Not Running

If no supervisor is running, start it in daemon mode before restarting:

```sh
$ sysg restart --daemonize
```

## How It Works

### Supervisor Detection

The `restart` command first determines whether a daemonized supervisor is running:

1. **Supervisor Check**: 
   - Reads the supervisor PID from `~/.local/share/systemg/supervisor.pid`
   - Sends a null signal to verify the process exists
   - If the supervisor is running, commands are sent via Unix domain socket IPC
   - If no supervisor is found, services are restarted directly

### Restarting via Supervisor (Daemon Mode)

When a supervisor is running:

1. **Command Construction**: 
   - If `--service` is specified, creates a `ControlCommand::Restart { service: Some(name), config: ... }`
   - If no service is specified, creates a `ControlCommand::Restart { service: None, config: ... }`
   - If `--config` is provided, includes the config path; otherwise uses `None`

2. **IPC Communication**:
   - Connects to the Unix domain socket
   - Sends the command as serialized JSON
   - Waits for a response

3. **Supervisor Handling**:
   - **Single Service Restart**: 
     - If a config path is provided, reloads the configuration
     - Calls `daemon.restart_service(name, service_config)` with the service's deployment strategy
   - **All Services Restart**:
     - Reloads the configuration from the specified path (or current config if none specified)
     - Stops all existing services
     - Shuts down the monitor thread
     - Creates a new `Daemon` instance from the reloaded config
     - Starts all services in dependency order
     - Spawns a new monitor thread

### Direct Service Restarting (Foreground Mode)

When no supervisor is running:

1. **Daemon Construction**: 
   - Loads the configuration file
   - Creates a `Daemon` instance from the config

2. **Service Restarting**:
   - If `--service` is specified: calls `daemon.restart_service(name, service_config)`
   - If no service is specified: calls `daemon.restart_services()`

3. **Daemonize Option**:
   - If `--daemonize` is specified, starts the supervisor in daemon mode first
   - Then proceeds with the restart via supervisor

### Restart Strategies

Systemg supports two deployment strategies for restarts, determined by the service's `deployment.strategy` configuration:

#### Immediate Restart (default)

When `deployment.strategy` is `"immediate"` or not specified:

1. **Stop Service**: 
   - Stops the running service using the same process as `sysg stop`
   - Sends `SIGTERM` to the process group
   - Waits for graceful termination

2. **Start Service**: 
   - Starts the service using the same process as `sysg start`
   - Waits for readiness verification
   - There is a brief downtime between stop and start

#### Rolling Restart

When `deployment.strategy` is `"rolling"`:

1. **Detach Current Instance**: 
   - Removes the service from the process map
   - Keeps the process handle to restore if the new instance fails
   - The old instance continues running during the restart

2. **Pre-Start Command** (if configured):
   - Executes the `deployment.pre_start` command (e.g., builds, migrations)
   - Streams stdout/stderr to logs with `[service pre-start]` prefix
   - If the command fails (non-zero exit), the restart is aborted and the old instance is restored

3. **Start New Instance**: 
   - Launches the new service process
   - If startup fails, the old instance is restored

4. **Readiness Verification**: 
   - Waits for the service to be ready (same polling as startup)
   - If the service exits immediately with success, it's considered ready
   - If readiness fails, the new instance is stopped and the old instance is restored

5. **Health Check** (if configured):
   - Performs HTTP health checks against `deployment.health_check.url`
   - Retries up to `deployment.health_check.retries` times (default: 3)
   - Total time is bounded by `deployment.health_check.timeout` (default: 30s)
   - If health checks fail, the new instance is stopped and the old instance is restored

6. **Grace Period** (if configured):
   - Waits for `deployment.grace_period` duration after health checks pass
   - Allows time for load balancers to drain connections to the old instance

7. **Terminate Old Instance**: 
   - After the new instance is healthy and the grace period expires
   - Sends `SIGTERM` to the old instance's process group
   - Ensures zero-downtime during the restart

### Restarting All Services

When restarting all services (no `--service` flag):

1. **Dependency Order**: Services are restarted in topological dependency order
2. **Strategy Per Service**: Each service uses its own configured deployment strategy
3. **Sequential Restart**: Services are restarted one at a time (not in parallel)
4. **Monitor Thread**: A new monitor thread is spawned after all services are restarted

### Configuration Reloading

When a new configuration file is specified:

1. **Path Resolution**: 
   - If absolute, used as-is
   - If relative, resolved relative to the supervisor's config directory (for supervisor mode) or current directory (for direct mode)

2. **Validation**: 
   - The new configuration is loaded and validated
   - Dependency cycles are checked
   - Environment variables are expanded

3. **Service Comparison**: 
   - Services in the new config that don't exist in the old config are started
   - Services in the old config that don't exist in the new config are stopped
   - Services that exist in both are restarted

## Error Handling

### Restart Failures

If a service fails to restart:
- For rolling restarts: the old instance is automatically restored
- For immediate restarts: the service remains stopped
- Dependent services are not automatically restarted

### Configuration Errors

If the new configuration is invalid:
- The restart is aborted
- Existing services continue running with the old configuration
- An error message is returned

### Health Check Failures

If health checks fail during a rolling restart:
- The new instance is stopped
- The old instance is restored
- An error is returned

## Command Options

```
$ sysg restart --help
Restart the process manager, optionally specifying a new configuration file

Usage: sysg restart [OPTIONS]

Options:
  -c, --config <CONFIG>    Path to the configuration file (defaults to `systemg.yaml`) [default: systemg.yaml]
      --log-level <LEVEL>  Override the logging verbosity for this invocation only
  -s, --service <SERVICE>  Optionally restart only the named service
      --daemonize          Start the supervisor before restarting if it isn't already running
  -h, --help               Print help
```
