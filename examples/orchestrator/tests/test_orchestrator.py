import os
from datetime import datetime, timedelta, timezone

from orchestrator.llm import StubLLMClient
from orchestrator.models import DagModel, TaskNode, TaskState, TaskStatus
from orchestrator.orchestrator import Orchestrator, RealSpawnAdapter, SpawnAdapter, SpawnHandle


class RecordingSpawner(SpawnAdapter):
    def __init__(self):
        """Initialize call recorder."""
        self.calls = []
        self._next_pid = 4000

    def spawn_agent(  # type: ignore[override]
        self,
        descriptor,
        *,
        parent_pid: int,
        redis_url: str,
        log_level: str,
        heartbeat_interval: float,
        instruction_interval: float,
        llm_config,
    ) -> SpawnHandle:
        """Record spawn arguments and return fixed handle."""
        pid = self._next_pid
        self._next_pid += 1
        self.calls.append(
            (
                descriptor.name,
                parent_pid,
                redis_url,
                log_level,
                heartbeat_interval,
                instruction_interval,
                llm_config.provider,
            )
        )
        return SpawnHandle(pid=pid, command=["sysg", "spawn"])


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
    assert spawner.calls == [
        ("agent-research", os.getpid(), "fakeredis://", "INFO", 300.0, 300.0, "claude")
    ]


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
    (tmp_path / "instructions" / "heartbeat" / "QA_DEV.md").write_text("RESUME\n", encoding="utf-8")
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


def test_orchestrator_start_recovers_stale_tasks(redis_store, tmp_path):
    """Orchestrator startup should recover stale running tasks and report resume state."""
    instructions_path = tmp_path / "INSTRUCTIONS.md"
    instructions_path.write_text(
        """```yaml
agents:
  - name: agent-research
    role: agent-research
    goal: goal-demo
    heartbeat: instructions/heartbeat/agent-research.md
    instructions: instructions/agent-research.md
```""",
        encoding="utf-8",
    )
    (tmp_path / "instructions" / "heartbeat").mkdir(parents=True)
    (tmp_path / "instructions" / "heartbeat" / "agent-research.md").write_text(
        "RESUME\n", encoding="utf-8"
    )
    (tmp_path / "instructions" / "agent-research.md").write_text("test", encoding="utf-8")

    dag = DagModel(
        goal_id="goal-demo", nodes=[TaskNode(id="task-1", title="Step 1", priority=1)], edges=[]
    )
    redis_store.write_dag(dag)
    redis_store.update_task_state(
        "task-1",
        TaskState(
            status=TaskStatus.RUNNING,
            owner="dead-agent",
            lease_expires=datetime.now(timezone.utc) - timedelta(seconds=1),
        ),
    )

    orchestrator = Orchestrator(
        instructions_path=instructions_path,
        redis_store=redis_store,
        redis_url="fakeredis://",
        llm_client=StubLLMClient(dag),
        spawn_adapter=RecordingSpawner(),
        poll_interval=0,
    )
    orchestrator.run(max_cycles=1)

    state = redis_store.get_task_state("task-1")
    assert state is not None
    assert state.status is TaskStatus.READY


def test_orchestrator_spawns_owner_team_lead_then_workers(redis_store, tmp_path):
    """Orchestrator should wire parent PID chain as owner->lead->workers."""
    instructions_path = tmp_path / "INSTRUCTIONS.md"
    instructions_path.write_text(
        """```yaml
agents:
  - name: owner
    role: owner
    goal: goal-demo
    heartbeat: instructions/heartbeat/OWNER.md
    instructions: instructions/OWNER.md
  - name: team-lead
    role: team-lead
    goal: goal-demo
    heartbeat: instructions/heartbeat/TEAM_LEAD.md
    instructions: instructions/TEAM_LEAD.md
  - name: worker-a
    role: features-dev
    goal: goal-demo
    heartbeat: instructions/heartbeat/WORKER_A.md
    instructions: instructions/WORKER_A.md
  - name: worker-b
    role: qa-dev
    goal: goal-demo
    heartbeat: instructions/heartbeat/WORKER_B.md
    instructions: instructions/WORKER_B.md
```""",
        encoding="utf-8",
    )
    (tmp_path / "instructions" / "heartbeat").mkdir(parents=True)
    (tmp_path / "instructions").mkdir(exist_ok=True)
    for name in ("OWNER", "TEAM_LEAD", "WORKER_A", "WORKER_B"):
        (tmp_path / "instructions" / "heartbeat" / f"{name}.md").write_text(
            "RESUME\n", encoding="utf-8"
        )
        (tmp_path / "instructions" / f"{name}.md").write_text(
            f"{name} instructions", encoding="utf-8"
        )

    dag = DagModel(
        goal_id="goal-demo",
        nodes=[TaskNode(id="task-1", title="Step 1", priority=1)],
        edges=[],
    )
    spawner = RecordingSpawner()
    orchestrator = Orchestrator(
        instructions_path=instructions_path,
        redis_store=redis_store,
        redis_url="fakeredis://",
        llm_client=StubLLMClient(dag),
        spawn_adapter=spawner,
        poll_interval=0,
    )
    orchestrator.run(max_cycles=1)

    assert [call[0] for call in spawner.calls] == ["owner", "team-lead", "worker-a", "worker-b"]
    owner_pid = 4000
    team_lead_pid = 4001
    assert spawner.calls[0][1] == os.getpid()
    assert spawner.calls[1][1] == owner_pid
    assert spawner.calls[2][1] == team_lead_pid
    assert spawner.calls[3][1] == team_lead_pid


def test_real_spawn_adapter_parses_plain_pid_output():
    """Real spawn adapter should accept plain numeric sysg output."""
    assert RealSpawnAdapter._extract_spawned_pid("89310\n") == 89310
