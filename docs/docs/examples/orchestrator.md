---
sidebar_position: 50
title: Orchestrator
---

# Orchestrator

A sophisticated multi-agent orchestration system that demonstrates LLM-driven task decomposition, distributed coordination through Redis, and hierarchical agent management using systemg's process supervision capabilities.

## Overview

The orchestrator example showcases how `systemg` can manage a complex multi-agent system where:
- An orchestrator process supervises multiple autonomous agents
- Agents coordinate through Redis-backed task queues and state management
- LLMs (via Claude CLI) drive task planning, execution, and summarization
- Markdown files serve as the human-facing control plane for instructions and heartbeats

## Architecture

The system operates with two distinct roles that communicate through Redis:

```
Orchestrator (Supervisor)
  ├── Watches INSTRUCTIONS.md for agent declarations
  ├── Generates and validates DAGs via LLM
  ├── Spawns agents via sysg spawn
  └── Monitors agent heartbeats

Agent (Worker)
  ├── Claims tasks from Redis queue
  ├── Executes tasks via LLM planning
  ├── Updates progress in Redis
  └── Responds to heartbeat directives
```

### Key Innovation: Redis-Based Coordination

Unlike simpler agent systems, the orchestrator uses Redis as a distributed state store:

```python
# Immutable DAG structure (orchestrator writes)
dag:<goal>:nodes    # Task metadata
dag:<goal>:deps     # Dependency graph

# Mutable task state (agents update)
task:<id>           # Status, owner, progress
task:<id>:lock      # Distributed locks with TTL

# Agent registry
agent:<name>:registered    # PID, capabilities
agent:<name>:heartbeat    # Liveness tracking
agent:<name>:memory       # Crash recovery state
```

## Features Demonstrated

### 1. LLM-Driven Task Decomposition
- Orchestrator prompts LLM to generate task DAGs from high-level instructions
- Structured JSON schemas ensure valid graph topology
- Automatic cycle detection and validation
- Priority-based task scheduling

### 2. Distributed Task Coordination
- **Lease-based locking**: Agents acquire time-limited locks on tasks
- **State transitions**: READY → RUNNING → DONE/FAILED
- **Dependency tracking**: Tasks become ready as dependencies complete
- **Crash recovery**: Agent memory snapshots enable resumption

### 3. Heartbeat-Based Control
- Live control through markdown heartbeat files
- Supported directives:
  - `PAUSE`/`RESUME`: Control agent execution
  - `REPARSE`: Reload instructions
  - `DROP-TASK <id>`: Release task locks
  - `ELEVATE <id> <priority>`: Reprioritize tasks
  - `FLUSH-MEMORY`: Clear agent context

### 4. Multi-Agent Spawning
- Orchestrator spawns agents with `sysg spawn`
- Each agent runs with dedicated:
  - Instruction file (`instructions/<agent>.md`)
  - Heartbeat file (`heartbeat/<agent>.md`)
  - Goal assignment (`--goal-id`)
  - Redis connection

## Project Structure

```
examples/orchestrator/
├── orchestrator/           # Core Python modules
│   ├── orchestrator.py    # Supervisor process
│   ├── runtime.py         # Agent runtime
│   ├── cache.py           # Redis abstractions
│   ├── llm.py             # LLM client interface
│   ├── models.py          # Pydantic data models
│   ├── instructions.py    # Markdown parsing
│   ├── heartbeat.py       # Directive processing
│   └── memory.py          # Agent context management
├── agent.py               # CLI entrypoint
├── systemg.yaml           # SystemG configuration
├── INSTRUCTIONS.md        # Master control file
├── SPEC.md               # Detailed specification
├── instructions/          # Per-agent instructions
├── heartbeat/            # Per-agent heartbeats
└── tests/                # Pytest test suite
```

## Configuration

The `systemg.yaml` configures the orchestrator service:

```yaml
version: "1"

services:
  orchestrator:
    command: |
      ./.venv/bin/python3 agent.py
        --role orchestrator
        --instructions ./INSTRUCTIONS.md
        --redis-url redis://localhost:6379
        --log-level INFO
    env:
      mode: "load"
      file: ".env"
```

The `INSTRUCTIONS.md` file declares agents:

```markdown
## Agents

### research_agent
- Goal: goal-001
- Heartbeat: ./heartbeat/research.md
- Log Level: INFO

### analysis_agent
- Goal: goal-001
- Heartbeat: ./heartbeat/analysis.md
- Log Level: DEBUG
```

