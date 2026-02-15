---
sidebar_position: 5
title: status
---

# status

Check the health of running services.

```sh
$ sysg status
```

## Options

| Option | Description |
|--------|------------|
| `--config` | Path to configuration file |
| `--service` | Show specific service details |
| `--all` | Show all services including orphaned state (services not in current config) |
| `--sys` | Opt into privileged system mode. Requires running as root |
| `--drop-privileges` | Drop privileges after performing privileged setup |
| `--json` | Emit machine-readable JSON output instead of a table |
| `--no-color` | Disable ANSI colors in output |
| `--watch` | Continuously refresh status at the provided interval in seconds |
| `--log-level` | Set verbosity (`debug`, `info`, `warn`, `error`) |

## Examples

### View all services

```sh
$ sysg status
```

```
SERVICE    STATUS    PID     UPTIME  CPU%  MEM%
postgres   running   14823   2h3m    0.1   2.3
redis      running   14824   2h3m    0.0   0.8
api        running   14826   2h3m    1.2   4.5
worker     running   14827   2h3m    0.3   3.1
```

### View specific service

```sh
$ sysg status --service api
```

```
Service: api
Status: running
PID: 14826
Uptime: 2h3m
CPU: 1.2%
Memory: 4.5%
Command: python app.py
```

## Output fields

- **SERVICE** - Service name from configuration
- **STATUS** - `running`, `stopped`, or `failed`
- **PID** - Process ID
- **UPTIME** - Time since service started
- **CPU%** - Current CPU usage
- **MEM%** - Current memory usage

## See also

- [`logs`](logs) - View service output
- [`restart`](restart) - Restart services
