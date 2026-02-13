# Systemg Agent Orchestration Spec

## Background
- `main.py` currently sketches an agent runtime that never fully executes because decorators short-circuit, subprocesses misuse `sysg` spawns, and no orchestrator supervises agents.
- The target architecture promotes an always-on orchestrator that spawns agents via `sysg spawn`, keeps authoritative state in Redis, and coordinates LLM-driven work decomposition.
- Markdown (`INSTRUCTIONS.md`, `heartbeat.md`) remains the human-facing control plane, mirroring practices in other agent platforms such as OpenClaw.

## Roles & Responsibilities
- **Orchestrator (`--role orchestrator`)**
  - Watches `examples/orchestrator/INSTRUCTIONS.md` for agent declarations and global directives.
  - Validates/updates the goal DAG in Redis by prompting the LLM and enforcing schema invariants.
  - Spawns/retiring agents exclusively via `sysg spawn`, piping role-specific CLI arguments.
  - Serves as validator: rejects malformed graphs, resolves conflicts, and guards against cycles.
- **Agent (`--role agent`)**
  - Reads dedicated instruction and heartbeat paths supplied by the orchestrator.
  - Maintains lease-based locks in Redis for tasks it works on.
  - Runs the three LLM phases (plan, execute, summarize) and updates Redis with progress snapshots.
  - Streams logs to stdout/stderr so systemg can capture them.

## Naming Conventions
- Attributes referencing filesystem paths carry a `_path` suffix (e.g., `instructions_path`, `heartbeat_path`).
- Methods that poll or refresh state use descriptive verb prefixes (`reload_instructions`, `poll_heartbeat`, `renew_leases`).
- Local helpers follow snake_case while Redis keys use colon-delimited segments (`task:<id>`).
- This prevents attribute/method collisions like the current `self.instructions` bug in the sketch.

## Agent Execution Model
- Agents run a single deterministic main loop under systemg supervision.
- Periodic duties (instruction reload, heartbeat check, lease renewal) execute via tick counters inside that loop; no background threads by default.
- Optional syntactic sugar (`@periodic(interval=...)`) compiles to tick registration rather than spawning threads, keeping scheduling visible and debuggable.
- Heartbeat-triggered directives can short-circuit the loop (e.g., pause) but the process remains single-threaded for observability.

## Instruction & Heartbeat Files
- `INSTRUCTIONS.md` (per master control):
  - Contains agent stanzas (name, specialization, heartbeat path, polling interval, log level).
  - Lists high-level goals and policy notes the orchestrator uses when prompting the LLM.
  - Orchestrator diff-detects changes and applies them without restarting.
- Per-agent files:
  - `instructions/<agent_name>.md` – individual directives; agents reload when told via heartbeat.
  - `heartbeat/<agent_name>.md` – live overrides (pause, re-plan, drop lease). Agents poll on short cadence.

## Goal Model
- Each goal tracked by the orchestrator maps to exactly one DAG rooted at `goal:<goal_id>`.
- Multiple goals may exist simultaneously, but an agent declares a single active `--goal-id` at launch.
- Cross-goal dependencies are explicitly disallowed in v1; sequencing between goals happens through orchestrator policy.
- Goals progress `draft → active → complete/failed`, and that status lives in `goal:<id>` alongside orchestration metadata.

## Redis Data Model
- **Immutable DAG structure** (written/validated by orchestrator):
  - `dag:<goal>:nodes` hash: node metadata (`id`, `title`, `priority`, `expected_artifacts`).
  - `dag:<goal>:edges` list/set: adjacency list for dependencies.
- **Mutable task state** (updated by agents):
  - `task:<id>` hash: `status` (`ready|claimed|running|blocked|done|failed`), `owner`, `lease_expires`, `progress`, `artifacts`.
  - `task:<id>:lock` implemented via `SET ... NX PX` for leases.
- **Goal tracker**: `goal:<id>` hash keeps aggregate status, completion timestamp, and orchestrator metadata.
- All writes pass through Lua/transaction helpers to keep node+state consistent and to enforce DAG invariants.

