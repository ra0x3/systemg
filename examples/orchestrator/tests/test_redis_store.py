from datetime import datetime, timedelta, timezone

from orchestrator.models import DagModel, TaskEdge, TaskNode, TaskState, TaskStatus


def test_write_and_list_ready_tasks(redis_store):
    """RedisStore should surface ready tasks by dependency status."""
    dag = DagModel(
        goal_id="goal-demo",
        nodes=[
            TaskNode(id="task-1", title="Step 1", priority=1),
            TaskNode(id="task-2", title="Step 2", priority=0),
        ],
        edges=[TaskEdge(source="task-1", target="task-2")],
    )
    redis_store.write_dag(dag)
    ready = redis_store.list_ready_tasks("goal-demo")
    assert ready == ["task-1"], ready

    state = redis_store.get_task_state("task-1")
    redis_store.update_task_state("task-1", state.model_copy(update={"status": TaskStatus.DONE}))
    ready_after = redis_store.list_ready_tasks("goal-demo")
    assert ready_after[0] == "task-2"


def test_lock_cycle(redis_store):
    """RedisStore lock lifecycle should acquire, renew, and release."""
    dag = DagModel(goal_id="g", nodes=[TaskNode(id="t", title="T", priority=0)], edges=[])
    redis_store.write_dag(dag)
    assert redis_store.acquire_lock("t", "agent-a", timedelta(seconds=5)) is True
    assert redis_store.acquire_lock("t", "agent-b", timedelta(seconds=5)) is False
    assert redis_store.lock_owner("t") == "agent-a"
    assert redis_store.renew_lock("t", "agent-a", timedelta(seconds=5)) is True
    redis_store.release_lock("t", "agent-a")
    assert redis_store.lock_owner("t") is None


def test_list_ready_tasks_recovers_stale_running(redis_store):
    """Stale running tasks should be reset to ready for retry."""
    dag = DagModel(
        goal_id="goal-recover", nodes=[TaskNode(id="t1", title="T1", priority=1)], edges=[]
    )
    redis_store.write_dag(dag)
    redis_store.update_task_state(
        "t1",
        TaskState(
            status=TaskStatus.RUNNING,
            owner="agent-crashed",
            lease_expires=datetime.now(timezone.utc) - timedelta(seconds=1),
        ),
    )

    ready = redis_store.list_ready_tasks("goal-recover")
    assert ready == ["t1"]

    recovered_state = redis_store.get_task_state("t1")
    assert recovered_state is not None
    assert recovered_state.status is TaskStatus.READY
    assert recovered_state.owner is None
    assert recovered_state.lease_expires is None


def test_goal_spending_cap_roundtrip(redis_store):
    """Goal spending-cap deadline should round-trip while active."""
    goal_id = "goal-cap"
    until = datetime.now(timezone.utc) + timedelta(seconds=45)
    redis_store.set_goal_spending_cap_until(goal_id, until)

    fetched = redis_store.get_goal_spending_cap_until(goal_id)
    assert fetched is not None
    assert fetched >= datetime.now(timezone.utc)
