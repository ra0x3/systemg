---
sidebar_position: 50
title: Orchestrator
---

# Orchestrator

Multi-agent task execution with cache coordination.

## Overview

Orchestrator supervises agents that claim and execute tasks from a shared cache.

## Architecture

```
Orchestrator → Reads instructions/INSTRUCTIONS.md → Spawns agents → Creates DAGs
Agent → Claims task → Executes → Updates cache
```

Cache keys:
```
dag:<goal>:nodes    # Tasks
dag:<goal>:deps     # Dependencies
task:<id>           # Status
task:<id>:lock      # Locks
agent:<name>:*      # Registry
```

## Features

- **Tasks**: LLM generates DAG from instructions
- **Locking**: Time-limited task leases
- **States**: `BLOCKED → READY → RUNNING → DONE/FAILED`
- **Control**: Heartbeat files accept `PAUSE`, `RESUME`, `DROP-TASK`, etc.
- **Spawning**: Each agent gets instruction file, heartbeat, goal ID

## Files

```
systemg.yaml                    # Orchestrator service config
instructions/INSTRUCTIONS.md    # Agent declarations
instructions/*.md               # Per-agent instructions
instructions/heartbeat/*.md     # Control files
docs/*.md                       # Example specs
```

## Configuration

**systemg.yaml:**
```yaml
version: "1"
services:
  redis:
    command: "redis-server"
  orchestrator:
    command: >
      porki
      --role orchestrator
      --instructions instructions/INSTRUCTIONS.md
      --redis-url redis://127.0.0.1:6379
    depends_on: ["redis"]
    spawn:
      mode: dynamic
      limit: 10
```

**instructions/INSTRUCTIONS.md:**
```markdown
## Agents
### research_agent
- Goal: goal-001
- Heartbeat: ./instructions/heartbeat/research.md
```

## DAG Format

```json
{
  "goal_id": "goal-001",
  "nodes": [{"id": "task-001", "title": "Research", "priority": 10}],
  "edges": [{"source": "task-001", "target": "task-002"}]
}
```

## Usage

```bash
# Setup
$ cd examples/orchestrator
$ pip install porki

# Run
$ redis-server
$ sysg start

# Monitor
$ sysg logs --service orchestrator
$ redis-cli KEYS "*"

# Control
$ echo "PAUSE" >> ./instructions/heartbeat/research.md
```

## Validation

Use `sysg status`, `sysg logs --service orchestrator`, and Redis keys to validate
runtime behavior. Package-level runtime tests execute in the `porki` repository.

## Links

- [Configuration](/docs/how-it-works/configuration)
- [How It Works](/docs/how-it-works)