## Spawn Taxonomy
- `sysg spawn`: launches processes with systemg supervision. For long-lived agents, orchestrator passes `--name agent-<name>` plus agent-specific flags and records the returned PID.
- Short-lived commands (LLM tools, auxiliary scripts) can be run with `sysg spawn --ttl <seconds>` for automatic cleanup, or directly via subprocess for immediate execution with stdout capture.
- Every spawn includes `--parent-pid <orchestrator_pid>` to anchor the process tree.
- Handles expose `.pid`, `.wait(timeout=...)`, `.stdout_text()`, and `.stderr_text()` so the orchestrator can gather results or enforce timeouts.
- Agents use subprocess sparingly—primarily for tool commands—while LLM calls go through an in-process client.

## LLM Interaction Pipeline
1. **Goal decomposition (orchestrator)**
   - Prompt: instructions + existing Redis state → structured DAG JSON following canonical schema.
   - Validator: JSON schema + cycle detection. Failures trigger retry or manual intervention.
   - Result stored atomically in Redis.
2. **Task selection (agent)**
   - Inputs: agent memory, goal context, list of `ready` nodes sorted by priority.
   - Output: chosen node id + rationale. Validator ensures node is eligible before lock acquisition.
3. **Task execution (agent)**
   - Agent supplies plan to LLM to generate concrete actions/artifacts.
   - Execution may call tools; progress captured for summary.
4. **Task summarization (agent)**
   - LLM distills execution log into structured progress update matching schema (`status`, `outputs`, `next_steps`).
   - Agent writes progress to Redis and releases lock if complete.

## Memory Model
- Each agent keeps a bounded local `Memory` deque for recency-sensitive LLM context (instructions snapshots, last actions).
- After finishing a task—or on periodic checkpoints—the agent snapshots this memory into `agent:<name>:memory` in Redis for crash recovery.
- On startup the agent hydrates local memory from Redis, falling back to empty if no snapshot exists.
- The orchestrator and other agents treat Redis task state as the source of truth; they never depend on another agent's memory snapshot.

## LLM Invocation Strategy
- Agents and orchestrator call the local `claude` CLI (optionally via `sysg spawn --ttl`), capturing stdout and enforcing JSON schemas for DAG generation, task selection, and execution summaries.
- The CLI client surfaces non-zero exit codes and rejects malformed JSON payloads so orchestrator/agents can escalate.
- Operators configure the CLI path with `--claude-cli` (default `claude`) and may supply extra arguments or `--claude-use-sysg` to delegate process management to systemg.
- Tool executions that manipulate the filesystem or external services can use `sysg spawn --ttl` for managed execution with automatic cleanup.

## Agent Lifecycle & Discovery
- Orchestrator issues `sysg spawn` for each agent declaration and expects registration within `REGISTRATION_TIMEOUT` seconds.
- Agent boot sequence:
  - Hydrate memory from Redis.
  - Write `agent:<name>:registered` hash (`pid`, `capabilities`, `timestamp`).
  - Start heartbeat loop updating `agent:<name>:heartbeat` with TTL via `PEXPIRE`.
- Orchestrator confirms registration, monitors heartbeat cadence, and reschedules the agent if the TTL lapses.
- On graceful shutdown the agent writes `agent:<name>:deregistered` and releases any held locks.

## Heartbeat File Format
- Plaintext, line-oriented directives processed in order on each poll.
- Grammar: `DIRECTIVE [ARGS...]`; directives are case-insensitive, lines starting with `#` are ignored.
- Supported directives: `PAUSE`, `RESUME`, `REPARSE`, `DROP-TASK <task_id>`, `ELEVATE <task_id> <priority>`, `FLUSH-MEMORY`.
- Heartbeat files contain persistent instructions that agents should follow repeatedly - they are never modified by agents.

## Concurrency & Recovery
- Agents acquire locks with leases (`PX ttl`) and renew via heartbeat thread.
- If a lease lapses, orchestrator or other agents detect and reclaim the node.
- Heartbeat loop also interprets directives in `heartbeat.md` (e.g., `reparse`, `drop-task`, `elevate`), publishing corresponding updates to Redis.
- Orchestrator monitors agent heartbeats; missing heartbeats trigger agent respawn.

