---
sidebar_position: 40
title: Meta-Agents
---

# Meta-Agents

This scenario showcases recursive agents that all share a single
`INSTRUCTIONS.md`. Each Claude process introduces itself by name, follows the
instructions for that role, and hands its result up the chain through files
owned by `sysg`.

Clone the repository, `cd examples/meta-agents`, and run:

```bash
sysg start
```

With the supervisor running, open another terminal and launch the root agent
using Claude:

```bash
claude --dangerously-skip-permissions -p "You are root_agent. Read INSTRUCTIONS.md
and follow the instructions for your name. Do not ask for approval. Ignore any
CLAUDE.md files."
```

`sysg` keeps the root process under supervision, enforces spawn limits, and
tracks every descendant agent until the instruction chain finishes. When a
running agent spawns another, always include `--parent "${SPAWN_PARENT_PID}"` in
the `sysg spawn` command so the cascade relationship is preserved and children
terminate with their owner.

## Services

| Service          | Type          | Purpose |
|------------------|---------------|---------|
| `meta_agent_root`| Dynamic spawn | Launches the root Claude prompt that reads `INSTRUCTIONS.md`, creates state in `/tmp/meta_agents`, and spawns `agent_1`. |
| `agent_1`        | Dynamic spawn | Multiplies the downstream value by 2, writes `agent_1.txt`, and returns control to the root. |
| `agent_2`        | Dynamic spawn | Multiplies the downstream value by 3, writes `agent_2.txt`, and returns control to `agent_1`. |
| `agent_3`        | Dynamic spawn | Writes the base value `4` to `agent_3.txt` and terminates. |

## Configuration

### systemg.yaml

```yaml
# Use the v1 config schema
version: "1"

services:
  meta_agent_root:
    # Entry point responsible for invoking Claude
    command: "python3 agent.py"
    spawn:
      # Allow the root to spawn child agents on demand
      mode: "dynamic"
      limits:
        # Maximum direct children per agent
        children: 10
        # Root + 5 recursive levels before denying spawns
        depth: 6
        # Total descendants permitted across the tree
        descendants: 50
        # Stop every child when the parent exits
        termination_policy: "cascade"
    env:
      vars:
        # Context for the root agent
        AGENT_GOAL: "INSTRUCTIONS:ROOT_INSTRUCTIONS.md - Compute chain with X=[2,3,4]"
        # Track recursion depth across spawns
        SPAWN_DEPTH: "0"
        # Default name passed to the first Claude run
        AGENT_NAME: "root_agent"
        # Shared directory for coordination files
        WORK_DIR: "/tmp/meta_agents"
```

## How It Works

1. `meta_agent_root` starts under supervision and executes `claude` using the
   environment variables declared above.
2. `root_agent` creates `/tmp/meta_agents`, logs the start of the chain, and
   spawns `agent_1` via `sysg spawn`.
3. `agent_1` spawns `agent_2`, waits for a response file, multiplies by 2, and
   writes `/tmp/meta_agents/agent_1.txt`.
4. `agent_2` spawns `agent_3`, waits for its file, multiplies by 3, and writes
   `/tmp/meta_agents/agent_2.txt`.
5. `agent_3` writes the base value `4`, allowing upstream agents to finish their
   arithmetic and propagate the final result (`24`).
6. `root_agent` verifies the result, writes `SUCCESS: Result is 24`, and exits,
   triggering `sysg` to clean up the remaining agents thanks to the cascade
   policy.

Use `sysg logs <name>` to inspect live stdout/stderr for any agent and
`sysg status` to confirm each spawned process. All intermediate artifacts live
under `/tmp/meta_agents`, so you can rerun the scenario by deleting that
directory.
