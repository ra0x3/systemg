import shutil
from pathlib import Path

import pytest

from orchestrator.cache import RedisStore
from orchestrator.llm import ClaudeCLIClient
from orchestrator.models import DagModel, TaskStatus

GOAL_INSTRUCTIONS = """
- Prepare a concise onboarding checklist for new operators
- Draft a communication template for status updates
""".strip()


def _invoke_with_credit_guard(func):
    """Run callable and bubble expected runtime/value failures."""
    try:
        return func()
    except RuntimeError:
        raise
    except ValueError:
        raise


@pytest.mark.live_claude
def test_claude_creates_valid_dag(claude_client):
    """Live Claude call should return a valid DAG payload."""
    dag = _invoke_with_credit_guard(
        lambda: claude_client.create_goal_dag(GOAL_INSTRUCTIONS, goal_id="goal-live-dag")
    )
    assert isinstance(dag, DagModel)
    assert dag.goal_id == "goal-live-dag"
    assert dag.nodes, "Claude should emit at least one task node"

    node_ids = {node.id for node in dag.nodes}
    for node in dag.nodes:
        assert node.title, "Task node must include a title"
        assert node.priority >= 0
    for edge in dag.edges:
        assert edge.source in node_ids and edge.target in node_ids


@pytest.mark.live_claude
def test_claude_task_selection_and_execution(claude_client):
    """Live Claude call should select and execute a task shape correctly."""
    dag = _invoke_with_credit_guard(
        lambda: claude_client.create_goal_dag(GOAL_INSTRUCTIONS, goal_id="goal-live-select")
    )
    ready_nodes = list(dag.nodes)
    selection = _invoke_with_credit_guard(
        lambda: claude_client.select_next_task(
            ready_nodes,
            memory=[],
            goal_id=dag.goal_id,
            instructions=GOAL_INSTRUCTIONS,
        )
    )
    assert selection.selected_task_id is None or selection.selected_task_id in {
        node.id for node in ready_nodes
    }
    assert selection.justification
    assert 0.0 <= selection.confidence <= 1.0

    chosen = (
        ready_nodes[0]
        if selection.selected_task_id is None
        else next(node for node in ready_nodes if node.id == selection.selected_task_id)
    )

    execution = _invoke_with_credit_guard(
        lambda: claude_client.execute_task(
            chosen,
            goal_id=dag.goal_id,
            instructions=GOAL_INSTRUCTIONS,
            memory=[],
        )
    )
    assert execution.status in {"done", "blocked", "failed"}
    assert isinstance(execution.outputs, list)
    summary = _invoke_with_credit_guard(
        lambda: claude_client.summarize_task(
            chosen,
            execution,
            goal_id=dag.goal_id,
            instructions=GOAL_INSTRUCTIONS,
            memory=[],
        )
    )
    assert isinstance(summary, str) and summary.strip()


@pytest.mark.live_claude
def test_claude_hardcoded_json_contract_roundtrip(claude_client):
    """Live Claude call should honor hardcoded execute-task JSON schema."""
    schema = {
        "status": "done|failed|blocked",
        "outputs": ["artifact-path"],
        "notes": "execution notes",
        "follow_ups": ["task-id"],
    }
    prompt = claude_client._render_prompt(
        "Return this hardcoded prompt payload as strict JSON only.",
        goal_id="goal-live-json-contract",
        instructions="Echo a plausible task execution payload.",
        context={
            "prompt": "Set up project scaffolding and summarize what was created.",
            "task": {"id": "setup-001", "title": "Initialize project", "priority": 10},
            "memory": [],
        },
        schema=schema,
    )
    payload = _invoke_with_credit_guard(
        lambda: claude_client._invoke_json_with_retries(
            prompt,
            required_keys=set(schema.keys()),
            operation="hardcoded_json_contract_roundtrip",
            max_attempts=3,
        )
    )
    assert set(payload.keys()) == set(schema.keys())
    assert payload["status"] in {"done", "failed", "blocked"}
    assert isinstance(payload["outputs"], list)
    assert isinstance(payload["notes"], str)
    assert isinstance(payload["follow_ups"], list)


@pytest.mark.live_claude
def test_agent_live_cycle(redis_store: RedisStore, tmp_path: Path, claude_client):
    """Live agent loop should update task state and memory."""
    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text(GOAL_INSTRUCTIONS, encoding="utf-8")
    heartbeat_path.write_text("RESUME\n", encoding="utf-8")

    goal_id = "goal-live-agent"
    dag = _invoke_with_credit_guard(
        lambda: claude_client.create_goal_dag(GOAL_INSTRUCTIONS, goal_id=goal_id)
    )
    redis_store.write_dag(dag)

    from orchestrator.runtime import (
        AgentRuntime,
    )  # imported lazily to avoid circular import during test collection

    agent = AgentRuntime(
        agent_name="agent-live",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=claude_client,
        loop_interval=0,
    )

    _invoke_with_credit_guard(lambda: agent.run(max_cycles=2))

    ready_nodes = list(dag.nodes)
    node_ids = [node.id for node in ready_nodes]
    task_states = [redis_store.get_task_state(task_id) for task_id in node_ids]
    assert any(
        state
        and state.status
        in {
            TaskStatus.DEV_DONE,
            TaskStatus.QA_PASSED,
            TaskStatus.DONE,
            TaskStatus.RUNNING,
            TaskStatus.BLOCKED,
        }
        for state in task_states
    )
    for state in task_states:
        if state and state.status is TaskStatus.DONE:
            assert state.progress, "Completed task should record progress summary"
            break

    snapshot = redis_store.load_memory_snapshot("agent-live")
    assert snapshot, "Memory snapshot should not be empty"
    assert any(entry.strip() for entry in snapshot), "Memory snapshot should capture LLM output"


@pytest.mark.live_claude
def test_claude_sysg_spawn_roundtrip():
    """Live Claude invocation through sysg should return a DAG."""
    sysg = shutil.which("sysg")
    assert sysg, "sysg executable must be available for live tests"
    claude = shutil.which("claude")
    assert claude, "Claude CLI executable must be available"

    client = ClaudeCLIClient(executable=claude, use_sysg_spawn=True)
    dag = _invoke_with_credit_guard(
        lambda: client.create_goal_dag("- produce a single test artifact", goal_id="goal-sysg")
    )
    assert dag.goal_id == "goal-sysg"
    assert dag.nodes, "Claude should emit at least one node via sysg spawn"
