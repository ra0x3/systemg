---
sidebar_position: 6
title: purge
---

# purge

Remove all systemg state and start fresh.

```sh
$ sysg purge
```

**⚠️ Warning**: Permanently deletes all service history, logs, and runtime files.

## Options

| Option | Description |
|--------|------------|
| `--sys` | Opt into privileged system mode. Requires running as root |
| `--drop-privileges` | Drop privileges after performing privileged setup |
| `--log-level` | Set verbosity (`debug`, `info`, `warn`, `error`) |

## What gets removed

- Service status history
- Cron execution history
- All log files
- PID tracking files
- Supervisor state
- Socket files

## When to use

- After ungraceful shutdowns
- To clear corrupted state
- Before uninstalling systemg
- To free disk space

## Example

```sh
$ sysg purge
$ All systemg state has been purged
```

## See also

- [`stop`](stop) - Stop services without removing state
- [`start`](start) - Start fresh after purging
