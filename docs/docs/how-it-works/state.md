---
sidebar_position: 3
title: State
---

# State

Runtime files systemg uses to track services.

## Location

`~/.local/share/systemg/` (user mode)
`/var/lib/systemg/` (system mode with `--sys`)

## Structure

```
~/.local/share/systemg/
├── sysg.pid           # Supervisor PID
├── control.sock       # Unix socket for IPC
├── config_hint        # Last config path
├── pid.json          # Service → PID mapping
├── state.json        # Service metadata
└── logs/             # Service output
```

## Key files

### pid.json

Maps services to process IDs:

```json
{"services": {"web": 67235, "db": 67236}}
```

### config_hint

Stores the last config path so you don't need `--config` on every command.

### state.json

Tracks service status, restart counts, and exit codes.

## Persistence

State survives supervisor restarts. Services keep running even if the supervisor crashes.

Clean shutdown removes `sysg.pid` and `control.sock`. Stale files after crash? Run `sysg purge`.

## See also

- [`purge`](commands/purge) - Clear all state
- [How It Works](./) - Architecture overview
