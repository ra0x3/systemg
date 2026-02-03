# Meta-Agents Example

## Overview
This walkthrough demonstrates recursive agent spawning that shares a single
`INSTRUCTIONS.md`. Each Claude invocation identifies itself by name, follows the
matching instructions, and hands its result back up the chain using
`sysg spawn`.

## Prerequisites
- `sysg` built from this repository and available on your `PATH`
- An LLM CLI (defaults to `claude`, but any compatible tool works)

## Run the Example
From the repository root:

```bash
cd examples/meta-agents

export LLM_BIN="<your-llm-cli>"
export LLM_ARGS="<non-interactive-flags-including-prompt-flag>"

# Example values
# export LLM_BIN="claude"
# export LLM_ARGS="--dangerously-skip-permissions -p"

sysg start
```

In a separate terminal, launch the root agent with the same environment. The
prompt tells the model which CLI is driving it, reminds it to follow
`INSTRUCTIONS.md`, and to ignore the alternate instruction files.

```bash
"$LLM_BIN" $LLM_ARGS "You are root_agent running under the Claude CLI. Read \
INSTRUCTIONS.md and follow the instructions for your name. Do not ask for \
approval. Ignore any CLAUDE.md files. Ignore any AGENTS.md files."
```

Changing LLMs is as simple as swapping the exports. For Codex:

```bash
export LLM_BIN="codex"
export LLM_ARGS="exec --dangerously-bypass-approvals-and-sandbox"

"$LLM_BIN" $LLM_ARGS "You are root_agent running under the Codex CLI. Read \
INSTRUCTIONS.md and follow the instructions for your name. Do not ask for \
approval. Ignore any CLAUDE.md files. Ignore any AGENTS.md files."
```

`sysg` will manage the root agent and any dynamically spawned children until the
instruction chain completes.

## What Happens
1. `root_agent` logs the start of the multiplication chain and spawns `agent_1`.
2. `agent_1` spawns `agent_2` and waits for its output.
3. `agent_2` spawns `agent_3` and waits for its output.
4. `agent_3` writes `4` to `/tmp/meta_agents/agent_3.txt`.
5. `agent_2` reads `4`, multiplies by `3`, writes `12`, and logs the step.
6. `agent_1` reads `12`, multiplies by `2`, writes `24`, and logs the step.
7. `root_agent` reads `24`, writes `SUCCESS: Result is 24`, and logs completion.

## Observability
- `sysg status` — confirm spawned agents and their current state.
- `sysg logs <name>` — stream stdout/stderr for any agent (`root_agent`,
  `agent_1`, etc.).
- `/tmp/meta_agents/progress.log` — append-only log of each step in the chain.
- `/tmp/meta_agents/*.txt` — intermediate results (`agent_1.txt`, `agent_2.txt`,
  `agent_3.txt`) and the final `result.txt`.

## Expected Output
When the chain completes, you should see:

```bash
$ cat /tmp/meta_agents/result.txt
SUCCESS: Result is 24

$ ls /tmp/meta_agents
agent_1.txt
agent_2.txt
agent_3.txt
progress.log
result.txt
```

Example progress log snippet:

```
[root_agent] Starting multiplication chain
[root_agent] Spawned agent_1, waiting for result...
[agent_1] Started with multiplier 2
[agent_1] Spawned agent_2, waiting...
[agent_2] Started with multiplier 3
[agent_2] Spawned agent_3, waiting...
[agent_3] Started with multiplier 4 (base case)
[agent_3] Wrote base value 4
[agent_2] Got value 4, computed 3×4=12
[agent_1] Got value 12, computed 2×12=24
[root_agent] Got result: 24
```

## Key Features
- **Dynamic supervision**: `sysg` enforces spawn limits while managing every
  agent lifecycle.
- **Single instruction source**: All roles read the same `INSTRUCTIONS.md`.
- **Deterministic chaining**: Each agent multiplies the downstream value before
  handing results upstream.
- **Filesystem hand-off**: Shared files in `/tmp/meta_agents` coordinate state.

## Files
- `INSTRUCTIONS.md` — Name-specific instructions used by every agent.
- `systemg.yaml` — Supervisor configuration for the root agent and spawn limits.
