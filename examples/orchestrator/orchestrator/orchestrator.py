"""Orchestrator process that supervises agents and the DAG lifecycle."""

from __future__ import annotations

import os
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from .cache import RedisStore
from .instructions import InstructionParser
from .llm import LLMClient
from .models import AgentDescriptor, DagModel
from .version import BaseLogger, InstructionStore


@dataclass
class SpawnHandle:
    pid: int
    command: list[str]


class SpawnAdapter:
    """Interface for spawning agents under systemg supervision."""

    def spawn_agent(
        self, descriptor: AgentDescriptor, *, redis_url: str, log_level: str
    ) -> SpawnHandle:  # pragma: no cover - interface
        raise NotImplementedError


class LoggingSpawnAdapter(SpawnAdapter, BaseLogger):
    """Default adapter used in tests and development environments."""

    def __init__(self):
        BaseLogger.__init__(self, f"{self.__class__.__name__}")

    def spawn_agent(
        self, descriptor: AgentDescriptor, *, redis_url: str, log_level: str
    ) -> SpawnHandle:
        command = [
            "sysg",
            "spawn",
            "--name",
            f"agent-{descriptor.name}",
            "--parent-pid",
            str(os.getpid()),
            "--log-level",
            log_level,
            sys.executable,
            str(Path(__file__).resolve().parent.parent / "agent.py"),
            "--role",
            "agent",
            "--agent-name",
            descriptor.name,
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
        ]
        self.logger.info("(dry-run) would spawn agent: %s", " ".join(command))
        return SpawnHandle(pid=-1, command=command)


class RealSpawnAdapter(SpawnAdapter, BaseLogger):
    """Adapter that actually spawns agent processes."""

    def __init__(self):
        BaseLogger.__init__(self, f"{self.__class__.__name__}")

    def spawn_agent(
        self, descriptor: AgentDescriptor, *, redis_url: str, log_level: str
    ) -> SpawnHandle:
        command = [
            "sysg",
            "spawn",
            "--name",
            f"agent-{descriptor.name}",
            "--parent-pid",
            str(os.getpid()),
            "--log-level",
            log_level,
            sys.executable,
            str(Path(__file__).resolve().parent.parent / "agent.py"),
            "--role",
            "agent",
            "--agent-name",
            descriptor.name,
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
        ]
        self.logger.info("Spawning agent: %s", " ".join(command))
        process = subprocess.Popen(command)
        return SpawnHandle(pid=process.pid, command=command)


class Orchestrator(BaseLogger):
    def __init__(
        self,
        *,
        instructions_path: Path,
        redis_store: RedisStore,
        redis_url: str,
        llm_client: LLMClient | None = None,
        spawn_adapter: SpawnAdapter | None = None,
        poll_interval: float = 5.0,
    ) -> None:
        super().__init__(f"{self.__class__.__name__}")
        self.instructions_path = instructions_path
        self.redis_store = redis_store
        self.redis_url = redis_url
        if llm_client is None:
            raise ValueError("Orchestrator requires an LLM client instance")
        self.llm_client = llm_client
        self.spawn_adapter = spawn_adapter or LoggingSpawnAdapter()
        self.poll_interval = poll_interval

        self.parser = InstructionParser(instructions_path)
        self.instruction_store = InstructionStore(redis_store.client)
        self._spawned: dict[str, SpawnHandle] = {}
        self._active = True

    def run(self, *, max_cycles: int | None = None) -> None:
        cycles = 0
        while self._active and (max_cycles is None or cycles < max_cycles):
            self._process_cycle()
            cycles += 1
            if self.poll_interval > 0:
                time.sleep(self.poll_interval)

    def stop(self) -> None:
        self._active = False

    # ------------------------------------------------------------------
    def _process_cycle(self) -> None:
        try:
            agents = self.parser.parse_agents()
        except ValueError as exc:
            self.logger.error("Failed to parse instructions: %s", exc)
            return

        for descriptor in agents:
            self.logger.debug("Processing agent descriptor %s", descriptor)
            self._ensure_goal_state(descriptor)
            self._ensure_agent_spawn(descriptor)

        # Detect removed agents and clean up
        stale_agents = set(self._spawned.keys()) - {descriptor.name for descriptor in agents}
        for agent_name in stale_agents:
            self.logger.info("Agent %s removed from instructions; marking as stale", agent_name)
            self._spawned.pop(agent_name, None)

    def _ensure_goal_state(self, descriptor: AgentDescriptor) -> None:
        dag = self.redis_store.read_dag(descriptor.goal_id)
        if dag:
            return
        if not descriptor.instructions_path.exists():
            self.logger.warning(
                "Instructions for agent %s missing at %s",
                descriptor.name,
                descriptor.instructions_path,
            )
            return
        instructions_text = descriptor.instructions_path.read_text(encoding="utf-8")

        instruction_id = descriptor.cname()
        self.instruction_store.push_version(instructions_text, instruction_id)
        self.logger.info(
            "Pushed instruction version for agent %s (ID: %s)", descriptor.name, instruction_id
        )

        self.logger.info("Generating DAG for goal %s using LLM", descriptor.goal_id)
        self.logger.debug("Instructions length: %d chars", len(instructions_text))

        dag = self.llm_client.create_goal_dag(instructions_text, goal_id=descriptor.goal_id)

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

        # Initialize task states as READY
        initialized_count = 0
        for node in dag.nodes:
            if not self.redis_store.get_task_state(node.id):
                from .models import TaskState, TaskStatus

                self.redis_store.update_task_state(node.id, TaskState(status=TaskStatus.READY))
                initialized_count += 1
        self.logger.info("Initialized %d task states as READY", initialized_count)

    def _ensure_agent_spawn(self, descriptor: AgentDescriptor) -> None:
        if descriptor.name in self._spawned:
            return
        handle = self.spawn_adapter.spawn_agent(
            descriptor, redis_url=self.redis_url, log_level=descriptor.log_level
        )
        self.logger.info("Spawned agent %s with pid %s", descriptor.name, handle.pid)
        self._spawned[descriptor.name] = handle

    def _validate_dag(self, dag: DagModel) -> None:
        node_ids = {node.id for node in dag.nodes}
        for edge in dag.edges:
            if edge.source not in node_ids or edge.target not in node_ids:
                raise ValueError(f"Invalid edge {edge.source}->{edge.target}")
        # Simple cycle detection using DFS
        adjacency: dict[str, list[str]] = {node_id: [] for node_id in node_ids}
        for edge in dag.edges:
            adjacency[edge.source].append(edge.target)

        visited: dict[str, bool] = {}

        def visit(node_id: str, stack: list[str]) -> None:
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
