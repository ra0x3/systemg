---
sidebar_position: 9
title: Start vs Spawn
---

# Start vs Spawn

Two ways to create processes in systemg.

## Quick comparison

| | `start` | `spawn` |
|---|---|---|
| **Configuration** | Required in YAML | No config needed |
| **Lifecycle** | Persistent service | Ephemeral process |
| **Restarts** | Automatic | Never |
| **Use case** | Core services | Dynamic workers |

## `start` - Launch configured services

Services defined in `systemg.yaml`:

```yaml
services:
  web:
    command: "python app.py"
    restart_policy: always
  database:
    command: "postgres"
    depends_on: []
```

Run with:

```sh
sysg start
```

Services run continuously, restart on failure, and stop together.

## `spawn` - Create dynamic processes

Parent service enables spawning:

```yaml
services:
  orchestrator:
    command: "python orchestrator.py"
    spawn:
      mode: dynamic
      limit: 10
```

Parent creates children at runtime:

```sh
sysg spawn --name worker_1 -- python job.py
```

Children terminate on completion or TTL expiry. No automatic restarts.

## When to use each

### Use `start` for:
- Web servers
- Databases
- Message queues
- Background workers
- Any persistent service

### Use `spawn` for:
- Job processing
- Batch operations
- Temporary workers
- Dynamic scaling
- One-off tasks

## Example: Job queue

```yaml
services:
  queue:
    command: "redis-server"
    restart_policy: always

  scheduler:
    command: "python scheduler.py"
    depends_on: ["queue"]
    spawn:
      mode: dynamic
      limit: 50
```

The scheduler reads from queue and spawns workers:

```python
# scheduler.py
import subprocess
while job := queue.pop():
    subprocess.run(["sysg", "spawn", "--name", f"job_{job.id}",
                    "--ttl", "3600", "--", "python", "worker.py", job.id])
```

## See also

- [`start`](start) - Launch services
- [`spawn`](spawn) - Create processes
- [Configuration](../configuration) - Service definitions