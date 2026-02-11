---
sidebar_position: 4
title: logs
---

# logs

View output from running services.

```sh
sysg logs
```

## Options

| Option | Description |
|--------|------------|
| `--config` | Path to configuration file |
| `--lines` | Number of lines to show (default: 50) |
| `--kind` | Kind of logs to show: stdout, stderr, or supervisor (default: stdout) |
| `--sys` | Opt into privileged system mode. Requires running as root |
| `--drop-privileges` | Drop privileges after performing privileged setup |
| `--log-level` | Set verbosity (`debug`, `info`, `warn`, `error`) |

## Examples

### View recent logs from all services

```sh
sysg logs
```

### View logs from specific service

```sh
sysg logs api
```

### View stderr logs

```sh
sysg logs --kind stderr api
```

### View supervisor logs

```sh
sysg logs --kind supervisor
```

### Show more history

```sh
sysg logs --lines 200 api
```

## Log files

Logs are stored in `~/.local/share/systemg/logs/`:
- `{service}_stdout.log` - Standard output
- `{service}_stderr.log` - Standard error (primary log stream)

> **Note:** systemg treats stderr as the primary log stream. Service logs written to stderr are given priority in the supervisor's log output.

## See also

- [`status`](status) - Check service health
- [`inspect`](inspect) - View detailed metrics