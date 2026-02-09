from orchestrator.llm import StubLLMClient
from orchestrator.models import DagModel, TaskNode
from orchestrator.orchestrator import Orchestrator, SpawnAdapter, SpawnHandle


class RecordingSpawner(SpawnAdapter):
    def __init__(self):
        self.calls = []

    def spawn_agent(self, descriptor, *, redis_url: str, log_level: str) -> SpawnHandle:  # type: ignore[override]
        self.calls.append((descriptor.name, redis_url, log_level))
        return SpawnHandle(pid=1234, command=["sysg", "spawn"])


def test_orchestrator_creates_dag_and_spawns_agent(redis_store, assets_dir, tmp_path):
    instructions_src = assets_dir / "INSTRUCTIONS.md"
    instructions_path = tmp_path / "INSTRUCTIONS.md"
    instructions_path.write_text(instructions_src.read_text(), encoding="utf-8")

    # Copy auxiliary files referenced in instructions
    for subdir in ("instructions", "heartbeat"):
        src_dir = assets_dir / subdir
        dest_dir = tmp_path / subdir
        dest_dir.mkdir()
        for file in src_dir.iterdir():
            dest_dir.joinpath(file.name).write_text(file.read_text(), encoding="utf-8")

    # Prepare deterministic DAG blueprint
    nodes = [TaskNode(id="task-1", title="Step 1", priority=1)]
    dag = DagModel(goal_id="goal-demo", nodes=nodes, edges=[])
    llm = StubLLMClient(dag)
    spawner = RecordingSpawner()

    orchestrator = Orchestrator(
        instructions_path=instructions_path,
        redis_store=redis_store,
        redis_url="fakeredis://",
        llm_client=llm,
        spawn_adapter=spawner,
        poll_interval=0,
    )

    orchestrator.run(max_cycles=1)
    stored_dag = redis_store.read_dag("goal-demo")
    assert stored_dag is not None
    assert stored_dag.goal_id == "goal-demo"
    assert spawner.calls == [("agent-research", "fakeredis://", "INFO")]
