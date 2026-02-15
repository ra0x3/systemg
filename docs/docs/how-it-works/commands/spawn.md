---
sidebar_position: 8
title: spawn
---

# spawn

Dynamically create child processes from parent services.

```sh
$ sysg spawn --name worker_1 -- python worker.py
```

## Options

| Option | Description |
|--------|------------|
| `--name` | Required. Unique identifier for spawned process |
| `--ttl` | Time-to-live in seconds (optional) |
| `--parent-pid` | Parent process ID (defaults to caller's parent PID if not specified) |
| `--sys` | Opt into privileged system mode. Requires running as root |
| `--drop-privileges` | Drop privileges after performing privileged setup |
| `--log-level` | Override the logging verbosity for the spawned process |

## Examples

### Spawn a worker process

```sh
$ sysg spawn --name worker_1 -- python worker.py
$ 12345  # Returns PID
```

### Spawn with time limit

Process automatically terminates after 1 hour:

```sh
$ sysg spawn --name temp_worker \
  --ttl 3600 \
  -- ./process.sh
```

### Spawn with parent PID tracking

```sh
$ sysg spawn --name api_worker \
  --parent-pid 12345 \
  -- node worker.js
```

## Requirements

Parent service must be configured with:

```yaml
services:
  orchestrator:
    command: "python orchestrator.py"
    spawn:
      mode: dynamic
      limit: 10
```

The parent can then spawn up to 10 child processes dynamically.

## See also

- [Spawn configuration](../configuration#spawn-settings)
- [`status`](status) - View spawned processes
