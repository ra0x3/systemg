---
sidebar_position: 8
title: spawn
---

## Overview

The `spawn` command dynamically creates child processes from parent services configured with `spawn.mode: dynamic`. It enables runtime process creation with full monitoring, logging, and resource limit enforcement.

## Usage

### Spawn a Traditional Worker Process

Spawn a child process with a specific command:

```sh
$ sysg spawn --name worker_1 -- python worker.py
12345  # Returns the child PID
```

### Spawn with Time-to-Live (TTL)

Create an ephemeral process that will be terminated after a specified duration:

```sh
$ sysg spawn --name temp_worker --ttl 3600 -- ./process.sh
```

### Spawn with Environment Variables

Pass environment variables to the spawned child:

```sh
$ sysg spawn --name worker --env KEY=value --env PORT=8080 -- node app.js
```

### Spawn an Autonomous Agent

Spawn an LLM-powered agent with a goal:

```sh
$ sysg spawn --name optimizer --provider claude --goal "Optimize database queries"
```

## Command Options

| Option | Description |
| --- | --- |
| `--name` | Name for the spawned child process (required) |
| `--ttl` | Time-to-live in seconds before automatic termination |
| `--env` | Environment variables in KEY=VALUE format (can be repeated) |
| `--provider` | LLM provider for autonomous agents (e.g., claude, openai) |
| `--goal` | Goal or objective for autonomous agent execution |
| `command...` | Command and arguments to execute (required unless --provider is specified) |

## How It Works

### Parent Service Requirements

Only services configured with `spawn.mode: dynamic` can spawn child processes:

```yaml
services:
  orchestrator:
    command: "python orchestrator.py"
    spawn:
      mode: "dynamic"
      limits:
        children: 100
        depth: 3
```

### Spawn Authorization Process

1. **Parent Identification**:
   - The spawn command captures the caller's process ID
   - Verifies the caller is a registered dynamic service
   - Checks the caller's current spawn depth in the tree

2. **Limit Enforcement**:
   - **Rate Limiting**: Maximum 10 spawns per second per parent
   - **Depth Check**: Ensures spawn depth doesn't exceed `max_depth`
   - **Children Limit**: Verifies parent hasn't exceeded `max_children`
   - **Descendant Cap**: Total descendants don't exceed `max_descendants`
   - **Memory Quota**: Combined memory usage within limits

3. **Authorization Failure**:
   - Returns an error if any limit would be exceeded
   - Prevents fork bombs and runaway spawning

### Process Creation

1. **Command Execution**:
   - Spawns the child process with the specified command
   - For agent spawning without a command, uses a default shell wrapper

2. **Environment Setup**:
   - Inherits parent's environment variables
   - Adds custom environment variables from `--env` flags
   - Sets special variables:
     - `SPAWN_DEPTH`: Current depth in the spawn tree
     - `SPAWN_PARENT_PID`: PID of the immediate parent
     - `LLM_PROVIDER`: Provider name for agents
     - `AGENT_GOAL`: Goal for autonomous agents

3. **Process Registration**:
   - Records parent-child relationship in PID file
   - Updates spawn tree tracking
   - Assigns monitoring and logging to the child

### Process Tree Tracking

Spawned children form hierarchical trees:

```
orchestrator (PID: 1000, depth: 0)
├─ worker_1 (PID: 1001, depth: 1)
│   └─ sub_worker_1 (PID: 1002, depth: 2)
├─ worker_2 (PID: 1003, depth: 1)
└─ agent_1 (PID: 1004, depth: 1)
    └─ sub_agent_1 (PID: 1005, depth: 2)
```

### Resource Management

1. **Inheritance**:
   - Children inherit parent's resource limits
   - Share the parent's total memory quota
   - Subject to the same termination policy

2. **Termination Policies**:
   - **cascade** (default): All descendants terminated when parent dies
   - **orphan**: Children continue running when parent terminates
   - **reparent**: Children reassigned to init process

3. **TTL Cleanup**:
   - Processes with TTL are automatically terminated after expiration
   - Cleanup includes all descendants if termination_policy is cascade

