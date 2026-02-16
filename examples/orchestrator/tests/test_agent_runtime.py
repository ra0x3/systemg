from datetime import datetime, timedelta, timezone

from orchestrator.llm import (
    LLMClient,
    RecoveryDecision,
    StubLLMClient,
    TaskExecutionResult,
    TaskSelection,
)
from orchestrator.models import DagModel, TaskEdge, TaskNode, TaskState, TaskStatus
from orchestrator.runtime import AgentRuntime


def _prepare_dag(goal_id: str) -> DagModel:
    """Build a single-node DAG fixture."""
    return DagModel(
        goal_id=goal_id,
        nodes=[TaskNode(id="task-1", title="Initial task", priority=1)],
        edges=[],
    )


class ScriptedLLM(LLMClient):
    """Deterministic client for role-transition tests."""

    def __init__(self) -> None:
        """Initialize scripted QA failure counter."""
        self.qa_failures = 1

    def create_goal_dag(self, instructions: str, *, goal_id: str) -> DagModel:
        """Unused in these runtime tests."""
        raise NotImplementedError

    def select_next_task(
        self, ready_nodes, *, memory, goal_id: str, instructions: str
    ) -> TaskSelection:
        """Select the first ready task deterministically."""
        first = next(iter(ready_nodes), None)
        if first is None:
            return TaskSelection(selected_task_id=None, justification="No tasks", confidence=0.0)
        return TaskSelection(selected_task_id=first.id, justification="Pick first", confidence=1.0)

    def execute_task(
        self, task: TaskNode, *, goal_id: str, instructions: str, memory
    ) -> TaskExecutionResult:
        """Fail first QA run, then return successful execution."""
        phase = task.metadata.get("phase", "development")
        if phase == "qa" and self.qa_failures > 0:
            self.qa_failures -= 1
            return TaskExecutionResult(
                status="failed",
                outputs=[],
                notes=f"QA failed for {task.id}",
                follow_ups=[],
            )
        return TaskExecutionResult(
            status="done",
            outputs=[f"artifact://{task.id}.txt"],
            notes=f"Completed {task.id}",
            follow_ups=[],
        )

    def summarize_task(
        self,
        task: TaskNode,
        execution: TaskExecutionResult,
        *,
        goal_id: str,
        instructions: str,
        memory,
    ) -> str:
        """Return deterministic summary text."""
        return f"{task.id}: {execution.notes}"

    def assess_recovery(
        self,
        task: TaskNode,
        *,
        error: str,
        goal_id: str,
        instructions: str,
        memory,
    ) -> RecoveryDecision:
        """Unused in QA workflow tests."""
        return RecoveryDecision(
            recoverable=False,
            reason="Not used in this test",
            remediation_title="",
            remediation_steps=[],
            confidence=0.0,
        )


class FailingOnceLLM(StubLLMClient):
    """Fail first execution, then succeed."""

    def __init__(self, error_text: str):
        super().__init__()
        self.error_text = error_text
        self.calls = 0

    def execute_task(
        self,
        task: TaskNode,
        *,
        goal_id: str,
        instructions: str,
        memory,
    ) -> TaskExecutionResult:
        self.calls += 1
        if self.calls == 1:
            raise RuntimeError(self.error_text)
        return super().execute_task(task, goal_id=goal_id, instructions=instructions, memory=memory)


class NoSelectLLM(StubLLMClient):
    """Fail fast if selection is attempted."""

    def select_next_task(
        self, ready_nodes, *, memory, goal_id: str, instructions: str
    ) -> TaskSelection:
        raise AssertionError("select_next_task should not run during spending-cap backoff")


class CountingHeartbeatRuntime(AgentRuntime):
    """Runtime test helper to count heartbeat file reads."""

    def __init__(self, *args, **kwargs):
        """Initialize counter before base runtime setup."""
        self.heartbeat_reads = 0
        super().__init__(*args, **kwargs)

    def poll_heartbeat(self):  # type: ignore[override]
        """Count heartbeat reads and return no directives."""
        self.heartbeat_reads += 1
        return []


class CountingReloadRuntime(AgentRuntime):
    """Runtime test helper to count instruction reloads."""

    def __init__(self, *args, **kwargs):
        """Initialize counter before base runtime setup."""
        self.reload_calls = 0
        super().__init__(*args, **kwargs)

    def reload_instructions(self) -> None:  # type: ignore[override]
        """Count reload calls and run normal behavior."""
        self.reload_calls += 1
        super().reload_instructions()


