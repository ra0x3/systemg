---
sidebar_position: 4
title: Configuration
---

## Overview

Systemg uses a YAML-based configuration file to define services, their dependencies, restart policies, and deployment strategies. The configuration file is typically named `systemg.yaml` or `sysg.yaml` and should be placed in your project root.

## Configuration Structure

### Top-Level Configuration

The configuration file has the following top-level sections:

- **`version`** – Configuration file version (currently `"1"`)
- **`env`** – Optional environment configuration shared by all services (see [Root-Level Environment Variables](#root-level-environment-variables))
- **`services`** – Map of service names to their configurations

### Complete Example

Below is a comprehensive example showing all available configuration options:

```yaml
# Configuration file version
version: "1"

# Optional root-level environment variables (shared across all services)
env:
  file: "/etc/myapp/common.env"
  vars:
    LOG_LEVEL: "info"
    APP_ENV: "production"

services:
  # Example service with environment file
  postgres:
    # The command to start the service (required)
    command: "postgres -D /var/lib/postgres"

    # Environment variable configuration (optional)
    env:
      # Path to a file containing environment variables for this service
      file: "/etc/myapp/database.env"

    # Policy for restarting the service: "always", "on-failure", or "never" (optional)
    restart_policy: "always"

    # Time to wait before attempting a restart (backoff duration) (optional)
    backoff: "5s"

    # Maximum number of restart attempts before giving up (optional, default: unlimited)
    max_restarts: 10

    # Lifecycle hooks (optional)
    hooks:
      # Commands to run when the service starts
      on_start:
        success:
          command: "echo 'Postgres started'"
        error:
          command: "echo 'Postgres failed to start'"
      # Commands to run when the service stops unexpectedly
      on_stop:
        error:
          command: "echo 'Postgres crashed'"

  # Example service with inline environment variables and deployment configuration
  django:
    # The command to start the service (required)
    command: "python manage.py runserver"

    # Environment variable configuration (optional)
    env:
      # Inline environment variables for the service
      vars:
        DEBUG: "true"
        DATABASE_URL: "postgres://user:password@localhost:5432/dbname"

    # Policy for restarting the service: "always", "on-failure", or "never" (optional)
    restart_policy: "on-failure"

    # Time to wait before attempting a restart (backoff duration) (optional)
    backoff: "5s"

    # Deployment strategy configuration (optional)
    py__deployment:
      # Deployment strategy: "rolling" or "immediate" (default: "immediate")
      strategy: "rolling"
      # Optional build or migration step executed before the service starts (works with any strategy)
      pre_start: "python manage.py migrate"
      # Optional health probe the new instance must satisfy
      health_check:
        # Health check URL (required if health_check is specified)
        url: "http://localhost:8000/health"
        # Health check timeout duration (e.g., "30s") (optional, default: "30s")
        timeout: "45s"
        # Number of retries before giving up (optional, default: 3)
        retries: 4
      # Optional grace window before the old instance is terminated
      grace_period: "5s"

    # List of services this one depends on (must be started before this) (optional)
    depends_on:
      - "postgres"

    # Lifecycle hooks (optional)
    hooks:
      on_start:
        success:
          command: "curl -X POST http://example.com/hook/django-start"
        error:
          command: "curl -X POST http://example.com/hook/django-error"
      on_stop:
        success:
          command: "curl -X POST http://example.com/hook/django-stop"

  # Example service with minimal configuration
  ngrok:
    # The command to start the service (required)
    command: "ngrok http 8000"

    # Policy for restarting the service: "always", "on-failure", or "never" (optional)
    restart_policy: "on-failure"

    # Time to wait before attempting a restart (backoff duration) (optional)
    backoff: "3s"

    # Lifecycle hooks (optional)
    hooks:
      on_start:
        success:
          command: "echo 'ngrok started'"
      on_restart:
        success:
          command: "echo 'ngrok restarted'"

  # Example service with cron scheduling
  sh__backup:
    # The command to start the service (required)
    command: "sh /scripts/backup.sh"

    # Cron configuration for scheduled service execution (optional)
    cron:
      # Cron expression defining the schedule (e.g., "0 * * * * *") (required if cron is specified)
      expression: "0 0 * * * *"  # Every hour at minute 0
      # Optional timezone for cron scheduling (defaults to system timezone)
      timezone: "America/New_York"

  # Example service with skip condition
  sh__optional:
    # The command to start the service (required)
    command: "sh optional.sh"

    # Skip this service if a condition is met (optional)
    # If the command exits with status 0, the service is skipped
    skip: "test -z \"$ENABLE_OPTIONAL\""
```

## Service Configuration Options

### Basic Service Properties

#### `command` (required)

The command to start the service. This is the only required field for each service.

```yaml
services:
  myapp:
    command: "./target/release/myapp"
```

#### `restart_policy` (optional)

Policy for restarting the service when it exits. Valid values:

- `"always"` – Always restart the service when it exits
- `"on-failure"` – Only restart if the service exits with a non-zero status code
- `"never"` – Never restart the service automatically

```yaml
services:
  myapp:
    command: "./myapp"
    restart_policy: "always"
```

#### `backoff` (optional)

Time to wait before attempting a restart after a service fails. Accepts duration strings like `"5s"`, `"1m"`, `"30s"`, etc.

```yaml
services:
  myapp:
    command: "./myapp"
    restart_policy: "on-failure"
    backoff: "5s"
```

#### `max_restarts` (optional)

Maximum number of restart attempts before giving up on a failing service. When a service crashes repeatedly, this limit prevents infinite restart loops. If not specified (or set to `null`), the service will restart indefinitely according to its `restart_policy`.

After reaching the maximum restart attempts, systemg will log an error and stop trying to restart the service. The restart counter is reset to zero after a service runs successfully, so services that recover normally will get fresh restart attempts if they fail again later.

```yaml
services:
  myapp:
    command: "./myapp"
    restart_policy: "on-failure"
    backoff: "5s"
    max_restarts: 5  # Give up after 5 consecutive failed restart attempts
```

This is particularly useful for services that may fail due to external conditions (like port conflicts, missing dependencies, or configuration errors) where continuing to restart indefinitely would waste resources.

#### `skip` (optional)

A shell command that determines whether the service should be skipped. If the command exits with status code 0 (success), the service is skipped and will not be started. If the command exits with a non-zero status code, the service proceeds normally.

This is useful for conditional service execution based on environment conditions, feature flags, or other runtime criteria.

```yaml
services:
  myapp:
    command: "./myapp"
    # Skip this service if a feature flag file doesn't exist
    skip: "test ! -f /etc/myapp/feature-enabled"

  postgres:
    command: "postgres -D /var/lib/postgres"
    # Skip if running in CI environment
    skip: "test -n \"$CI\""
```

When a service is skipped:
- The service is treated as successfully started for dependency resolution
- Dependent services can still start normally
- A log message is emitted indicating the service was skipped

### Environment Variables

Environment variables can be configured at two levels: root-level (applying to all services by default) or service-level (applying to a specific service). Service-level environment configuration overrides root-level configuration.

#### Root-Level Environment Variables

You can define environment variables at the top level of your configuration file that apply to all services by default. This is useful when you have common environment variables shared across multiple services.

```yaml
version: "1"

# Root-level environment variables
env:
  file: "/etc/myapp/common.env"
  vars:
    LOG_LEVEL: "info"
    APP_ENV: "production"

services:
  api:
    command: "./api"
    # api inherits LOG_LEVEL=info and APP_ENV=production from root env

  worker:
    command: "./worker"
    # worker also inherits LOG_LEVEL=info and APP_ENV=production from root env
```

**Override Behavior:**

When both root-level and service-level environment configurations are present, service-level configuration takes precedence:

- **Service-level `env.file`** overrides root-level `env.file`
- **Service-level `env.vars`** override root-level `env.vars` for matching keys
- Variables defined only at the root level are inherited by all services
- Variables defined only at the service level apply only to that service

```yaml
version: "1"

# Root-level environment variables
env:
  file: "/etc/myapp/common.env"
  vars:
    LOG_LEVEL: "info"
    APP_ENV: "production"
    TIMEOUT: "30s"

services:
  api:
    command: "./api"
    env:
      # Service-level env overrides root-level env
      vars:
        LOG_LEVEL: "debug"  # Overrides root LOG_LEVEL
        PORT: "8080"        # Additional service-specific variable
    # Result: LOG_LEVEL=debug, APP_ENV=production, TIMEOUT=30s, PORT=8080

  worker:
    command: "./worker"
    # worker uses root-level env without modifications
    # Result: LOG_LEVEL=info, APP_ENV=production, TIMEOUT=30s
```

#### Service-Level Environment Variables

#### `env` (optional)

Configuration for environment variables specific to a service. You can use either a file or inline variables, or both.

#### `env.file` (optional)

Path to a file containing environment variables. Can be absolute or relative to the configuration file location. If specified, this overrides any root-level `env.file`.

```yaml
services:
  myapp:
    command: "./myapp"
    env:
      file: "/etc/myapp/.env"
```

#### `env.vars` (optional)

Inline key-value pairs of environment variables. These override any matching root-level `env.vars`.

```yaml
services:
  myapp:
    command: "./myapp"
    env:
      vars:
        DEBUG: "true"
        PORT: "8080"
        DATABASE_URL: "postgres://localhost/mydb"
```

### Privileged Service Options

The following fields are most useful when the supervisor runs with `--sys`:

#### `user`, `group`, `supplementary_groups`

Switch the service to a target account after privileged setup.

```yaml
services:
  api:
    command: "/usr/local/bin/api"
    user: "www-data"
    group: "www-data"
    supplementary_groups: ["www-logs"]
```

#### `limits`

Controls resource limits (`setrlimit`), scheduler priority, CPU affinity, and optional cgroup v2 settings.

| Field            | Description                                       |
| ---------------- | ------------------------------------------------- |
| `nofile`         | Max open file descriptors (`RLIMIT_NOFILE`)       |
| `nproc`          | Max processes (`RLIMIT_NPROC`, Unix only)         |
| `memlock`        | Locked memory (`RLIMIT_MEMLOCK`), supports `K/M/G/T` suffixes |
| `nice`           | Scheduler priority (-20..19)                      |
| `cpu_affinity`   | CPU cores to pin the process to (Linux)           |
| `cgroup`         | cgroup v2 controllers (memory/cpu)                |

```yaml
services:
  worker:
    command: "./worker"
    limits:
      nofile: 32768
      memlock: "256M"
      nice: 5
      cgroup:
        memory_max: "1G"
        cpu_max: "200000 100000"
```

#### `capabilities`

Linux capabilities to retain after the privilege drop. All other capability sets are cleared.

```yaml
services:
  proxy:
    command: "./proxy"
    capabilities:
      - CAP_NET_BIND_SERVICE
      - CAP_SYS_NICE
```

#### `isolation`

Request kernel namespaces or sandbox hints. Unsupported toggles log warnings instead of crashing.

```yaml
services:
  sandboxed:
    command: "./app"
    isolation:
      network: true
      pid: true
      mount: true
      private_tmp: true
```

### Service Dependencies

#### `depends_on` (optional)

List of service names that must be started before this service. Systemg evaluates the dependency graph before launching processes and enforces the following rules:

- **Topological startup** – services are started in an order that guarantees every dependency is already running (or has exited successfully for one-shot jobs) before its dependents launch.
- **Fail fast on unhealthy prerequisites** – if a dependency fails to start, dependents are skipped and the failure is surfaced instead of allowing a partial boot.
- **Cascading shutdowns** – when a running service crashes, all services that depend on it are stopped automatically to keep the environment consistent.

```yaml
services:
  redis:
    command: "redis-server"

  worker:
    command: "node worker.js"
    depends_on:
      - redis
```

If `redis` exits with a non-zero status, `worker` will not start (or will be stopped if it is already running) until `redis` is healthy again.

### Deployment Configuration

#### `deployment` (optional)

Deployment strategy configuration for managing service restarts and updates.

#### `deployment.strategy` (optional)

Deployment strategy for the service. Valid values:

- `"immediate"` *(default)* – stop the running instance and start a fresh copy right away. This matches the behavior in earlier releases and requires no additional configuration.
- `"rolling"` – launch a replacement alongside the existing instance, verify it is healthy, optionally wait for a grace period, and only then terminate the previous process. This keeps services available throughout a restart.

```yaml
services:
  api:
    command: "./target/release/api"
    restart_policy: "always"
    deployment:
      strategy: "rolling"
```

#### `deployment.pre_start` (optional)

Shell command executed before the service process launches. Useful for builds, migrations, or asset preparation.

**Works with any deployment strategy** (not just rolling deployments):
- **Initial startup**: If the pre_start command fails (non-zero exit code), the service will not start
- **Rolling restart**: If the pre_start command fails, deployment is aborted and the old instance is preserved
- **Immediate restart**: If the pre_start command fails, the service will not start

```yaml
services:
  api:
    command: "./target/release/api"
    deployment:
      # pre_start works with or without specifying a strategy
      pre_start: "cargo build --release"
```

#### `deployment.health_check` (optional)

HTTP probe configuration that the new instance must pass before the old instance is terminated.

#### `deployment.health_check.url` (required if `health_check` is specified)

The URL to check for health. Must return a successful HTTP status code (2xx).

```yaml
services:
  api:
    command: "./api"
    deployment:
      strategy: "rolling"
      health_check:
        url: "http://localhost:8080/health"
```

#### `deployment.health_check.timeout` (optional)

Maximum time to wait for health checks to pass. Defaults to `"30s"` if not specified.

```yaml
services:
  api:
    command: "./api"
    deployment:
      strategy: "rolling"
      health_check:
        url: "http://localhost:8080/health"
        timeout: "60s"
```

#### `deployment.health_check.retries` (optional)

Number of retry attempts before giving up. Defaults to `3` if not specified.

```yaml
services:
  api:
    command: "./api"
    deployment:
      strategy: "rolling"
      health_check:
        url: "http://localhost:8080/health"
        timeout: "60s"
        retries: 5
```

#### `deployment.grace_period` (optional)

Additional delay to keep the old instance alive after the new one passes health checks. Handy for draining load balancer connections.

```yaml
services:
  api:
    command: "./api"
    deployment:
      strategy: "rolling"
      health_check:
        url: "http://localhost:8080/health"
      grace_period: "5s"
```

If any step of the rolling restart fails, the new process is halted and the previous instance is restored automatically. This ensures unhealthy builds never displace a working service.

### Lifecycle Hooks

#### `hooks` (optional)

Commands to run at specific points in the service lifecycle. Each lifecycle stage supports
`success` and `error` handlers with a required `command` and an optional `timeout`
(`"10s"`, `"2m"`, etc.). Hook commands inherit the service environment; values defined in
`env.vars` override those loaded from `.env` files. See the dedicated
[Webhooks](./webhooks.md) guide for a deeper reference and best practices.

#### `hooks.on_start` (optional)

Runs after Systemg attempts to start the service. `success` handlers fire once the process
survives the initial readiness window (or exits cleanly immediately for one-shot tasks), while
`error` handlers run if the spawn fails or the process exits before reaching that state (for
example, when the binary is missing or crashes instantly).

```yaml
services:
  myapp:
    command: "./myapp"
    hooks:
      on_start:
        success:
          command: "curl -X POST https://example.com/hooks/myapp-started"
          timeout: "10s"
        error:
          command: "curl -X POST https://example.com/hooks/myapp-start-failed"
```

#### `hooks.on_stop` (optional)

Runs whenever the service exits. `success` handlers execute for graceful shutdowns (including
operator-initiated stops), while `error` handlers execute if the process crashes or exits with a
non-zero status.

```yaml
services:
  myapp:
    command: "./myapp"
    hooks:
      on_stop:
        success:
          command: "curl -X POST https://example.com/hooks/myapp-stopped"
        error:
          command: "curl -X POST https://example.com/hooks/myapp-crashed"
```

#### `hooks.on_restart` (optional)

Runs when Systemg automatically restarts a crashed service. Use this to surface self-healing
events to external systems.

```yaml
services:
  myapp:
    command: "./myapp"
    hooks:
      on_restart:
        success:
          command: "curl -X POST https://example.com/hooks/myapp-restarted"
```

### Cron Scheduling

#### `cron` (optional)

Configuration for scheduled service execution. When specified, the service runs on a cron schedule rather than continuously.

#### `cron.expression` (required if `cron` is specified)

Cron expression defining the schedule. Uses standard cron format with seconds: `"second minute hour day month weekday"`.

```yaml
services:
  backup:
    command: "sh /scripts/backup.sh"
    cron:
      expression: "0 0 * * * *"  # Every hour at minute 0
```

#### `cron.timezone` (optional)

Timezone for cron scheduling. Defaults to system timezone if not specified.

```yaml
services:
  backup:
    command: "sh /scripts/backup.sh"
    cron:
      expression: "0 0 * * * *"
      timezone: "America/New_York"
```

### Dynamic Process Spawning

#### `spawn_mode` (optional)

Controls whether a service can dynamically spawn child processes at runtime. When set to `"dynamic"`, the service can use the `sysg spawn` command to create tracked child processes.

Valid values:
- `"static"` *(default)* – Service cannot spawn tracked children
- `"dynamic"` – Service can spawn child processes that inherit monitoring and logging

```yaml
services:
  orchestrator:
    command: "python orchestrator.py"
    spawn_mode: "dynamic"
    spawn_limits:
      max_children: 100
```

#### `spawn_limits` (optional)

Resource limits and policies for dynamically spawned children. Only applies when `spawn_mode` is `"dynamic"`.

#### `spawn_limits.max_children` (optional)

Maximum number of direct child processes this service can spawn. Default: `100`.

```yaml
services:
  orchestrator:
    command: "./orchestrator"
    spawn_mode: "dynamic"
    spawn_limits:
      max_children: 50
```

#### `spawn_limits.max_depth` (optional)

Maximum depth of the spawn tree (levels of recursion). Prevents infinite spawn chains. Default: `3`.

```yaml
services:
  orchestrator:
    command: "./orchestrator"
    spawn_mode: "dynamic"
    spawn_limits:
      max_depth: 2  # orchestrator -> child -> grandchild (no deeper)
```

#### `spawn_limits.max_descendants` (optional)

Total maximum number of descendants across all levels. Default: `500`.

```yaml
services:
  orchestrator:
    command: "./orchestrator"
    spawn_mode: "dynamic"
    spawn_limits:
      max_descendants: 200
```

#### `spawn_limits.total_memory` (optional)

Memory limit shared by the entire spawn tree. Supports `K/M/G/T` suffixes.

```yaml
services:
  orchestrator:
    command: "./orchestrator"
    spawn_mode: "dynamic"
    spawn_limits:
      total_memory: "2GB"
```

#### `spawn_limits.termination_policy` (optional)

Policy for handling process termination in spawn trees. Valid values:

- `"cascade"` *(default)* – Terminate all descendants when parent dies
- `"orphan"` – Leave children running when parent dies
- `"reparent"` – Reassign children to init process

```yaml
services:
  orchestrator:
    command: "./orchestrator"
    spawn_mode: "dynamic"
    spawn_limits:
      termination_policy: "cascade"
```

#### Complete Dynamic Spawn Example

```yaml
services:
  # Parent service that can spawn workers dynamically
  task_orchestrator:
    command: "python orchestrator.py"
    spawn_mode: "dynamic"
    spawn_limits:
      max_children: 100       # Direct children limit
      max_depth: 3            # Maximum spawn tree depth
      max_descendants: 500    # Total across all levels
      total_memory: "2GB"     # Shared by entire tree
      termination_policy: "cascade"  # Clean up all children on exit

  # Agent spawner for autonomous tasks
  agent_controller:
    command: "./agent_controller"
    spawn_mode: "dynamic"
    spawn_limits:
      max_children: 20
      max_depth: 2            # Agents can spawn sub-agents
      max_descendants: 50
      termination_policy: "orphan"  # Let agents finish their work
```

Child processes spawned from these services:
- Inherit their parent's monitoring and logging
- Appear in `sysg status` with hierarchical display
- Are subject to spawn limits and rate limiting (10 spawns/second)
- Can be spawned with time-to-live (TTL) for automatic cleanup
- Support both traditional workers and LLM-powered agents

To spawn children from a dynamic service, use the `sysg spawn` command:

```bash
# Spawn a traditional worker
sysg spawn --name worker_42 --ttl 3600 -- python worker.py

# Spawn an autonomous agent
sysg spawn --name optimizer --provider claude --goal "Optimize database queries"
```

See the [Spawn Command](/docs/usage/spawn) documentation for detailed usage.
