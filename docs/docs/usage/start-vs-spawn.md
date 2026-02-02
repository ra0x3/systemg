---
sidebar_position: 9
title: Start vs Spawn
---

# When to Use `start` vs `spawn`

## Overview

Systemg provides two distinct commands for process creation: `start` and `spawn`. Understanding when to use each is crucial for building effective process management strategies.

## Quick Comparison

| Aspect | `sysg start` | `sysg spawn` |
| --- | --- | --- |
| **Purpose** | Launch configured services | Create dynamic child processes |
| **Configuration** | Required in `systemg.yaml` | No config needed (but parent needs `spawn.mode: dynamic`) |
| **Lifecycle** | Persistent, managed services | Ephemeral, on-demand processes |
| **Restart Policy** | Supports automatic restarts | No restart (can use TTL for auto-cleanup) |
| **Use Case** | Long-running services | Dynamic workers and tasks |
| **Invocation** | From command line | From within running services |

## `sysg start` - Service Orchestration

### Purpose
Launches and manages services defined in your `systemg.yaml` configuration file. These are typically long-running processes that form the backbone of your application.

### Characteristics
- **Pre-configured**: Services must be defined in the configuration file
- **Persistent**: Runs continuously until explicitly stopped
- **Managed**: Full monitoring, health checks, and restart policies
- **Dependencies**: Can define start order and dependencies between services
- **Logging**: Automatic log rotation and management
- **Hooks**: Supports pre/post start/stop hooks

### Example Configuration
```yaml
# systemg.yaml
services:
  web_server:
    command: "python app.py"
    restart_policy: always
    health_check:
      interval: 30s
      timeout: 5s

  database:
    command: "postgres"
    restart_policy: on_failure
    max_restarts: 3
```

### Usage
```bash
# Start all configured services
$ sysg start --daemonize

# Start a specific service
$ sysg start -s web_server

# Start in foreground (useful for containers)
$ sysg start
```

### When to Use `start`
- **Web servers and APIs** that need to run continuously
- **Database services** requiring persistent connections
- **Background workers** with fixed configurations
- **Message queue consumers** that process continuously
- **Any service** with known configuration at deployment time

## `sysg spawn` - Dynamic Process Creation

### Purpose
Dynamically creates child processes at runtime from within a parent service. Enables runtime scaling and on-demand task execution.

### Characteristics
- **Dynamic**: Created on-demand based on runtime conditions
- **Hierarchical**: Forms parent-child process trees
- **Limited**: Subject to spawn limits (depth, children, memory)
- **Ephemeral**: Can have time-to-live (TTL) for automatic cleanup
- **Inherited**: Inherits parent's monitoring and logging context

### Parent Configuration
```yaml
# Parent service must enable dynamic spawning
services:
  orchestrator:
    command: "python orchestrator.py"
    spawn:
      mode: dynamic
      limits:
        children: 100      # Max direct children
        depth: 3          # Max tree depth
        descendants: 500  # Total descendants
```

### Usage
```bash
# Spawn a worker process (called from within orchestrator)
$ sysg spawn --name worker_1 -- python worker.py

# Spawn with time-to-live
$ sysg spawn --name temp_worker --ttl 3600 -- ./process.sh

# Spawn with environment variables
$ DB_HOST=localhost sysg spawn --name db_worker -- python db_task.py
```

### When to Use `spawn`
- **Load-based scaling** when workers are created based on queue depth
- **Batch processing** for temporary jobs with defined lifespans
- **Agent trees** where agents spawn sub-agents for task decomposition
- **Map-reduce patterns** for parallel processing
- **Any scenario** where process count/type is determined at runtime

## Decision Matrix

### Use `start` When You Need:
✅ Service configuration known at deployment
✅ Automatic restart on failure
✅ Health monitoring and checks
✅ Service dependencies and ordering
✅ Persistent logging with rotation
✅ Init system or supervisor behavior

### Use `spawn` When You Need:
✅ Dynamic worker creation based on load
✅ Temporary processes with TTL
✅ Hierarchical process trees
✅ Runtime-determined process counts
✅ Agent-based architectures
✅ Fork-and-forget task execution

## Common Patterns

### Pattern 1: Fixed Services with Dynamic Workers
```yaml
services:
  # Fixed service (use start)
  task_queue:
    command: "python queue_manager.py"
    spawn:
      mode: dynamic
      limits:
        children: 50
```
```bash
# From within queue_manager.py when load increases:
os.system("sysg spawn --name worker --ttl 3600 -- python process_task.py")
```

### Pattern 2: Agent Hierarchies
```yaml
services:
  # Root agent (use start)
  root_agent:
    command: "python root_agent.py"
    spawn:
      mode: dynamic
      limits:
        depth: 5
        descendants: 1000
```
```bash
# Root agent spawns specialized sub-agents:
LLM_PROVIDER=claude sysg spawn --name analyzer -- python analyze.py
LLM_PROVIDER=openai sysg spawn --name coder -- python generate.py
```

### Pattern 3: Batch Processing
```yaml
services:
  # Batch scheduler (use start)
  scheduler:
    command: "python scheduler.py"
    spawn:
      mode: dynamic
```
```python
# scheduler.py spawns jobs:
for job_id in pending_jobs:
    subprocess.run([
        "sysg", "spawn",
        "--name", f"job_{job_id}",
        "--ttl", "7200",
        "--",
        "python", "process_job.py", str(job_id)
    ])
```

## Best Practices

### For `start`:
1. Define all known services in configuration
2. Use appropriate restart policies
3. Configure health checks for critical services
4. Set up service dependencies correctly
5. Use hooks for initialization/cleanup

### For `spawn`:
1. Always set spawn limits to prevent fork bombs
2. Use TTL for temporary tasks
3. Monitor spawn tree depth
4. Implement proper error handling in parent
5. Consider memory quotas for resource control

## Summary

**`sysg start`** is your service orchestrator - use it for the stable, configured foundation of your application.

**`sysg spawn`** is your dynamic task runner - use it for runtime scaling, temporary jobs, and hierarchical process trees.

Together, they provide a complete solution for both static service management and dynamic process orchestration.