def test_agent_completes_task(redis_store, tmp_path, assets_dir):
    """Agent should complete a development task."""
    instructions_src = assets_dir / "instructions" / "agent-research.md"
    heartbeat_src = assets_dir / "instructions" / "heartbeat" / "agent-research.md"
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
    assert state.status is TaskStatus.DEV_DONE
    assert state.progress.startswith("Task task-1 completed")


def test_agent_pause_directive(redis_store, tmp_path, assets_dir):
    """Pause directive should skip execution cycle."""
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
    assert state.status is not TaskStatus.DEV_DONE


def test_role_restriction_blocks_wrong_agent(redis_store, tmp_path):
    """Agent should not claim task assigned to another role."""
    goal_id = "goal-role-check"
    dag = DagModel(
        goal_id=goal_id,
        nodes=[
            TaskNode(
                id="task-dev",
                title="Build feature",
                priority=1,
                metadata={"phase": "development", "required_role": "features-dev"},
            )
        ],
        edges=[],
    )
    redis_store.write_dag(dag)
    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text("test", encoding="utf-8")
    heartbeat_path.write_text("RESUME\n", encoding="utf-8")

    agent = AgentRuntime(
        agent_name="qa-dev",
        agent_role="qa-dev",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=StubLLMClient(),
        loop_interval=0,
    )
    agent.run(max_cycles=1)

    state = redis_store.get_task_state("task-dev")
    assert state is not None
    assert state.status is TaskStatus.READY


def test_iterative_qa_loop_creates_remediation(redis_store, tmp_path):
    """QA failure should create remediation and pass after fix."""
    goal_id = "goal-iterative"
    dag = DagModel(
        goal_id=goal_id,
        nodes=[
            TaskNode(
                id="task-dev",
                title="Build feature",
                priority=1,
                metadata={
                    "phase": "development",
                    "required_role": "features-dev",
                    "dev_role": "features-dev",
                },
            ),
            TaskNode(
                id="task-dev__qa",
                title="QA review",
                priority=1,
                metadata={
                    "phase": "qa",
                    "required_role": "qa-dev",
                    "parent_task_id": "task-dev",
                    "review_cycle": "0",
                    "dev_role": "features-dev",
                },
            ),
            TaskNode(
                id="task-dev__integrate",
                title="Integrate",
                priority=1,
                metadata={"phase": "integration", "required_role": "team-lead"},
            ),
        ],
        edges=[
            TaskEdge(source="task-dev", target="task-dev__qa"),
            TaskEdge(source="task-dev__qa", target="task-dev__integrate"),
        ],
    )
    redis_store.write_dag(dag)
    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text("test", encoding="utf-8")
    heartbeat_path.write_text("RESUME\n", encoding="utf-8")

    llm = ScriptedLLM()

    features_agent = AgentRuntime(
        agent_name="features-dev",
        agent_role="features-dev",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=llm,
        loop_interval=0,
    )
    qa_agent = AgentRuntime(
        agent_name="qa-dev",
        agent_role="qa-dev",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=llm,
        loop_interval=0,
    )
    lead_agent = AgentRuntime(
        agent_name="team-lead",
        agent_role="team-lead",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=llm,
        loop_interval=0,
    )

    features_agent.run(max_cycles=1)
    state = redis_store.get_task_state("task-dev")
    assert state is not None
    assert state.status is TaskStatus.DEV_DONE

    qa_agent.run(max_cycles=2)
    qa_state = redis_store.get_task_state("task-dev__qa")
    assert qa_state is not None
    assert qa_state.status is TaskStatus.BLOCKED

    remediation_nodes = [
        node
        for node in redis_store.read_dag(goal_id).nodes  # type: ignore[union-attr]
        if node.id.startswith("task-dev__qa__fix_")
    ]
    assert remediation_nodes
    remediation_id = remediation_nodes[0].id
    assert remediation_nodes[0].metadata["required_role"] == "features-dev"

    features_agent.run(max_cycles=1)
    remediation_state = redis_store.get_task_state(remediation_id)
    assert remediation_state is not None
    assert remediation_state.status is TaskStatus.DEV_DONE

    qa_agent.run(max_cycles=1)
    qa_state = redis_store.get_task_state("task-dev__qa")
    assert qa_state is not None
    assert qa_state.status is TaskStatus.QA_PASSED

    lead_agent.run(max_cycles=1)
    integration_state = redis_store.get_task_state("task-dev__integrate")
    assert integration_state is not None
    assert integration_state.status is TaskStatus.DONE