## Agent Spawning

### LLM Provider Integration

When spawning with `--provider`, the child process receives:
- `LLM_PROVIDER` environment variable with the provider name
- `AGENT_GOAL` environment variable with the specified goal
- The agent process decides how to use these hints

### Provider Examples

```sh
# Claude-powered optimization agent
$ sysg spawn --name optimizer --provider claude \
    --goal "Analyze and optimize slow database queries"

# OpenAI code generation agent
$ sysg spawn --name coder --provider openai \
    --goal "Generate unit tests for the API module"

# Custom provider with specific command
$ sysg spawn --name analyzer --provider custom \
    --goal "Security audit" -- python custom_agent.py
```

### Heterogeneous Agent Trees

Agents can spawn sub-agents with different providers:

```sh
# Parent spawns a Claude agent
$ sysg spawn --name researcher --provider claude \
    --goal "Research optimization strategies"

# Claude agent spawns an OpenAI sub-agent (from within its code)
$ sysg spawn --name implementer --provider openai \
    --goal "Implement the optimization"
```

## Safety Mechanisms

### Fork Bomb Prevention

Multiple layers prevent runaway spawning:

1. **Rate Limiting**: 10 spawns/second per parent
2. **Depth Limits**: Default max_depth of 3 levels
3. **Descendant Caps**: Default max_descendants of 500
4. **Memory Quotas**: Shared memory limits for entire trees

### Recursive Spawn Control

Children can spawn their own children if:
- Parent service has `spawn.mode: dynamic`
- Current depth < max_depth
- Total descendants < max_descendants
- Rate limits not exceeded

### Error Handling

Spawn failures return specific error messages:

```sh
$ sysg spawn --name worker -- ./app
Error: Spawn limit exceeded: Maximum depth reached

$ sysg spawn --name worker -- ./app
Error: Spawn limit exceeded: Maximum direct children reached

$ sysg spawn --name worker -- ./app
Error: Spawn authorization failed: No spawn tree found for process
```

## Integration Examples

### Python Orchestrator

```python
import subprocess
import json

def spawn_worker(task_id, task_data):
    """Spawn a worker process for a specific task"""
    result = subprocess.run([
        "sysg", "spawn",
        "--name", f"worker_{task_id}",
        "--ttl", "3600",
        "--env", f"TASK_DATA={json.dumps(task_data)}",
        "--", "python", "worker.py"
    ], capture_output=True, text=True)

    if result.returncode == 0:
        return int(result.stdout.strip())  # Child PID
    else:
        raise Exception(f"Spawn failed: {result.stderr}")
```

### Go Agent Controller

```go
package main

import (
    "fmt"
    "os/exec"
    "strings"
)

func SpawnAgent(name, provider, goal string) (int, error) {
    cmd := exec.Command("sysg", "spawn",
        "--name", name,
        "--provider", provider,
        "--goal", goal)

    output, err := cmd.Output()
    if err != nil {
        return 0, err
    }

    var pid int
    fmt.Sscanf(strings.TrimSpace(string(output)), "%d", &pid)
    return pid, nil
}
```

### Shell Script Batch Spawning

```bash
#!/bin/bash

# Spawn multiple workers for parallel processing
for i in {1..10}; do
    sysg spawn --name "worker_$i" --ttl 1800 -- \
        python process_chunk.py --chunk $i &
done

wait  # Wait for all background spawns to complete
```

## Best Practices

1. **Name Uniqueness**: Use descriptive, unique names for spawned processes
2. **TTL Usage**: Set TTL for temporary tasks to ensure cleanup
3. **Resource Planning**: Configure appropriate spawn.limits based on workload
4. **Error Handling**: Check spawn return codes and handle failures
5. **Monitoring**: Use `sysg status` to monitor spawn trees
6. **Cleanup**: Ensure proper termination_policy for your use case

## See Also

- [Configuration: Dynamic Process Spawning](/docs/configuration#dynamic-process-spawning)
- [Status Command](/docs/usage/status) - View process trees
- [Stop Command](/docs/usage/stop) - Terminate processes