## Error Handling & Recovery
- **Transient** errors (network hiccups, LLM timeouts) retry automatically with exponential backoff (max three attempts) before escalation.
- **Structural** faults (schema violation, DAG cycle, invalid heartbeat directive) are logged, pushed to `task:<id>:errors`, and mark the task `failed`; orchestrator receives a pub/sub alert on `alerts`.
- **Fatal** events (agent crash, unrecoverable exception) are observed by systemg; orchestrator clears leases, writes `agent:<name>:errors`, and respawns after cooldown.
- Error records include agent name, task id, correlation id, and timestamp for post-mortem analysis.

## CLI & Logging
- Shared entry point `python main.py --role {orchestrator,agent}`.
- Common flags: `--instructions`, `--heartbeat`, `--redis-url`, `--log-level`, `--agent-name`, `--goal-id`.
- Claude configuration flags: `--claude-cli`, `--claude-extra-arg` (repeatable), `--claude-use-sysg` (invoke through systemg).
- Logging via `logging.basicConfig(level=...)` with structured context (agent, task, lease id).
- `uv` used as the Python workspace orchestrator for dependency management and task automation (e.g., `uv pip install`, `uv run pytest`).

## Testing Strategy (Pytest)
- **Unit tests** (use `fakeredis`):
  - DAG validator ensures acyclic graphs, priority ordering, schema compliance.
  - Lock manager acquires/releases leases, handles renewals and expirations.
  - Instruction parser extracts agent descriptors and catches malformed entries.
- **Happy path integration**:
  - Orchestrator seeds Redis with LLM-mocked DAG, spawns mocked agents, verifies tasks complete in order.
  - Agent claims task, runs mock LLM responses, updates progress, releases lock.
- **Unhappy paths**:
  - Malformed LLM output rejected; orchestrator logs error and retries.
  - Lock loss mid-task forces agent to relinquish progress and requeue node.
  - Heartbeat directive `pause` halts agent loop; `resume` restarts.
- **Assets** under `examples/orchestrator/tests/`:
  - Sample `INSTRUCTIONS.md` / heartbeat files for fixtures.
  - Mock LLM responses (JSON) for DAG creation and task execution.
- Single command: `uv run pytest examples/orchestrator/tests -q` exercises happy/unhappy paths with mocks.
- Live Claude CLI tests run whenever the `claude` executable is available (and `sysg` when `--claude-use-sysg` is enabled).

## Implementation Phases
- **Phase 1**: Refactor the sketch into typed modules with local memory, mock LLM client, single-agent loop, and CLI/logging scaffolding.
- **Phase 2**: Introduce Redis-backed DAG/state, lease mechanics, and heartbeat directives while retaining mocked LLMs.
- **Phase 3**: Add orchestrator process, instruction parsing, multi-agent spawning via systemg, and Redis-based coordination.
- **Phase 4**: Integrate production LLM client, tool execution pathways, and comprehensive telemetry/alerting.
- Each phase lands with pytest coverage (happy + unhappy paths) and documentation updates before moving forward.

## Schema Appendix
- **DAG Node (`dag:<goal>:nodes`)**
  ```json
  {
    "id": "task-001",
    "title": "Enumerate seed repositories",
    "priority": 10,
    "expected_artifacts": ["repos.json"],
    "metadata": {"owner_hint": "research"}
  }
  ```
- **Task State (`task:<id>`)**
  ```json
  {
    "status": "running",
    "owner": "agent-research",
    "lease_expires": "2024-03-18T18:12:00Z",
    "progress": "Fetched 12 repos; awaiting classification",
    "artifacts": ["s3://bucket/repos.json"],
    "last_error": null
  }
  ```
- **LLM Task Selection Response**
  ```json
  {
    "selected_task_id": "task-002",
    "justification": "Prerequisites complete; next highest priority",
    "confidence": 0.82
  }
  ```
- **LLM Task Summary Response**
  ```json
  {
    "status": "done",
    "outputs": ["/tmp/report.md"],
    "notes": "Validated against acceptance criteria",
    "follow_ups": ["task-005"]
  }
  ```

## Operational Considerations
- Orchestrator should run as a systemg-managed service with restart policy.
- Redis TTLs tuned to give agents enough time to renew leases while recovering quickly from crashes.
- Prompt templates versioned alongside code; schema changes require orchestrator/agent updates in lockstep.
- Markdown edits should follow review process to prevent partial updates; optional future enhancement is syncing them into Redis for transactional updates.