## Key Concepts

### 1. Goal Model
Each goal maps to exactly one DAG with tasks as nodes:

```json
{
  "goal_id": "goal-001",
  "nodes": [
    {
      "id": "task-001",
      "title": "Research competitors",
      "priority": 10,
      "expected_artifacts": ["competitors.json"]
    }
  ],
  "edges": [
    {"source": "task-001", "target": "task-002"}
  ]
}
```

### 2. Task Lifecycle
Tasks progress through well-defined states:

```
BLOCKED → READY → RUNNING → DONE/FAILED
         ↑                      ↓
         └──────(retry)─────────┘
```

### 3. LLM Integration Pipeline
Four distinct LLM interactions drive the system:

1. **Goal Decomposition** (Orchestrator)
   - Instructions → Structured DAG

2. **Task Selection** (Agent)
   - Ready tasks + Memory → Task choice

3. **Task Execution** (Agent)
   - Task + Context → Concrete steps

4. **Task Summarization** (Agent)
   - Execution results → Progress update

## Running the Example

1. **Setup environment:**
   ```bash
   cd examples/orchestrator
   python3 -m venv .venv
   ./.venv/bin/pip install -r requirements.txt
   ```

2. **Start Redis:**
   ```bash
   redis-server
   ```

3. **Launch orchestrator:**
   ```bash
   sysg start
   ```

4. **Monitor execution:**
   ```bash
   # View orchestrator logs
   sysg logs orchestrator -f

   # Check Redis state
   redis-cli KEYS "*"

   # View task states
   redis-cli HGETALL "task:task-001"

   # Monitor agent heartbeats
   redis-cli GET "agent:research_agent:heartbeat"
   ```

5. **Control agents via heartbeats:**
   ```bash
   # Pause an agent
   echo "PAUSE" >> ./heartbeat/research.md

   # Drop a task
   echo "DROP-TASK task-001" >> ./heartbeat/research.md

   # Resume execution
   echo "RESUME" >> ./heartbeat/research.md
   ```

## Testing

The example includes comprehensive pytest coverage:

```bash
# Run all tests
uv run pytest examples/orchestrator/tests -v

# Test categories:
# - Unit tests with fakeredis
# - DAG validation and cycle detection
# - Lock acquisition and renewal
# - Instruction parsing
# - Heartbeat directive processing
# - Integration tests with mock LLMs
# - Live Claude CLI tests (when available)
```

## Observability

Monitor the orchestration through multiple channels:

### 1. SystemG Commands
- `sysg status`: View orchestrator and agent states
- `sysg logs <service> -f`: Follow service output
- `sysg inspect orchestrator`: Detailed metrics

### 2. Redis Monitoring
```bash
# Watch key changes
redis-cli --scan --pattern "*"

# Monitor task distribution
redis-cli HGETALL "dag:goal-001:nodes"

# Check agent registration
redis-cli HGETALL "agent:research_agent:registered"
```

### 3. Application Logs
The system produces structured logs with context:
```
2024-03-18 10:15:23 INFO orchestrator: DAG created for goal goal-001: 5 nodes, 4 edges
2024-03-18 10:15:24 INFO runtime: [research_agent] Acquired lock for task task-001
2024-03-18 10:15:30 INFO runtime: [research_agent] Task task-001 completed
```

## Key Takeaways

1. **Distributed Coordination**: Redis enables multiple agents to coordinate without direct communication, providing fault tolerance and scalability

2. **LLM-Driven Autonomy**: Agents make intelligent decisions about task selection and execution through structured LLM interactions

3. **Operational Control**: Markdown-based heartbeat files provide human operators with real-time control over agent behavior

4. **Crash Recovery**: Memory snapshots and lease-based locking ensure the system recovers gracefully from agent failures

5. **Observable State**: All system state lives in Redis, making debugging and monitoring straightforward

## Implementation Phases

The orchestrator was developed in four phases:

1. **Phase 1**: Core modules with typed interfaces and mock LLMs
2. **Phase 2**: Redis integration with DAG storage and locking
3. **Phase 3**: Orchestrator spawning and multi-agent coordination
4. **Phase 4**: Production LLM client and comprehensive telemetry

Each phase included pytest coverage before proceeding.

## Further Reading

- [SystemG Configuration Reference](/docs/configuration)
- [State Management](/docs/state)
- [Meta-Agents Example](/docs/examples/meta-agents)
- [How SystemG Works](/docs/how-it-works)