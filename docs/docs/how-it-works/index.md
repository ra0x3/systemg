---
sidebar_position: 3
title: How It Works
---

# How It Works

systemg is a single binary that manages processes as a supervisor.

## Architecture

```
CLI → Unix Socket → Supervisor → Services
                 ↓
           State Files
```

When you run `sysg start`, the supervisor launches and monitors your services. Subsequent commands communicate with this supervisor via Unix socket.

## Process lifecycle

1. **Start**: Services launch in dependency order
2. **Monitor**: Supervisor tracks PIDs and health
3. **Restart**: Failed services restart per policy
4. **Stop**: Services terminate in reverse order

## State location

All runtime data lives in one place:

**User mode** (default):
- `~/.local/share/systemg/` - State files
- `~/.local/share/systemg/logs/` - Service logs

**System mode** (`sudo sysg --sys`):
- `/var/lib/systemg/` - State files
- `/var/log/systemg/` - Service logs

## Daemon vs foreground

**Foreground** (default):
- Services run as children of your shell
- `Ctrl+C` stops everything
- Good for development

**Daemon** (`--daemonize`):
- Supervisor runs in background
- Services survive terminal close
- Good for production

## Topics

- [Commands](commands) - CLI reference
- [Configuration](configuration) - Service definitions
- [Cron](cron) - Scheduled tasks
- [Webhooks](webhooks) - Lifecycle hooks
- [State](state) - Runtime files
- [Logs](logs) - Supervisor logs
- [Privileged Mode](privileged-mode) - System features
