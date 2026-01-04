---
sidebar_position: 3
title: How It Works
---

# How Systemg Works

This page walks through systemg's architecture from the perspective of an operator who needs a deep understanding of how services are orchestrated, how state is maintained, and how privileged features integrate with the operating system.

## High-Level Overview

Systemg is a single binary containing:

- **CLI front-end (`sysg`)** – Parses user commands, configures logging, and forwards requests to the supervisor via a Unix socket when running in daemon mode.
- **Supervisor runtime** – Manages long-lived processes, service state, cron jobs, and IPC.
- **Daemon module** – Encapsulates service spawning, PID/state tracking, restart policies, hooks, and logs.
- **Runtime helpers** – Provide consistent paths for state, logs, and configuration in both userspace and system mode.

Most environments run systemg unprivileged: the supervisor lives entirely in the invoking user's HOME directory and never touches kernel primitives beyond process creation. Privileged mode is strictly opt-in and layers additional responsibilities (capabilities, cgroups, namespaces) without changing the userspace-first defaults.

## Userspace Mode

Userspace mode covers the standard workflow (`sysg start`, `sysg stop`, `sysg status`) without root access.

### Configuration Loading

1. **Parsing** – `configuration.rs` deserialises YAML into strongly typed structures, merging global and per-service environment blocks.
2. **Validation** – Dependency graphs are topologically sorted; invalid references raise errors before any process is started.
3. **Environment** – `.env` files and inline variables are resolved per service, with HOME-relative paths expanded based on the configuration's project directory.

### Supervisor Loop

1. **State Directories** – Runtime paths resolve to `~/.local/share/systemg` via `runtime::state_dir()`, with logs inside `logs/`.
2. **Process Launch** – `daemon::launch_attached_service` spawns each service under the current UID, capturing stdout/stderr via log writer threads.
3. **Monitoring** – The daemon maintains PID maps and state files (`pid.json`, `state.json`) and uses the monitor thread to react to exits, apply restart policies, and fire hooks.
4. **Cron Integration** – Cron jobs are tracked separately with overlap detection and execution history stored in `cron_state.json`.

### IPC & CLI

- The supervisor listens on `control.sock`; CLI commands send JSON payloads (`ControlCommand` / `ControlResponse`).
- `sysg logs` tails service files, while supervisor logs live in `logs/supervisor.log` in userspace.

## Kernel-Space (Privileged) Mode

Privileged mode (`sudo sysg --sys`) relocates state to system directories and enables optional kernel integrations.

### Directory Layout

| Asset            | Userspace Mode                      | Privileged Mode             |
| ---------------- | ----------------------------------- | --------------------------- |
| State            | `~/.local/share/systemg`            | `/var/lib/systemg`          |
| Supervisor Logs  | `~/.local/share/systemg/logs/...`   | `/var/log/systemg/...`      |
| Config Search    | Working directory + relative paths  | Adds `/etc/systemg`         |

### Privilege Context

For each service, `PrivilegeContext` aggregates:

- Target user/group/supplementary groups (resolved via `nix::unistd::{User, Group}`)
- Resource limits (`setrlimit`, `setpriority`, `sched_setaffinity`)
- Linux capabilities (implemented with the [`caps-rs`](https://github.com/lucab/caps-rs))
- Optional cgroup v2 settings and namespace toggles (`network`, `pid`, `mount`, `user`)

### Spawn Sequence

1. **Pre-exec Phase** – Inside the forked child, systemg applies namespace settings (using `unshare`), resource limits, CPU affinity, clears or sets capability sets, and prepares to drop privileges.
2. **User Switch** – `setgroups`, `setgid`, and `setuid` run last; capability keepcaps ensures requested capabilities can be reinstated after the UID change.
3. **Post-exec Phase** – Ambient capabilities and cgroup attachments are applied with the final PID. If no capabilities are configured, every set is cleared so the process runs with minimal privilege.

### Namespaces, Cgroups, and Activation

- Namespaces (network, PID, mount, user) are best-effort: unsupported kernels emit warnings.
- `limits.cgroup.*` writes to cgroup v2 controllers before the process joins the new group; the optional `root` field lets you test against a temporary mount.
- When running under systemd socket activation, systemg records inherited file descriptors via `LISTEN_FDS/FDNAMES` and exposes them through the runtime so services can bind without reopening sockets.

### Drop Privileges Flow

- `--drop-privileges` allows root-only setup, then runs services under the configured account.
- Capabilities default to empty; only explicitly listed values (e.g. `CAP_NET_BIND_SERVICE`) are retained after the drop.

## Further Reading

- [Configuration](configuration.md)
- [Privileged Mode](privileged-mode.md)
- [State](state.md)
- [Supervisor Logs](supervisor-logs.md)
- [Usage → Logs](usage/logs.md)
