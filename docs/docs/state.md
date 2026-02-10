---
sidebar_position: 5
title: State
---

# State

State in `~/.local/share/systemg/` (or `/var/lib/systemg` with `--sys`).

## Files

```
~/.local/share/systemg/
├── sysg.pid           # Supervisor PID
├── control.sock       # IPC socket
├── config_hint        # Last config path
├── pid.json          # Service PIDs
├── state.json        # Service states
└── logs/             # All logs
```

## Key Files

**`sysg.pid`** - Supervisor PID (plain text)

**`control.sock`** - Unix socket for CLI→supervisor IPC

**`config_hint`** - Saves config path for auto-discovery

**`pid.json`** - Service PID registry
```json
{"services": {"web": 67235, "db": 67236}}
```

**`state.json`** - Service metadata (status, restarts, exit codes)

**`logs/`** - Service stdout/stderr and supervisor logs

## Persistence

State survives supervisor restarts. Services continue running.

Clean shutdown removes `sysg.pid` and `control.sock`.

## Troubleshooting

Stale files after crash? Run `sysg purge`.

## Commands

```bash
cat ~/.local/share/systemg/pid.json    # View PIDs
sysg purge                             # Clean all state
```
