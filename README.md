# systemg

![CI](https://github.com/ra0x3/systemg/actions/workflows/ci.yaml/badge.svg)

<div display="flex" align-items="center">
    <img src="https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white" />
    <img src="https://img.shields.io/badge/ts--node-3178C6?style=for-the-badge&logo=ts-node&logoColor=white" />
    <img src="https://img.shields.io/badge/Vite-B73BFE?style=for-the-badge&logo=vite&logoColor=FFD62E" />
    <img src="https://img.shields.io/badge/mac%20os-000000?style=for-the-badge&logo=apple&logoColor=white" />
    <img src="https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" />
    <img src="https://img.shields.io/badge/ChatGPT-74aa9c?style=for-the-badge&logo=openai&logoColor=white" />
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

# Systemg - A Lightweight Process Manager

Systemg is a **simple, fast, and dependency-free process manager** written in Rust.
It aims to provide **a minimal alternative to systemd** and other heavyweight service managers, focusing on **ease of use**, **clarity**, and **performance**.

---

## Getting Started

### How It Works

Curious about the architecture? Read [How Systemg Works](docs/docs/how-it-works.md) for a deep dive into userspace vs. kernel-space behavior, socket activation, and runtime helpers.

### Installation

Install the system binary:

```sh
$ curl --proto '=https' --tlsv1.2 -fsSL https://sh.sysg.dev/ | sh
```

Install systemg using cargo:

```sh
$ cargo install sysg
```

Or download the pre-built binary from the releases page.

For system deployments, `scripts/install-systemg.sh` installs `/usr/bin/sysg`, provisions `/etc/systemg`, `/var/lib/systemg`, `/var/log/systemg`, and drops sample logrotate + systemd assets for socket activation. Review and adapt it to match your distribution policies before running. Pair it with `examples/system-mode.yaml` and check the new `docs/docs/security.md` guide for hardening best practices.

### Running a Basic Start Command

Start the process manager with the default configuration:

```sh
# Start with default configuration file (systemg.yaml)
$ sysg start

# Start with a specific configuration file
$ sysg start --config systemg.yaml

# Start the long-lived supervisor (persists after you log out)
$ sysg start --config systemg.yaml --daemonize
```

When the supervisor is running it remains active in the background, holding service processes in the same process group so commands like `sysg stop`, `sysg restart`, `sysg status`, and `sysg logs` can coordinate them even after you disconnect from the shell that started them.

---

## Why systemg

Traditional process managers like **systemd** are complex, heavy, and introduce unnecessary dependencies.
Systemg offers a **lightweight**, **configuration-driven** solution that's **easy to set up and maintain**.

### Features

- **Declarative YAML Configuration** - Define services, dependencies, and restart policies easily.
- **Automatic Process Monitoring** - Restart crashed services based on custom policies.
- **Dependency-Aware Startup** - Honour `depends_on` chains, skip unhealthy dependencies, and cascade stop dependents on failure.
- **Environment Variable Support** - Load variables from `.env` files and per-service configurations.
- **Rolling Deployments** - Orchestrate zero-downtime restarts with pre-start commands, health probes, and grace periods.
- **Lifecycle Webhooks** - Trigger outbound notifications or remediation scripts on start/stop/restart outcomes with per-hook timeouts. See [Webhooks documentation](docs/docs/webhooks.md).
- **Cron Scheduling** - Run short-lived, recurring tasks on a cron schedule with overlap detection and execution history.
- **Minimal & Fast** - Built with Rust, designed for performance and low resource usage.
- **No Root Required** - Unlike systemd, it doesn't take over PID 1.

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
```

**Inspect** - Inspect a service or cron unit in detail:

```sh
# Inspect a specific service or cron unit by name or hash
$ sysg inspect myservice

# Show metrics in JSON format
$ sysg inspect myservice --json

# View only recent metrics (last 6 hours)
$ sysg inspect myservice --since 21600

# Display metrics in table format instead of chart
$ sysg inspect myservice --table

# Live tail mode - continuously updates the chart with real-time data
$ sysg inspect myservice --tail

# Live tail with custom time window (default: 5 seconds, max: 60 seconds)
$ sysg inspect myservice --tail --tail-window 10
```

**Logs** - View logs for a specific service:

```sh
# View the last 50 lines of stdout logs (default)
$ sysg logs

# View logs for a specific service
$ sysg logs api-service

# View a custom number of log lines
$ sysg logs database --lines 100

# View specific log type (stdout, stderr, or supervisor)
$ sysg logs myservice --kind stderr
```

**Log Level** - Override logging verbosity:

```sh
# Override logging verbosity for the current run (works with every subcommand; names or 0-5)
$ sysg start --log-level debug
$ sysg start --log-level 4
```

### Comparison

| Feature            | Systemg       | systemd         | Supervisor   | Docker Compose  |
|--------------------|-----------------|-----------------|-----------------|------------------|
| **Lightweight**    | ✓             | ✗ (Heavy)       | ✗ (Python)   | ✗ (Containers)  |
| **No Dependencies**| ✓             | ✗ (DBus, etc.)  | ✗ (Python)   | ✗ (Docker)      |
| **Simple Config**  | YAML          | Complex Units   | INI          | YAML            |
| **Process Monitoring** | ✓        | ✓               | ✓            | ✓               |
| **PID 1 Required?**| ✗             | ✓               | ✗            | ✗               |
| **Handles Dependencies?** | ✓    | ✓               | ✗            | ✓               |

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
