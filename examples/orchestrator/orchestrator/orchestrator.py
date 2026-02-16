"""Orchestrator process that supervises agents and the DAG lifecycle."""

from __future__ import annotations

import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from .cache import RedisStore
from .constants import DEFAULT_POLL_INTERVAL
from .instructions import InstructionParser
from .llm import LLMClient, LLMRuntimeConfig
from .models import AgentDescriptor, DagModel
from .version import BaseLogger, InstructionStore


@dataclass
class SpawnHandle:
    """Tracks a spawned process PID and its command arguments."""

    pid: int
    command: list[str]


@dataclass
class PreparedAgent:
    """Stores an agent descriptor plus parsed instruction context."""

    descriptor: AgentDescriptor
    instructions_text: str
    category: str


class SpawnAdapter:
    """Interface for spawning agents under systemg supervision."""

    def spawn_agent(
        self,
        descriptor: AgentDescriptor,
        *,
        parent_pid: int,
        redis_url: str,
        log_level: str,
        heartbeat_interval: float,
        instruction_interval: float,
        llm_config: LLMRuntimeConfig,
    ) -> SpawnHandle:
        """Spawn one agent process and return a handle."""
        raise NotImplementedError


class LoggingSpawnAdapter(SpawnAdapter, BaseLogger):
    """Default adapter used in tests and development environments."""

    def __init__(self):
        """Initialize dry-run spawn adapter."""
        BaseLogger.__init__(self, f"{self.__class__.__name__}")

    def spawn_agent(
        self,
        descriptor: AgentDescriptor,
        *,
        parent_pid: int,
        redis_url: str,
        log_level: str,
        heartbeat_interval: float,
        instruction_interval: float,
        llm_config: LLMRuntimeConfig,
    ) -> SpawnHandle:
        """Build an agent command and log it without executing."""
        command = [
            "sysg",
            "spawn",
            "--name",
            f"agent-{descriptor.name}",
            "--parent-pid",
            str(parent_pid),
            "--log-level",
            log_level,
            sys.executable,
            str(Path(__file__).resolve().parent.parent / "main.py"),
            "--role",
            "agent",
            "--agent-name",
            descriptor.name,
            "--agent-role",
            descriptor.effective_role,
            "--goal-id",
            descriptor.goal_id,
            "--instructions",
            str(descriptor.instructions_path),
            "--heartbeat",
            str(descriptor.heartbeat_path),
            "--redis-url",
            redis_url,
            "--log-level",
            log_level,
            "--loop-interval",
            str(float(descriptor.cadence_seconds)),
            "--heartbeat-interval",
            str(float(heartbeat_interval)),
            "--instruction-interval",
            str(float(instruction_interval)),
            *llm_config.cli_args(),
        ]
        self.logger.info("(dry-run) would spawn agent: %s", " ".join(command))
        return SpawnHandle(pid=-1, command=command)


class RealSpawnAdapter(SpawnAdapter, BaseLogger):
    """Adapter that actually spawns agent processes."""

    def __init__(self):
        """Initialize real spawn adapter."""
        BaseLogger.__init__(self, f"{self.__class__.__name__}")

    def spawn_agent(
        self,
        descriptor: AgentDescriptor,
        *,
        parent_pid: int,
        redis_url: str,
        log_level: str,
        heartbeat_interval: float,
        instruction_interval: float,
        llm_config: LLMRuntimeConfig,
    ) -> SpawnHandle:
        """Spawn agent process via `sysg spawn` and return handle."""
        command = [
            "sysg",
            "spawn",
            "--name",
            f"agent-{descriptor.name}",
            "--parent-pid",
            str(parent_pid),
            "--log-level",
            log_level,
            sys.executable,
            str(Path(__file__).resolve().parent.parent / "main.py"),
            "--role",
            "agent",
            "--agent-name",
            descriptor.name,
            "--agent-role",
            descriptor.effective_role,
            "--goal-id",
            descriptor.goal_id,
            "--instructions",
            str(descriptor.instructions_path),
            "--heartbeat",
            str(descriptor.heartbeat_path),
            "--redis-url",
            redis_url,
            "--log-level",
            log_level,
            "--loop-interval",
            str(float(descriptor.cadence_seconds)),
            "--heartbeat-interval",
            str(float(heartbeat_interval)),
            "--instruction-interval",
            str(float(instruction_interval)),
            *llm_config.cli_args(),
        ]
        self.logger.info("Spawning agent: %s", " ".join(command))
        completed = subprocess.run(command, capture_output=True, text=True, check=False)
        stdout = (completed.stdout or "").strip()
        stderr = (completed.stderr or "").strip()
        if completed.returncode != 0:
            raise RuntimeError(f"sysg spawn failed ({completed.returncode}): {stderr or stdout}")
        pid = self._extract_spawned_pid(stdout)
        return SpawnHandle(pid=pid, command=command)

    @staticmethod
    def _extract_spawned_pid(stdout: str) -> int:
        """Extract child PID from sysg spawn command output."""
        text = stdout.strip()
        if not text:
            raise RuntimeError("Unable to parse spawned PID from empty sysg output")
        if text.isdigit():
            return int(text)
        for line in reversed(text.splitlines()):
            candidate = line.strip()
            if candidate.isdigit():
                return int(candidate)
            match = re.search(r"Spawned process with PID:\s*(\d+)", candidate)
            if match:
                return int(match.group(1))
        match = re.search(r"Spawned process with PID:\s*(\d+)", text)
        if match:
            return int(match.group(1))
        raise RuntimeError(f"Unable to parse spawned PID from sysg output: {stdout}")


