from datetime import timedelta

from orchestrator.llm import StubLLMClient
from orchestrator.models import DagModel, TaskNode, TaskStatus
from orchestrator.runtime import AgentRuntime


def _prepare_dag(goal_id: str) -> DagModel:
    return DagModel(
        goal_id=goal_id,
        nodes=[TaskNode(id="task-1", title="Initial task", priority=1)],
        edges=[],
    )


def test_agent_completes_task(redis_store, tmp_path, assets_dir):
    instructions_src = assets_dir / "instructions" / "agent-research.md"
    heartbeat_src = assets_dir / "heartbeat" / "agent-research.md"
    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text(instructions_src.read_text(), encoding="utf-8")
    heartbeat_path.write_text(heartbeat_src.read_text(), encoding="utf-8")

    goal_id = "goal-demo"
    redis_store.write_dag(_prepare_dag(goal_id))

    agent = AgentRuntime(
        agent_name="agent-research",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=StubLLMClient(),
        loop_interval=0,
        lease_ttl=timedelta(seconds=5),
    )

    agent.run(max_cycles=3)
    state = redis_store.get_task_state("task-1")
    assert state is not None
    assert state.status is TaskStatus.DONE
    assert state.progress.startswith("Task task-1 completed")


def test_agent_pause_directive(redis_store, tmp_path, assets_dir):
    instructions_src = assets_dir / "instructions" / "agent-research.md"
    instructions_path = tmp_path / "instructions.md"
    instructions_path.write_text(instructions_src.read_text(), encoding="utf-8")
    heartbeat_path = tmp_path / "heartbeat.md"
    heartbeat_path.write_text("PAUSE\n", encoding="utf-8")

    goal_id = "goal-demo"
    redis_store.write_dag(_prepare_dag(goal_id))

    agent = AgentRuntime(
        agent_name="agent-research",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=StubLLMClient(),
        loop_interval=0,
    )

    agent.run(max_cycles=1)
    state = redis_store.get_task_state("task-1")
    assert state is not None
    assert state.status is not TaskStatus.DONE
