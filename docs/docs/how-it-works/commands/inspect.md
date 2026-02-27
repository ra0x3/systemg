---
sidebar_position: 5
title: inspect
---

# inspect

View detailed metrics for a specific service.

```sh
$ sysg inspect myservice
```

## Options

| Option | Description |
|--------|------------|
| `--stream` | Continuously refresh output and render a rolling metrics window of the provided duration (e.g., `5`, `1s`, `2m`) |
| `--config` | Path to configuration file |
| `--json` | Emit machine-readable JSON output instead of a report |
| `--sys` | Opt into privileged system mode. Requires running as root |
| `--drop-privileges` | Drop privileges after performing privileged setup |
| `--no-color` | Disable ANSI colors in output |
| `--log-level` | Set verbosity (`debug`, `info`, `warn`, `error`) |

## Examples

### View service metrics

```sh
$ sysg inspect api
```

Shows CPU and memory usage chart:

```
CPU & Memory Usage - api (Last 5m)
100% ┤
     │     ╭─╮
 80% ┤    ╱  ╰╮
     │   ╱    ╰─╮
 60% ┤  ╱       ╰───╮
     │ ╱            ╰───────
 40% ┤╱
     │
 20% ┤━━━━━━━━━━━━━━━━━━━━━━  Memory
     │
  0% └────────────────────────
     0s              5m

CPU: 45.2% (current)  Memory: 23.1% (current)
```

### Stream with a longer rolling window

```sh
$ sysg inspect api --stream 24h
```

### Inspect by hash

Useful for cron jobs:

```sh
$ sysg inspect 3abad7ffa39c
```

## Metrics shown

- **CPU usage** - Percentage over time
- **Memory usage** - Percentage over time
- **Execution count** - For cron jobs
- **Average duration** - For completed processes
- **Success rate** - For cron jobs

## See also

- [`status`](status) - Quick service overview
- [`logs`](logs) - View service output
