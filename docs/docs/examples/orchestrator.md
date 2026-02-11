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
Orchestrator → Reads INSTRUCTIONS.md → Spawns agents → Creates DAGs
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
agent.py            # Entry point
orchestrator/*.py   # Core modules
systemg.yaml       # Config
INSTRUCTIONS.md    # Agent declarations
instructions/*.md  # Per-agent instructions
heartbeat/*.md     # Control files
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
      ./.venv/bin/python3 agent.py
      --role orchestrator
    depends_on: ["redis"]
    spawn:
      mode: dynamic
      limit: 10
```

**INSTRUCTIONS.md:**
```markdown
## Agents
### research_agent
- Goal: goal-001
- Heartbeat: ./heartbeat/research.md
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
cd examples/orchestrator
python3 -m venv .venv
./.venv/bin/pip install -r requirements.txt

# Run
redis-server
sysg start

# Monitor
sysg logs orchestrator -f
redis-cli KEYS "*"

# Control
echo "PAUSE" >> ./heartbeat/research.md
```

## Testing

```bash
uv run pytest examples/orchestrator/tests -v
```

## Links

- [Configuration](/docs/how-it-works/configuration)
- [How It Works](/docs/how-it-works)