def test_agent_retries_stale_running_task_after_restart(redis_store, tmp_path):
    """Agent should resume by reclaiming stale running work on startup."""
    goal_id = "goal-resume"
    redis_store.write_dag(_prepare_dag(goal_id))
    redis_store.update_task_state(
        "task-1",
        TaskState(
            status=TaskStatus.RUNNING,
            owner="agent-crashed",
            lease_expires=datetime.now(timezone.utc) - timedelta(seconds=1),
        ),
    )

    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text("test", encoding="utf-8")
    heartbeat_path.write_text("RESUME\n", encoding="utf-8")

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
    agent.run(max_cycles=1)

    state = redis_store.get_task_state("task-1")
    assert state is not None
    assert state.status is TaskStatus.DEV_DONE


def test_agent_reads_heartbeat_every_120_seconds_by_default(redis_store, tmp_path):
    """Heartbeat directives should be read on the configured refresh interval."""
    goal_id = "goal-heartbeat-interval"
    redis_store.write_dag(_prepare_dag(goal_id))

    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text("test", encoding="utf-8")
    heartbeat_path.write_text("RESUME\n", encoding="utf-8")

    agent = CountingHeartbeatRuntime(
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

    assert agent.heartbeat_reads == 1


def test_agent_reads_instructions_every_120_seconds_by_default(redis_store, tmp_path):
    """Instruction file should not be reloaded each cycle under default interval."""
    goal_id = "goal-instruction-interval"
    redis_store.write_dag(_prepare_dag(goal_id))

    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text("test", encoding="utf-8")
    heartbeat_path.write_text("RESUME\n", encoding="utf-8")

    agent = CountingReloadRuntime(
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

    assert agent.reload_calls == 1


def test_recoverable_runtime_error_creates_recovery_task(redis_store, tmp_path):
    """Recoverable runtime errors should block original task and enqueue remediation."""
    goal_id = "goal-recovery-loop"
    redis_store.write_dag(_prepare_dag(goal_id))

    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text("test", encoding="utf-8")
    heartbeat_path.write_text("RESUME\n", encoding="utf-8")

    llm = FailingOnceLLM("node: command not found")
    agent = AgentRuntime(
        agent_name="features-dev",
        agent_role="features-dev",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=llm,
        loop_interval=0,
    )
    agent.run(max_cycles=1)

    state = redis_store.get_task_state("task-1")
    assert state is not None
    assert state.status is TaskStatus.BLOCKED
    assert "created remediation task" in (state.progress or "")

    dag = redis_store.read_dag(goal_id)
    assert dag is not None
    recovery_nodes = [node for node in dag.nodes if node.id.startswith("task-1__recover_")]
    assert recovery_nodes
    recovery_state = redis_store.get_task_state(recovery_nodes[0].id)
    assert recovery_state is not None
    assert recovery_state.status is TaskStatus.READY


def test_recovery_attempt_cap_marks_terminal_failure(redis_store, tmp_path):
    """Tasks should fail after bounded recovery attempts."""
    goal_id = "goal-recovery-cap"
    dag = DagModel(
        goal_id=goal_id,
        nodes=[
            TaskNode(
                id="task-1",
                title="Initial task",
                priority=1,
                metadata={"recovery_attempts": str(AgentRuntime.MAX_RECOVERY_ATTEMPTS)},
            )
        ],
        edges=[],
    )
    redis_store.write_dag(dag)

    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text("test", encoding="utf-8")
    heartbeat_path.write_text("RESUME\n", encoding="utf-8")

    llm = FailingOnceLLM("node: command not found")
    agent = AgentRuntime(
        agent_name="features-dev",
        agent_role="features-dev",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=llm,
        loop_interval=0,
    )
    agent.run(max_cycles=1)

    state = redis_store.get_task_state("task-1")
    assert state is not None
    assert state.status is TaskStatus.FAILED


def test_goal_spending_cap_backoff_skips_work_cycle(redis_store, tmp_path):
    """Active goal-level spending cap should prevent task selection attempts."""
    goal_id = "goal-cap-backoff"
    redis_store.write_dag(_prepare_dag(goal_id))
    redis_store.set_goal_spending_cap_until(
        goal_id,
        datetime.now(timezone.utc) + timedelta(seconds=60),
    )

    instructions_path = tmp_path / "instructions.md"
    heartbeat_path = tmp_path / "heartbeat.md"
    instructions_path.write_text("test", encoding="utf-8")
    heartbeat_path.write_text("RESUME\n", encoding="utf-8")

    agent = AgentRuntime(
        agent_name="features-dev",
        agent_role="features-dev",
        goal_id=goal_id,
        instructions_path=instructions_path,
        heartbeat_path=heartbeat_path,
        redis_store=redis_store,
        llm_client=NoSelectLLM(),
        loop_interval=0,
    )
    agent.run(max_cycles=1)

    state = redis_store.get_task_state("task-1")
    assert state is not None
    assert state.status is TaskStatus.READY
