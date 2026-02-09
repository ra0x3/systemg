from datetime import timedelta

from orchestrator.models import DagModel, TaskEdge, TaskNode, TaskStatus


def test_write_and_list_ready_tasks(redis_store):
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
    dag = DagModel(goal_id="g", nodes=[TaskNode(id="t", title="T", priority=0)], edges=[])
    redis_store.write_dag(dag)
    assert redis_store.acquire_lock("t", "agent-a", timedelta(seconds=5)) is True
    assert redis_store.acquire_lock("t", "agent-b", timedelta(seconds=5)) is False
    assert redis_store.lock_owner("t") == "agent-a"
    assert redis_store.renew_lock("t", "agent-a", timedelta(seconds=5)) is True
    redis_store.release_lock("t", "agent-a")
    assert redis_store.lock_owner("t") is None
