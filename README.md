# systemg

Process supervisor with dependencies, health checks, and rolling deployments. Built on systemd/cgroups.

<div align="center">

[![CI](https://img.shields.io/github/actions/workflow/status/ra0x3/systemg/ci.yaml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/ra0x3/systemg/actions/workflows/ci.yaml)
[![GitHub branch status](https://img.shields.io/github/checks-status/ra0x3/systemg/main?style=flat-square&label=checks)](https://github.com/ra0x3/systemg/actions)

[![docs.rs (with version)](https://img.shields.io/docsrs/systemg/latest)](https://docs.rs/systemg)
[![GitHub tag](https://img.shields.io/github/v/tag/ra0x3/systemg?style=flat-square&logo=github&label=version)](https://github.com/ra0x3/systemg/releases)
[![Crate size](https://img.shields.io/crates/size/systemg?style=flat-square&logo=rust&label=size)](https://crates.io/crates/systemg)
![Crates.io Total Downloads](https://img.shields.io/crates/d/systemg)

[![Deps.rs Crate Dependencies (specific version)](https://img.shields.io/deps-rs/systemg/latest)](https://deps.rs/crate/systemg)
[![License](https://img.shields.io/crates/l/systemg?style=flat-square)](LICENSE)

</div>

<div align="center">
    <img src="https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white" />
    <img src="https://img.shields.io/badge/ts--node-3178C6?style=for-the-badge&logo=ts-node&logoColor=white" />
    <img src="https://img.shields.io/badge/Vite-B73BFE?style=for-the-badge&logo=vite&logoColor=FFD62E" />
    <img src="https://img.shields.io/badge/mac%20os-000000?style=for-the-badge&logo=apple&logoColor=white" />
    <img src="https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" />
    <img src="https://img.shields.io/badge/OpenAI-412991?style=for-the-badge&logo=openai&logoColor=white" />
    <img src="https://img.shields.io/badge/Anthropic-D97757?style=for-the-badge&logo=anthropic&logoColor=white" />
</div>

[//]: # (<img height="500px" src="https://i.imgur.com/MHXfe9T.png" />)

## Table of Contents

1. [Getting Started](#getting-started)
   - 1.1 [How It Works](#how-it-works)
   - 1.2 [Installation](#installation)
   - 1.3 [Running a Basic Start Command](#running-a-basic-start-command)
   - 1.3 [How It Works (docs)](docs/docs/how-it-works.md)
2. [Why systemg](#why-systemg)
   - 2.1 [Features](#features)
   - 2.2 [Comparison](#comparison)
3. [Development](#development)
   - 3.1 [Testing](#testing)
   - 3.2 [Build from Source](#build-from-source)
   - 3.3 [Contributing](#contributing)

---

## Getting Started

### Installation

![Installation](https://i.imgur.com/6d2aq0U.gif)

```sh
$ curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh
```

System deployments: `scripts/install-systemg.sh` sets up `/usr/bin/sysg`, `/etc/systemg`, `/var/lib/systemg`. See [security guide](docs/docs/security.md).

### Usage

```sh
$ sysg start                     # Default config (systemg.yaml)
$ sysg start --config my.yaml    # Custom config
$ sysg start --daemonize         # Background supervisor
```

Commands: `sysg stop`, `sysg restart`, `sysg status`, `sysg logs`

---

## Why systemg

Compose programs into systems with explicit dependencies and health checks.

### Features

- **Dependencies** - Topological startup order with health-aware cascading
- **Rolling Deployments** - Blue-green swaps with health validation
- **Environment** - `.env` file propagation
- **Webhooks** - Event notifications ([docs](docs/docs/webhooks.md))
- **Cron** - Scheduled tasks with overlap detection
- **Spawning** - Dynamic child process tracking
- **OS Integration** - systemd/cgroups when available
- **Single Binary** - No runtime dependencies

### Privileged Mode (Optional)

Need to manage system daemons, bind privileged ports, or attach cgroup limits? Run the supervisor in privileged mode:

```sh
# Start with elevated privileges and system-wide state directories
$ sudo sysg --sys start --config /etc/systemg/nginx.yaml --daemonize

# Bind as root, then immediately drop to the configured service user
$ sudo sysg --sys --drop-privileges start --service web

# Check status without elevated privileges (falls back to userspace mode)
$ sysg status --service web
```

In privileged mode systemg relocates state to `/var/lib/systemg`, writes supervisor logs to `/var/log/systemg/supervisor.log`, and respects the new service-level fields:

```yaml
services:
  web:
    command: "./server"
    user: "www-data"
    group: "www-data"
    supplementary_groups: ["www-logs"]
    limits:
      nofile: 65536
      nproc: 4096
      memlock: "unlimited"      # supports K/M/G/T suffixes
      nice: -5
      cpu_affinity: [0, 1]
      cgroup:
        memory_max: "512M"
        cpu_max: "200000 100000"
    capabilities:
      - CAP_NET_BIND_SERVICE
      - CAP_SYS_NICE
    isolation:
      network: true
      pid: true
      mount: true
```

All privileged operations are opt-in: services that omit these fields continue to run unprivileged, and unit tests skip elevated scenarios automatically when not running as root.

#### Dependency Handling

Declare service relationships with the `depends_on` field to coordinate startup order and health checks. Systemg will:

- start services in a topologically sorted order so each dependency is running or has exited successfully before its dependents launch;
- skip dependents whose prerequisites fail to start, surfacing a clear dependency error instead of allowing a partial boot;
- stop running dependents automatically when an upstream service crashes, preventing workloads from running against unhealthy backends.

For example:

```yaml
version: "1"
services:
  database:
    command: "postgres -D /var/lib/postgres"

  web:
    command: "python app.py"
    depends_on:
      - database
```

If `database` fails to come up, `web` will remain stopped and log the dependency failure until the database is healthy again.

#### Rolling Deployments

Services can opt into rolling restarts so existing instances keep serving traffic until replacements are healthy. Add a `deployment` block to configure the behavior:

```yaml
version: "1"
services:
  api:
    command: "./target/release/api"
    restart_policy: "always"
    deployment:
      strategy: "rolling"          # default is "immediate"
      pre_start: "cargo build --release"
      health_check:
        url: "http://localhost:8080/health"
        timeout: "60s"
        retries: 5
      grace_period: "5s"
```

- `strategy` — set to `rolling` to enable the zero-downtime workflow, or omit to keep the traditional stop/start cycle.
- `pre_start` — optional shell command executed before the new instance launches (perfect for build or migrate steps).
- `health_check` — optional HTTP probe the replacement must pass before traffic flips; configure timeout and retry budget per service.
- `grace_period` — optional delay to keep the old instance alive after the new one passes health checks, giving load balancers time to rebalance.

If any rolling step fails, systemg restores the original instance and surfaces the error so unhealthy builds never replace running services.

#### Cron Scheduling

Services can be configured to run on a cron schedule for short-lived, recurring tasks. Cron jobs are managed by the supervisor and run independently of regular services:

```yaml
version: "1"
services:
  backup:
    command: "sh backup-script.sh"
    cron:
      expression: "0 0 * * * *"  # Run every hour at minute 0
      timezone: "America/New_York"  # Optional, defaults to system timezone
```

Key features:
- **Standard cron syntax** - Uses 6-field cron expressions (second, minute, hour, day, month, day of week).
- **Overlap detection** - If a cron job is scheduled to run while a previous execution is still running, the new execution is skipped and an error is logged.
- **Execution history** - The last 10 executions are tracked with their start time, completion time, and status.
- **Service separation** - A service cannot have both a `command` for continuous running and a `cron` configuration; cron is opt-in via the `cron` field.

Note: Cron jobs do not support restart policies, as they are designed to be short-lived tasks that complete and exit.

### Dynamic Process Spawning

Services can dynamically spawn child processes at runtime with full tracking and resource limits:

```yaml
version: "1"
services:
  orchestrator:
    command: "python orchestrator.py"
    spawn:
      mode: "dynamic"
      limits:
        children: 100           # Direct children limit
        depth: 3                # Maximum spawn tree depth
        descendants: 500        # Total across all levels
        total_memory: "2GB"     # Shared by entire tree
        termination_policy: "cascade"  # Clean up all children on exit
```

Key features:
- **Process Tree Tracking** - Child processes inherit parent's monitoring and logging
- **Resource Limits** - Configurable limits prevent fork bombs and runaway spawning
- **Rate Limiting** - Built-in protection (10 spawns/second) prevents abuse
- **Flexible Spawning** - Support for both traditional workers and LLM-powered agents
- **TTL Support** - Automatic cleanup of temporary processes after specified duration

From your application code, spawn children using the CLI:

```python
# Python example
subprocess.run(["sysg", "spawn", "--name", "worker_1", "--ttl", "3600", "--", "python", "worker.py"])
```

#### Additional Commands

The `sysg` command-line interface provides several subcommands for managing processes:

**Stop** - Stop the process manager or a specific service:

```sh
# Stop the supervisor and every managed service
$ sysg stop

# Stop a specific service
$ sysg stop --service myapp
```

**Restart** - Restart the process manager:

```sh
# Restart all services managed by the supervisor
$ sysg restart

# Restart a specific service
$ sysg restart -s myapp

# Restart with a different configuration
$ sysg restart --config new-config.yaml
```

**Status** - Check the status of running services:

```sh
# Show status of all services (uses default systemg.yaml)
$ sysg status

# Show status with a specific configuration file
$ sysg status --config myapp.yaml

# Show status of a specific service
$ sysg status --service webserver

# Show all services including orphaned state
$ sysg status --all

# Refresh status every 5 seconds (also accepts 1s, 2m, 1second)
$ sysg status --stream 5
```

**Inspect** - Inspect a service or cron unit in detail:

```sh
# Inspect a specific service or cron unit by name or hash
$ sysg inspect myservice

# Show metrics in JSON format
$ sysg inspect myservice --json

# Refresh continuously using a rolling 2-minute metrics window
$ sysg inspect myservice --stream 2m

# Render output without ANSI coloring
$ sysg inspect myservice --no-color
```

**Logs** - View logs for a specific service:

```sh
# View the last 50 lines of stderr logs (default)
$ sysg logs

# View logs for a specific service
$ sysg logs --service api-service

# View a custom number of log lines
$ sysg logs --service database --lines 100

# Refresh log snapshots every 2 seconds (respects --lines)
$ sysg logs --service api-service --lines 100 --stream 2

# View specific log type (stdout, stderr, or supervisor)
$ sysg logs --service myservice --kind stderr
```

**Spawn** - Dynamically spawn child processes from parent services:

```sh
# Spawn a worker process (parent must have spawn.mode: dynamic)
$ sysg spawn --name worker_1 -- python worker.py
$ 12345  # Returns the child PID

# Spawn with time-to-live for automatic cleanup
$ sysg spawn --name temp_worker --ttl 3600 -- ./process.sh

# Spawn with custom log level for debugging
$ sysg spawn --name debug_worker --log-level debug -- python worker.py

# Spawn with environment variables (pass them directly)
$ KEY=value PORT=8080 sysg spawn --name worker -- node app.js

# Spawn an autonomous agent (pass env vars for provider/goal)
$ LLM_PROVIDER=claude AGENT_GOAL="Optimize database queries" sysg spawn --name optimizer -- python3 agent.py
```

**Log Level** - Override logging verbosity:

```sh
# Override logging verbosity for the current run (works with every subcommand; names or 0-5)
$ sysg start --log-level debug
$ sysg start --log-level 4
```

### Comparison

| Feature            | systemg       | systemd         | Supervisor   | Docker Compose  |
|--------------------|-----------------|-----------------|-----------------|------------------|
| **Focus**          | Program Composition | System Management | Process Supervision | Container Orchestration |
| **Abstractions**   | Systems of Programs | Individual Units | Individual Processes | Container Services |
| **Configuration**  | Declarative YAML | Unit Files | INI Files | YAML |
| **Dependencies**   | Topological with Health | Complex Chains | Manual Priority | Service Links |
| **Deployment**     | Built-in Rolling | External Tools | Manual | Recreate/Rolling |
| **Runtime Deps**   | None | DBus, Journal | Python | Docker Daemon |
| **OS Integration** | Optional | Required (PID 1) | None | Container Runtime |

---

## Development

### Testing

To run the test suite:

```sh
# Run all tests
$ cargo test

# Run specific test
$ cargo test test_service_lifecycle
```

## Build from Source

To build systemg from source:

```sh
# Clone the repository
$ git clone https://github.com/ra0x3/systemg.git
$ cd systemg

# Build the project
$ cargo build --release

# The binary will be available at target/release/sysg
```

### Contributing

Contributions to systemg are welcome! Please see the [CONTRIBUTING.md](CONTRIBUTING.md) file for guidelines.
