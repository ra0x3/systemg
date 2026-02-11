---
sidebar_position: 8
title: spawn
---

# spawn

Dynamically create child processes from parent services.

```sh
sysg spawn --name worker_1 -- python worker.py
```

## Options

| Option | Description |
|--------|------------|
| `--name` | Unique identifier for spawned process |
| `--ttl` | Time-to-live in seconds |
| `--env` | Environment variables (`KEY=value`) |
| `--log-level` | Set verbosity (`debug`, `info`, `warn`, `error`) |

## Examples

### Spawn a worker process

```sh
sysg spawn --name worker_1 -- python worker.py
12345  # Returns PID
```

### Spawn with time limit

Process automatically terminates after 1 hour:

```sh
sysg spawn --name temp_worker \
  --ttl 3600 \
  -- ./process.sh
```

### Spawn with environment

```sh
sysg spawn --name api_worker \
  --env API_KEY=secret \
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