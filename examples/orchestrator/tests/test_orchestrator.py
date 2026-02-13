from orchestrator.llm import StubLLMClient
from orchestrator.models import DagModel, TaskNode
from orchestrator.orchestrator import Orchestrator, SpawnAdapter, SpawnHandle


class RecordingSpawner(SpawnAdapter):
    def __init__(self):
        """Initialize call recorder."""
        self.calls = []

    def spawn_agent(self, descriptor, *, redis_url: str, log_level: str) -> SpawnHandle:  # type: ignore[override]
        """Record spawn arguments and return fixed handle."""
        self.calls.append((descriptor.name, redis_url, log_level))
        return SpawnHandle(pid=1234, command=["sysg", "spawn"])


def test_orchestrator_creates_dag_and_spawns_agent(redis_store, assets_dir, tmp_path):
    """Orchestrator should build DAG and spawn configured agent."""
    instructions_src = assets_dir / "INSTRUCTIONS.md"
    instructions_path = tmp_path / "INSTRUCTIONS.md"
    instructions_path.write_text(instructions_src.read_text(), encoding="utf-8")

    # Copy auxiliary files referenced in instructions
    for subdir in ("instructions",):
        src_dir = assets_dir / subdir
        dest_dir = tmp_path / subdir
        for file in src_dir.rglob("*.md"):
            rel = file.relative_to(src_dir)
            target = dest_dir / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(file.read_text(), encoding="utf-8")

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
    assert {node.id for node in stored_dag.nodes} == {"task-1"}
    assert spawner.calls == [("agent-research", "fakeredis://", "INFO")]


def test_orchestrator_applies_role_workflow(redis_store, tmp_path):
    """Orchestrator should add QA and integration workflow nodes."""
    instructions_path = tmp_path / "INSTRUCTIONS.md"
    instructions_path.write_text(
        """```yaml
agents:
  - name: team-lead
    role: team-lead
    goal: goal-demo
    heartbeat: instructions/heartbeat/TEAM_LEAD.md
    instructions: instructions/TEAM_LEAD.md
  - name: features-dev
    role: features-dev
    goal: goal-demo
    heartbeat: instructions/heartbeat/FEATURES_DEV.md
    instructions: instructions/FEATURES_DEV.md
  - name: qa-dev
    role: qa-dev
    goal: goal-demo
    heartbeat: instructions/heartbeat/QA_DEV.md
    instructions: instructions/QA_DEV.md
```""",
        encoding="utf-8",
    )
    (tmp_path / "instructions" / "heartbeat").mkdir(parents=True)
    (tmp_path / "instructions").mkdir(exist_ok=True)
    (tmp_path / "instructions" / "heartbeat" / "TEAM_LEAD.md").write_text(
        "RESUME\n", encoding="utf-8"
    )
    (tmp_path / "instructions" / "heartbeat" / "FEATURES_DEV.md").write_text(
        "RESUME\n", encoding="utf-8"
    )
    (tmp_path / "instructions" / "heartbeat" / "QA_DEV.md").write_text(
        "RESUME\n", encoding="utf-8"
    )
    (tmp_path / "instructions" / "TEAM_LEAD.md").write_text(
        "Coordinate integration and final report", encoding="utf-8"
    )
    (tmp_path / "instructions" / "FEATURES_DEV.md").write_text(
        "Implement features", encoding="utf-8"
    )
    (tmp_path / "instructions" / "QA_DEV.md").write_text(
        "Validation only. Run tests and manual validation.", encoding="utf-8"
    )

    dag = DagModel(
        goal_id="goal-demo",
        nodes=[TaskNode(id="task-1", title="Step 1", priority=1)],
        edges=[],
    )
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

    stored = redis_store.read_dag("goal-demo")
    assert stored is not None
    node_ids = {node.id for node in stored.nodes}
    assert node_ids == {"task-1", "task-1__qa", "task-1__integrate"}
