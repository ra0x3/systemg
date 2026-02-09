"""Implements the single-process agent runtime."""

from __future__ import annotations

import os
import time
from collections.abc import Iterable
from datetime import datetime, timedelta, timezone
from pathlib import Path

from .cache import RedisStore
from .heartbeat import HeartbeatController, HeartbeatDirective
from .llm import LLMClient, TaskExecutionResult
from .memory import Memory
from .models import DEFAULT_LEASE_TTL, TaskNode, TaskState, TaskStatus
from .version import BaseLogger, InstructionStore


class AgentRuntime(BaseLogger):
    """Coordinates local memory, heartbeat control, and Redis state."""

    def __init__(
        self,
        *,
        agent_name: str,
        goal_id: str,
        instructions_path: Path,
        heartbeat_path: Path,
        redis_store: RedisStore,
        llm_client: LLMClient | None = None,
        loop_interval: float = 1.0,
        lease_ttl: timedelta = DEFAULT_LEASE_TTL,
        instructions_refresh_interval: timedelta = timedelta(seconds=10),
    ) -> None:
        super().__init__(f"{self.__class__.__name__}[{agent_name}]")
        self.agent_name = agent_name
        self.goal_id = goal_id
        self.instructions_path = instructions_path
        self.heartbeat_controller = HeartbeatController(heartbeat_path)
        self.redis_store = redis_store
        if llm_client is None:
            raise ValueError("AgentRuntime requires an LLM client instance")
        self.llm_client = llm_client
        self.loop_interval = loop_interval
        self.lease_ttl = lease_ttl
        self.instructions_refresh_interval = instructions_refresh_interval

        self.memory = Memory()
        self.instructions_text = ""
        self.instruction_store = InstructionStore(redis_store.client)
        self.instruction_id: str | None = None
        self._active = True
        self._paused = False
        self._last_reload = datetime.fromtimestamp(0, tz=timezone.utc)

        self._hydrate_memory()

    def _hydrate_memory(self) -> None:
        snapshot = self.redis_store.load_memory_snapshot(self.agent_name)
        if snapshot:
            self.logger.debug("Hydrating memory with %d entries", len(snapshot))
            self.memory.hydrate(snapshot)

    def reload_instructions(self) -> None:
        if self.instructions_path.exists():
            file_instructions = self.instructions_path.read_text(encoding="utf-8")

            if not self.instruction_id:
                self.instruction_id = f"{self.agent_name}:{self.goal_id}"

            latest_version = self.instruction_store.get_latest(self.instruction_id)
            if not latest_version or latest_version.instructions != file_instructions:
                self.instruction_store.push_version(file_instructions, self.instruction_id)
                self.logger.info("Pushed new instruction version for ID: %s", self.instruction_id)

            version = self.instruction_store.get_latest(self.instruction_id)
            if version:
                self.instructions_text = version.instructions
                self.memory.append(
                    f"Loaded instruction version {version.hash[:8]} at {datetime.now(timezone.utc).isoformat()}"
                )
                self.logger.info(
                    "Instructions loaded from store (ID: %s, hash: %s)",
                    self.instruction_id,
                    version.hash[:8],
                )
            else:
                self.instructions_text = file_instructions
                self.logger.warning("Failed to load from store, using file directly")
        else:
            self.instructions_text = ""
            self.logger.warning("Instructions file missing: %s", self.instructions_path)
        self._last_reload = datetime.now(timezone.utc)

    def poll_heartbeat(self) -> list[HeartbeatDirective]:
        directives = self.heartbeat_controller.consume()
        for directive in directives:
            self.logger.info("Received heartbeat directive: %s", directive)
        return directives

    def apply_directives(self, directives: Iterable[HeartbeatDirective]) -> None:
        for directive in directives:
            if directive.command == "PAUSE":
                self._paused = True
            elif directive.command == "RESUME":
                self._paused = False
            elif directive.command == "REPARSE":
                self.reload_instructions()
            elif directive.command == "DROP-TASK" and directive.args:
                self._drop_task(directive.args[0])
            elif directive.command == "ELEVATE" and len(directive.args) >= 2:
                self._elevate_task(directive.args[0], directive.args[1])
            elif directive.command == "FLUSH-MEMORY":
                self.memory.hydrate([])
                self.redis_store.store_memory_snapshot(self.agent_name, [])

    def _drop_task(self, task_id: str) -> None:
        self.logger.info("Dropping task %s on operator request", task_id)
        state = self.redis_store.get_task_state(task_id)
        if not state:
            return
        self.redis_store.update_task_state(
            task_id, state.model_copy(update={"status": TaskStatus.READY, "owner": None})
        )
        self.redis_store.release_lock(task_id, self.agent_name)

    def _elevate_task(self, task_id: str, priority: str) -> None:
        dag = self.redis_store.read_dag(self.goal_id)
        if not dag:
            return
        for node in dag.nodes:
            if node.id == task_id:
                try:
                    node.priority = int(priority)
                except ValueError:
                    self.logger.warning("Invalid priority %s", priority)
                break
        self.redis_store.write_dag(dag)

    def heartbeat(self) -> None:
        self.redis_store.heartbeat_agent(self.agent_name, ttl=self.lease_ttl)

    def run(self, *, max_cycles: int | None = None) -> None:
        pid = os.getpid()
        self.logger.info(
            "[%s] Starting agent (PID=%d) for goal %s", self.agent_name, pid, self.goal_id
        )
        self.redis_store.register_agent(self.agent_name, pid)
        self.reload_instructions()
        cycles = 0
        self.logger.info(
            "[%s] Agent initialized, entering main loop (max_cycles=%s)",
            self.agent_name,
            max_cycles if max_cycles else "unlimited",
        )

        while self._active and (max_cycles is None or cycles < max_cycles):
            directives = self.poll_heartbeat()
            self.apply_directives(directives)
            self.heartbeat()
            if not self._paused:
                if (
                    datetime.now(timezone.utc) - self._last_reload
                    >= self.instructions_refresh_interval
                ):
                    self.reload_instructions()
                self._run_cycle()
            else:
                self.logger.debug("[%s] Agent paused, skipping cycle %d", self.agent_name, cycles)
            cycles += 1
            if self.loop_interval > 0:
                time.sleep(self.loop_interval)

        self.logger.info("[%s] Agent shutting down after %d cycles", self.agent_name, cycles)
        memory_snapshot = self.memory.snapshot()
        self.logger.info(
            "[%s] Storing final memory snapshot (%d entries)", self.agent_name, len(memory_snapshot)
        )
        self.redis_store.store_memory_snapshot(self.agent_name, memory_snapshot)
        self.redis_store.deregister_agent(self.agent_name)
        self.logger.info("[%s] Agent shutdown complete", self.agent_name)

    def stop(self) -> None:
        self._active = False

    def _run_cycle(self) -> None:
        ready_task_ids = self.redis_store.list_ready_tasks(self.goal_id)
        if not ready_task_ids:
            self.logger.info("[%s] No ready tasks for goal %s", self.agent_name, self.goal_id)
            return

        self.logger.info(
            "[%s] Found %d ready tasks for goal %s",
            self.agent_name,
            len(ready_task_ids),
            self.goal_id,
        )
        ready_nodes = []
        for task_id in ready_task_ids:
            node = self.redis_store.get_task_node(self.goal_id, task_id)
            if node:
                ready_nodes.append(node)
                self.logger.debug(
                    "[%s]   Ready task: %s (priority=%d)", self.agent_name, node.id, node.priority
                )

        self.logger.info(
            "[%s] Asking LLM to select from %d tasks", self.agent_name, len(ready_nodes)
        )
        selection = self.llm_client.select_next_task(
            ready_nodes,
            memory=self.memory,
            goal_id=self.goal_id,
            instructions=self.instructions_text,
        )
        if not selection.selected_task_id:
            self.logger.info(
                "[%s] LLM declined to select a task: %s", self.agent_name, selection.justification
            )
            return

        self.logger.info(
            "[%s] LLM selected task %s (confidence=%.2f): %s",
            self.agent_name,
            selection.selected_task_id,
            selection.confidence,
            selection.justification,
        )

        node = self.redis_store.get_task_node(self.goal_id, selection.selected_task_id)
        if not node:
            self.logger.warning(
                "[%s] Selected task %s missing from DAG",
                self.agent_name,
                selection.selected_task_id,
            )
            return

        if not self.redis_store.acquire_lock(node.id, self.agent_name, self.lease_ttl):
            self.logger.info(
                "[%s] Could not acquire lock for %s (another agent has it)",
                self.agent_name,
                node.id,
            )
            return
        try:
            self.logger.info(
                "[%s] Acquired lock for task %s: %s", self.agent_name, node.id, node.title
            )
            state = self.redis_store.get_task_state(node.id) or TaskState(status=TaskStatus.READY)
            lease_expires = datetime.now(timezone.utc) + self.lease_ttl
            state = state.as_running(owner=self.agent_name, lease_expires=lease_expires)
            self.redis_store.update_task_state(node.id, state)

            self.logger.info("[%s] Executing task %s with LLM", self.agent_name, node.id)
            execution = self.llm_client.execute_task(
                node,
                goal_id=self.goal_id,
                instructions=self.instructions_text,
                memory=self.memory,
            )
            self.logger.info(
                "[%s] Task %s execution result: status=%s, outputs=%s, notes=%s",
                self.agent_name,
                node.id,
                execution.status,
                execution.outputs,
                execution.notes[:200] if execution.notes else "None",
            )

            if execution.follow_ups:
                self.logger.info(
                    "[%s] Task %s suggested follow-ups: %s",
                    self.agent_name,
                    node.id,
                    execution.follow_ups,
                )

            self.logger.info("[%s] Generating summary for task %s", self.agent_name, node.id)
            summary = self.llm_client.summarize_task(
                node,
                execution,
                goal_id=self.goal_id,
                instructions=self.instructions_text,
                memory=self.memory,
            )
            self.logger.info("[%s] Task %s summary: %s", self.agent_name, node.id, summary[:200])

            self._record_success(node, execution, summary)
        except Exception as exc:  # pragma: no cover - defensive guard
            self.logger.error("[%s] Task %s failed: %s", self.agent_name, node.id, exc)
            self.logger.exception("Full exception details:")
            self._record_failure(node, str(exc))
        finally:
            self.logger.info("[%s] Releasing lock for task %s", self.agent_name, node.id)
            self.redis_store.release_lock(node.id, self.agent_name)

    def _record_success(self, node: TaskNode, execution: TaskExecutionResult, summary: str) -> None:
        state = TaskState(status=TaskStatus.DONE, progress=summary, artifacts=execution.outputs)
        self.redis_store.update_task_state(node.id, state)
        self.memory.append(f"Completed {node.id}: {summary}")
        self.redis_store.store_memory_snapshot(self.agent_name, self.memory.snapshot())

    def _record_failure(self, node: TaskNode, error: str) -> None:
        state = TaskState(status=TaskStatus.FAILED, last_error=error)
        self.redis_store.update_task_state(node.id, state)
        self.memory.append(f"Failed {node.id}: {error}")
        self.redis_store.store_memory_snapshot(self.agent_name, self.memory.snapshot())
