---
sidebar_position: 1
title: start
---

## Overview

The `start` command launches the systemg process manager and begins managing services defined in your configuration file. It supports both foreground and daemonized modes, automatically handles service dependencies, and monitors services for crashes.

## Usage

### Basic Usage

Start the process manager with the default configuration file (`systemg.yaml` or `sysg.yaml`):

```sh
$ sysg start
```

Start the process manager with a specific configuration file:

```sh
$ sysg start --config systemg.yaml
```

### Daemon Mode

Start the long-lived supervisor that persists after you log out:

```sh
$ sysg start --config systemg.yaml --daemonize
```

When daemonized, systemg runs in the background and subsequent commands (`status`, `restart`, `logs`, `stop`) communicate with the same background process via a Unix domain socket rather than re-spawning services.

### Logging Verbosity

Adjust logging verbosity for the current invocation. Named levels are case-insensitive and numbers map to TRACE=5 down to OFF=0:

```sh
$ sysg start --log-level debug
$ sysg start --log-level 4
```

The same `--log-level` flag works with every `sysg` subcommand if you need different verbosity elsewhere.

## How It Works

### Configuration Loading

1. **File Resolution**: Systemg first attempts to resolve the configuration file path:
   - If an absolute path is provided, it's used as-is
   - If a relative path is provided, it's resolved relative to the current working directory
   - If no path is specified, systemg looks for `systemg.yaml` or `sysg.yaml` in the current directory

2. **Configuration Parsing**: The YAML file is parsed and validated:
   - Environment variables in the config are expanded using `${VAR_NAME}` syntax
   - Service dependencies are validated to ensure no circular dependencies exist
   - Environment files specified in `env.file` are loaded and applied

### Service Startup Process

#### Dependency Resolution

When configuration files declare `depends_on` entries, `sysg start` bootstraps services in dependency order using a topological sort:

1. **Dependency Graph Construction**: Systemg builds a dependency graph from the `depends_on` fields
2. **Topological Ordering**: Services are ordered so that every dependency starts before its dependents
3. **Validation**: The graph is checked for:
   - Circular dependencies (which cause an error)
   - Unknown dependencies (services that don't exist in the config)

#### Service Launch Sequence

For each service in dependency order:

1. **Dependency Check**: Before starting a service, systemg verifies all its dependencies are running:
   - If a dependency failed to start, the service is skipped
   - If a dependency is not yet running, the service waits (this shouldn't happen due to topological ordering)
   - Failed services are tracked to prevent dependent services from starting

2. **Process Launch**: The service command is executed:
   - Commands are executed via `sh -c` to support shell features
   - Each service runs in its own process group (on Linux, uses `setpgid(0, 0)`)
   - On Linux, services are configured to receive `SIGTERM` when systemg exits using `prctl(PR_SET_PDEATHSIG)`
   - On macOS, services share the same process group for coordinated termination

3. **Environment Setup**:
   - Environment variables from `env.vars` are set
   - Environment files from `env.file` are loaded
   - The working directory is set to the project root (config file's parent directory)

4. **Log Capture**: 
   - Standard output and standard error are captured via pipes
   - Separate threads write logs to files in `~/.local/share/systemg/logs/` followed by the service name and `_stdout.log` or `_stderr.log`
   - Logs are written asynchronously to avoid blocking the service

5. **Lifecycle Hooks**: If configured, `on_start.success` hooks are executed after the process is spawned, while `on_start.error` hooks run if the spawn fails

6. **Readiness Verification**: Systemg polls the service to confirm it's running:
   - Polls every 50ms for up to 5 seconds
   - Confirms the process is still alive across consecutive polls
   - For one-shot services (that exit immediately), success is determined by exit code

7. **PID Tracking**: The service's PID is recorded in `~/.local/share/systemg/pid.json` for later commands

### Foreground vs Daemon Mode

#### Foreground Mode (default)

When started without `--daemonize`:

- Systemg runs in the foreground and blocks until services exit
- A signal handler is registered for `SIGINT`/`SIGTERM` to gracefully shut down services
- Services are terminated by sending `SIGTERM` to their process groups, then `SIGKILL` if needed
- The process exits after all services are stopped

#### Daemon Mode (`--daemonize`)

When started with `--daemonize`:

1. **Duplicate Check**: Systemg first checks if a supervisor is already running:
   - Reads the supervisor PID from `~/.local/share/systemg/supervisor.pid`
   - Sends a null signal to verify the process exists
   - If running, aborts with a warning

2. **Daemonization Process**:
   - Forks twice to detach from the terminal (double-fork pattern)
   - Creates a new session with `setsid()`
   - Changes working directory to `/`
   - Redirects stdin, stdout, and stderr to `/dev/null`

3. **Supervisor Initialization**:
   - Creates a Unix domain socket at `~/.local/share/systemg/supervisor.sock`
   - Writes the supervisor PID to the PID file
   - Starts only non-cron services (cron services are managed separately)
   - Spawns a background thread to check for due cron jobs every second

4. **Event Loop**: The supervisor enters an event loop:
   - Listens for commands on the Unix socket
   - Handles `Stop`, `Restart`, and `Shutdown` commands from CLI invocations
   - Manages service lifecycle and responds to crashes

### Service Monitoring

After services are started, systemg spawns a monitoring thread that:

1. **Periodic Checks**: Polls all running services every 2 seconds
2. **Crash Detection**: Detects when services exit unexpectedly
3. **Restart Handling**: For services with `restart_policy: "always"` or `restart_policy: "on-failure"`:
   - Waits for the configured `backoff` duration (default: 5 seconds)
   - Runs any `on_stop.error` hooks to report the crash
   - Restarts the service if the policy allows and fires `on_restart.success` hooks on success (or `on_restart.error` if the restart attempt fails)
4. **Dependency Cascading**: When a service crashes, all services that depend on it are automatically stopped to prevent workloads from running against unhealthy backends

### Cron Services

Services with `cron` configuration are handled specially:

- They are **not** started during the initial service launch
- A background thread checks for due cron jobs every second
- When a cron job is due, it's executed in a separate thread
- The job is monitored until completion (up to 1 hour)
- Completion status is tracked for overlap detection

## Error Handling

### Dependency Failures

If a prerequisite service fails to start:
- All dependent services are skipped
- The command exits with a dependency error
- The system is not left in a half-running state

### Service Start Failures

If a service fails to start:
- The error is logged
- `on_start.error` hooks are executed if configured
- Dependent services are skipped
- The first error encountered is returned

### Configuration Errors

Invalid configuration files result in:
- Clear error messages indicating the problem
- No services are started
- The process exits with an error code

## Command Options

```
$ sysg start --help
Start the process manager with the given configuration

Usage: sysg start [OPTIONS]

Options:
  -c, --config <CONFIG>    Path to the configuration file (defaults to `systemg.yaml`) [default: systemg.yaml]
      --log-level <LEVEL>  Override the logging verbosity for this invocation only
      --daemonize          Whether to daemonize systemg
  -h, --help               Print help
```
