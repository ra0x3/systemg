---
sidebar_position: 3
title: How It Works
---

# How It Works

## Architecture

Single binary with:
- CLI frontend → Unix socket → Supervisor
- Daemon module (process management)
- State in `~/.local/share/systemg/`

## Userspace Mode (Default)

1. Parse YAML config, resolve dependencies
2. Launch processes, capture stdout/stderr
3. Monitor PIDs, apply restart policies
4. Track cron jobs in `cron_state.json`

IPC via `control.sock` for CLI commands.

## Privileged Mode

`sudo sysg --sys`:

| Path | Userspace | Privileged |
|------|-----------|------------|
| State | `~/.local/share/systemg` | `/var/lib/systemg` |
| Logs | `~/.local/share/systemg/logs` | `/var/log/systemg` |

Features:
- User/group switching
- Resource limits (ulimits, nice, CPU affinity)
- Linux capabilities
- Cgroups v2
- Namespaces (network, PID, mount)

Services drop to least privilege by default.
