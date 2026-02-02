# Meta-Agents Example

## Overview
- Demonstrates systemg's dynamic spawning using a single Python agent (`agent.py`).
- Agents read instruction files (root vs. recursive) and multiply an ordered list bottom-up.
- Each agent writes its result to `/tmp/meta_agents/agent_{depth}_output.txt`; the root aggregates into `root_agent_output.txt`.

## Execution Flow
1. Root agent parses `X=[…]`, persists `chain_config.json`, and spawns `agent_1`.
2. Recursive agents descend until the base case, then return products while unwinding.
3. The root waits for `agent_1_output.txt`, verifies the final product, and records a summary.

## Running
```bash
# Start the chain under the supervisor
sysg start --config systemg.yaml --daemonize

# Inspect state
sysg status

# Stop when finished
sysg stop
```

Quick local run without the supervisor:
```bash
AGENT_GOAL="INSTRUCTIONS:ROOT_INSTRUCTIONS.md - Compute chain with X=[2,3,4]" python3 agent.py
```

## Files
- `agent.py` – root/recursive logic with dynamic spawning
- `ROOT_INSTRUCTIONS.md` / `RECURSIVE_INSTRUCTIONS.md` – instruction prompts consumed by the agent
- `systemg.yaml` – service definition for `sysg start`
