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
| `--lines` | Number of lines to show (default: 50) |
| `--follow` | Stream new logs in real-time |
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

### Follow logs in real-time

```sh
sysg logs --follow
```

Press `Ctrl+C` to stop following.

### Show more history

```sh
sysg logs --lines 200 api
```

## Log files

Logs are stored in `~/.local/share/systemg/logs/`:
- `{service}_stdout.log` - Standard output
- `{service}_stderr.log` - Standard error

## See also

- [`status`](status) - Check service health
- [`inspect`](inspect) - View detailed metrics