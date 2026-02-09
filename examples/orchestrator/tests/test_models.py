from datetime import datetime, timezone
from pathlib import Path

import pytest

from orchestrator.models import AgentDescriptor, DagModel, TaskEdge, TaskNode, TaskState, TaskStatus


def test_dag_model_validates_edges():
    node_a = TaskNode(id="task-a", title="A", priority=1)
    node_b = TaskNode(id="task-b", title="B", priority=0)
    dag = DagModel(
        goal_id="goal-demo",
        nodes=[node_a, node_b],
        edges=[TaskEdge(source="task-a", target="task-b")],
    )
    assert dag.dependencies_for("task-b") == ["task-a"]

    with pytest.raises(ValueError):
        DagModel(
            goal_id="goal-demo", nodes=[node_a], edges=[TaskEdge(source="task-b", target="task-a")]
        )


def test_task_state_transitions():
    state = TaskState(status=TaskStatus.READY)
    running = state.as_running(owner="agent-1", lease_expires=datetime.now(timezone.utc))
    assert running.status is TaskStatus.RUNNING
    done = running.as_done(progress="complete", artifacts=["foo.txt"])
    assert done.status is TaskStatus.DONE
    failed = running.as_failed("boom")
    assert failed.status is TaskStatus.FAILED


def test_agent_descriptor_cname():
    descriptor = AgentDescriptor(
        name="test-agent",
        goal_id="goal-123",
        instructions_path=Path("/tmp/instructions.txt"),
        heartbeat_path=Path("/tmp/heartbeat.txt"),
    )
    assert descriptor.cname() == "test-agent:goal-123"

    descriptor2 = AgentDescriptor(
        name="agent-alpha",
        goal_id="task-xyz",
        instructions_path=Path("/tmp/instructions.txt"),
        heartbeat_path=Path("/tmp/heartbeat.txt"),
        log_level="DEBUG",
    )
    assert descriptor2.cname() == "agent-alpha:task-xyz"