class Orchestrator(BaseLogger):
    """Runs the reconciliation loop for DAG state and agent processes."""

    def __init__(
        self,
        *,
        instructions_path: Path,
        redis_store: RedisStore,
        redis_url: str,
        llm_client: LLMClient | None = None,
        llm_config: LLMRuntimeConfig | None = None,
        spawn_adapter: SpawnAdapter | None = None,
        poll_interval: float = DEFAULT_POLL_INTERVAL,
        heartbeat_interval: float = 300.0,
        instruction_interval: float = 300.0,
    ) -> None:
        """Initialize orchestrator dependencies and runtime state."""
        super().__init__(f"{self.__class__.__name__}")
        self.instructions_path = instructions_path
        self.redis_store = redis_store
        self.redis_url = redis_url
        if llm_client is None:
            raise ValueError("Orchestrator requires an LLM client instance")
        self.llm_client = llm_client
        self.llm_config = llm_config or LLMRuntimeConfig()
        self.spawn_adapter = spawn_adapter or LoggingSpawnAdapter()
        self.poll_interval = poll_interval
        self.heartbeat_interval = heartbeat_interval
        self.instruction_interval = instruction_interval

        self.parser = InstructionParser(instructions_path)
        self.instruction_store = InstructionStore(redis_store.client)
        self._spawned: dict[str, SpawnHandle] = {}
        self._resume_reported_goals: set[str] = set()
        self._active = True

    def run(self, *, max_cycles: int | None = None) -> None:
        """Run orchestration loop."""
        cycles = 0
        while self._active and (max_cycles is None or cycles < max_cycles):
            self._process_cycle()
            cycles += 1
            if self.poll_interval > 0:
                time.sleep(self.poll_interval)

    def stop(self) -> None:
        """Request orchestrator shutdown."""
        self._active = False

    def _process_cycle(self) -> None:
        """Execute one reconciliation cycle."""
        try:
            agents = self._prepare_agents(self.parser.parse_agents())
        except ValueError as exc:
            self.logger.error("Failed to parse instructions: %s", exc)
            return

        goal_map: dict[str, list[PreparedAgent]] = {}
        for agent in agents:
            goal_map.setdefault(agent.descriptor.goal_id, []).append(agent)

        for goal_agents in goal_map.values():
            planner = next(
                (agent for agent in goal_agents if agent.category == "manager"),
                None,
            )
            if planner is None:
                planner = goal_agents[0]
            self._ensure_goal_state(planner, goal_agents)
            self._report_resume_state(planner.descriptor.goal_id)

        self._spawn_agents_hierarchy(agents)

        stale_agents = set(self._spawned.keys()) - {agent.descriptor.name for agent in agents}
        for agent_name in stale_agents:
            self.logger.info("Agent %s removed from instructions; marking as stale", agent_name)
            self._spawned.pop(agent_name, None)

    def _ensure_goal_state(self, planner: PreparedAgent, goal_agents: list[PreparedAgent]) -> None:
        """Create and persist goal DAG state when absent."""
        descriptor = planner.descriptor
        dag = self.redis_store.read_dag(descriptor.goal_id)
        if dag:
            return
        if not planner.instructions_text:
            self.logger.warning(
                "Instructions for agent %s missing at %s",
                descriptor.name,
                descriptor.instructions_path,
            )
            return
        instructions_text = planner.instructions_text

        instruction_id = descriptor.cname()
        self.instruction_store.push_version(instructions_text, instruction_id)
        self.logger.info(
            "Pushed instruction version for agent %s (ID: %s)", descriptor.name, instruction_id
        )

        self.logger.info("Generating DAG for goal %s using LLM", descriptor.goal_id)
        self.logger.debug("Instructions length: %d chars", len(instructions_text))

        dag = self.llm_client.create_goal_dag(instructions_text, goal_id=descriptor.goal_id)
        dag = self._apply_role_workflow(dag, goal_agents)

        self.logger.info(
            "DAG created for goal %s: %d nodes, %d edges",
            descriptor.goal_id,
            len(dag.nodes),
            len(dag.edges),
        )
        for node in dag.nodes:
            self.logger.info("  Task: %s (priority=%d) - %s", node.id, node.priority, node.title)
            if node.expected_artifacts:
                self.logger.debug("    Expected artifacts: %s", node.expected_artifacts)
        for edge in dag.edges:
            self.logger.debug("  Dependency: %s -> %s", edge.source, edge.target)

        self._validate_dag(dag)
        self.redis_store.write_dag(dag)

        initialized_count = 0
        for node in dag.nodes:
            if not self.redis_store.get_task_state(node.id):
                from .models import TaskState, TaskStatus

                self.redis_store.update_task_state(node.id, TaskState(status=TaskStatus.READY))
                initialized_count += 1
        self.logger.info("Initialized %d task states as READY", initialized_count)

    def _apply_role_workflow(self, dag: DagModel, goal_agents: list[PreparedAgent]) -> DagModel:
        """Wrap each development node with QA and integration ownership stages."""
        from .models import TaskEdge, TaskNode

        role_classification = {
            agent.descriptor.effective_role: agent.category for agent in goal_agents
        }
        manager_roles = [
            role for role, category in role_classification.items() if category == "manager"
        ]
        reviewer_roles = [
            role for role, category in role_classification.items() if category == "reviewer"
        ]
        builder_roles = [
            role for role, category in role_classification.items() if category == "builder"
        ]
        if not builder_roles:
            builder_roles = [goal_agents[0].descriptor.effective_role]
        qa_role = reviewer_roles[0] if reviewer_roles else None
        lead_role = manager_roles[0] if manager_roles else None

        original_nodes = list(dag.nodes)
        original_edges = list(dag.edges)
        nodes: list[TaskNode] = []
        edges: list[TaskEdge] = []

        for edge in original_edges:
            edges.append(TaskEdge(source=edge.source, target=edge.target))

        for index, node in enumerate(original_nodes):
            metadata = dict(node.metadata)
            metadata.setdefault("phase", "development")
            metadata.setdefault("review_cycle", "0")
            metadata.setdefault("required_role", builder_roles[index % len(builder_roles)])
            metadata.setdefault("dev_role", metadata["required_role"])
            if lead_role:
                metadata.setdefault("manager_role", lead_role)
            nodes.append(
                node.model_copy(
                    update={
                        "metadata": metadata,
                    }
                )
            )

            previous = node.id
            if qa_role:
                qa_id = f"{node.id}__qa"
                qa_node = TaskNode(
                    id=qa_id,
                    title=f"QA review for {node.title}",
                    priority=node.priority,
                    expected_artifacts=["qa-report.md"],
                    metadata={
                        "phase": "qa",
                        "required_role": qa_role,
                        "parent_task_id": node.id,
                        "review_cycle": "0",
                        "dev_role": metadata["dev_role"],
                        "manager_role": lead_role or "",
                    },
                )
                nodes.append(qa_node)
                edges.append(TaskEdge(source=previous, target=qa_id))
                previous = qa_id

            if lead_role:
                integration_id = f"{node.id}__integrate"
                integration_node = TaskNode(
                    id=integration_id,
                    title=f"Integrate {node.title}",
                    priority=node.priority,
                    expected_artifacts=["integration-report.md"],
                    metadata={
                        "phase": "integration",
                        "required_role": lead_role,
                        "parent_task_id": node.id,
                        "manager_role": lead_role,
                    },
                )
                nodes.append(integration_node)
                edges.append(TaskEdge(source=previous, target=integration_id))

        return DagModel(goal_id=dag.goal_id, nodes=nodes, edges=edges)

    def _prepare_agents(self, descriptors: list[AgentDescriptor]) -> list[PreparedAgent]:
        """Build per-agent execution profiles before orchestration starts."""
        prepared: list[PreparedAgent] = []
        for descriptor in descriptors:
            instructions_text = ""
            if descriptor.instructions_path.exists():
                instructions_text = descriptor.instructions_path.read_text(encoding="utf-8")
            category = self._classify_agent(descriptor)
            prepared.append(
                PreparedAgent(
                    descriptor=descriptor,
                    instructions_text=instructions_text,
                    category=category,
                )
            )
        return prepared

    def _classify_agent(self, descriptor: AgentDescriptor) -> str:
        """Infer broad execution category from instruction/heartbeat filenames."""
        explicit = (descriptor.role or "").strip().lower()
        filename_tokens = " ".join(
            [
                descriptor.instructions_path.stem.lower().replace("-", " ").replace("_", " "),
                descriptor.heartbeat_path.stem.lower().replace("-", " ").replace("_", " "),
                descriptor.name.lower().replace("-", " ").replace("_", " "),
            ]
        )
        manager_tokens = {"owner", "lead", "manager"}
        reviewer_tokens = {"qa", "test", "validator", "review"}
        if explicit in {"manager", "owner", "team-lead", "lead"}:
            return "manager"
        if explicit in {"reviewer", "qa", "qa-dev", "tester"}:
            return "reviewer"
        if any(token in filename_tokens.split() for token in manager_tokens):
            return "manager"
        if any(token in filename_tokens.split() for token in reviewer_tokens):
            return "reviewer"
        return "builder"

    def _spawn_agents_hierarchy(self, agents: list[PreparedAgent]) -> None:
        """Spawn agents using owner->team-lead->worker parent chain."""
        orchestrator_pid = os.getpid()
        owner = next((agent for agent in agents if self._is_owner(agent.descriptor)), None)
        team_lead = next(
            (
                agent
                for agent in agents
                if self._is_team_lead(agent.descriptor)
                and (owner is None or agent.descriptor.name != owner.descriptor.name)
            ),
            None,
        )

        owner_pid = orchestrator_pid
        if owner is not None:
            owner_handle = self._ensure_agent_spawn(owner.descriptor, parent_pid=orchestrator_pid)
            owner_pid = owner_handle.pid

        team_lead_parent_pid = owner_pid if owner is not None else orchestrator_pid
        team_lead_pid = team_lead_parent_pid
        if team_lead is not None:
            team_lead_handle = self._ensure_agent_spawn(
                team_lead.descriptor, parent_pid=team_lead_parent_pid
            )
            team_lead_pid = team_lead_handle.pid

        for agent in agents:
            if owner is not None and agent.descriptor.name == owner.descriptor.name:
                continue
            if team_lead is not None and agent.descriptor.name == team_lead.descriptor.name:
                continue
            self._ensure_agent_spawn(agent.descriptor, parent_pid=team_lead_pid)

    @staticmethod
    def _is_owner(descriptor: AgentDescriptor) -> bool:
        """Return whether descriptor maps to the owner role."""
        role = (descriptor.role or "").strip().lower()
        name = descriptor.name.strip().lower()
        return role == "owner" or name == "owner"

    @staticmethod
    def _is_team_lead(descriptor: AgentDescriptor) -> bool:
        """Return whether descriptor maps to the team-lead role."""
        role = (descriptor.role or "").strip().lower()
        name = descriptor.name.strip().lower().replace("_", "-")
        return role in {"team-lead", "lead"} or name in {"team-lead", "teamlead", "lead"}

    def _ensure_agent_spawn(self, descriptor: AgentDescriptor, *, parent_pid: int) -> SpawnHandle:
        """Ensure a declared agent has been spawned."""
        if descriptor.name in self._spawned:
            return self._spawned[descriptor.name]
        handle = self.spawn_adapter.spawn_agent(
            descriptor,
            parent_pid=parent_pid,
            redis_url=self.redis_url,
            log_level=descriptor.log_level,
            heartbeat_interval=self.heartbeat_interval,
            instruction_interval=self.instruction_interval,
            llm_config=self.llm_config,
        )
        self.logger.info("Spawned agent %s with pid %s", descriptor.name, handle.pid)
        self._spawned[descriptor.name] = handle
        return handle

    def _report_resume_state(self, goal_id: str) -> None:
        """Recover stale tasks once per goal and emit a resume summary."""
        if goal_id in self._resume_reported_goals:
            return
        recovered = self.redis_store.recover_stale_tasks(goal_id)
        self.logger.info(
            "Resume summary for goal %s: recovered %d stale task(s)",
            goal_id,
            len(recovered),
        )
        self._resume_reported_goals.add(goal_id)

    def _validate_dag(self, dag: DagModel) -> None:
        """Validate edge integrity and acyclic structure."""
        node_ids = {node.id for node in dag.nodes}
        for edge in dag.edges:
            if edge.source not in node_ids or edge.target not in node_ids:
                raise ValueError(f"Invalid edge {edge.source}->{edge.target}")
        adjacency: dict[str, list[str]] = {node_id: [] for node_id in node_ids}
        for edge in dag.edges:
            adjacency[edge.source].append(edge.target)

        visited: dict[str, bool] = {}

        def visit(node_id: str, stack: list[str]) -> None:
            """Depth-first walk helper for cycle detection."""
            if visited.get(node_id) is True:
                return
            if visited.get(node_id) is False:
                cycle = "->".join(stack + [node_id])
                raise ValueError(f"Cycle detected in DAG: {cycle}")
            visited[node_id] = False
            for neighbor in adjacency.get(node_id, []):
                visit(neighbor, stack + [neighbor])
            visited[node_id] = True

        for node_id in node_ids:
            visit(node_id, [node_id])
