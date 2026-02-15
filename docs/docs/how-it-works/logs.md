---
sidebar_position: 4
title: Logs
---

# Logs

systemg's internal operational logs, separate from service output.

## What's logged

- Service lifecycle (start, stop, restart, crash)
- Cron job execution
- Configuration changes
- Supervisor events

## Location

`~/.local/share/systemg/logs/supervisor.log` (user mode)
`/var/log/systemg/supervisor.log` (system mode)

## View logs

```bash
# Last 50 lines
$ sysg logs --kind supervisor

# Follow in real-time
$ tail -f ~/.local/share/systemg/logs/supervisor.log

# Search for events
$ grep "Starting service" ~/.local/share/systemg/logs/supervisor.log
```

## Log levels

Set verbosity when starting:

```bash
$ sysg start --log-level debug
```

Levels: `trace` (5), `debug` (4), `info` (3), `warn` (2), `error` (1), `off` (0)

## Log format

```
2025-12-02T10:30:15.123456Z  INFO systemg::daemon: Starting service: api
```

Format: `[TIMESTAMP] [LEVEL] [MODULE]: [MESSAGE]`

## Common messages

### Service events
```
Starting service: api
Service api exited with status 0
Restarting service: api (attempt 1/5)
Service api crashed: exit code 1
```

### Cron events
```
Running cron job 'backup'
Cron job 'backup' completed successfully
Cron job 'backup' exited with non-zero status
```

### Supervisor events
```
systemg supervisor listening on "/path/to/socket"
Supervisor shutting down
Reloading configuration from "/path/to/config"
```

## Log rotation

### Using logrotate

Create `/etc/logrotate.d/systemg`:

```
~/.local/share/systemg/logs/supervisor.log {
    daily
    rotate 7
    compress
    missingok
}
```

### Manual rotation

```bash
$ mv ~/.local/share/systemg/logs/supervisor.log ~/.local/share/systemg/logs/supervisor.log.old
# systemg creates new file automatically
```

## Troubleshooting

**Log file missing**
- Check systemg has started
- Verify directory exists

**Empty logs**
- Try `--log-level debug`
- Check services are running

**Large log files**
- Implement rotation
- Reduce log level

## See also

- [`logs`](commands/logs) - View service output
- [`status`](commands/status) - Check service health
