"""Redis-backed persistence helpers for the agent runtime."""

from __future__ import annotations

import json
from collections.abc import Iterable
from datetime import datetime, timedelta, timezone

from pydantic import ValidationError

from .version import BaseLogger

try:
    import redis
except ImportError as exc:
    raise RuntimeError("redis package is required for the agent runtime") from exc

from .models import DagModel, TaskNode, TaskState, TaskStatus

LocalRedisClient = redis.Redis


def _now_utc() -> datetime:
    """Return the current UTC timestamp."""
    return datetime.now(timezone.utc)


class RedisStore(BaseLogger):
    """Serialization helpers on top of a Redis client."""

    def __init__(self, client: LocalRedisClient):
        """Initialize the store with a Redis client."""
        super().__init__(f"{self.__class__.__name__}")
        self.client = client

    @staticmethod
    def _nodes_key(goal_id: str) -> str:
        """Return the Redis hash key for DAG nodes."""
        return f"dag:{goal_id}:nodes"

    @staticmethod
    def _deps_key(goal_id: str) -> str:
        """Return the Redis hash key for DAG dependency lists."""
        return f"dag:{goal_id}:deps"

    @staticmethod
    def _task_key(task_id: str) -> str:
        """Return the Redis hash key for task state."""
        return f"task:{task_id}"

    @staticmethod
    def _lock_key(task_id: str) -> str:
        """Return the Redis key used for task lease locks."""
        return f"task:{task_id}:lock"

    @staticmethod
    def _agent_registration_key(agent_name: str) -> str:
        """Return the Redis hash key for agent registration data."""
        return f"agent:{agent_name}:registered"

    @staticmethod
    def _agent_heartbeat_key(agent_name: str) -> str:
        """Return the Redis key for agent heartbeat timestamps."""
        return f"agent:{agent_name}:heartbeat"

    @staticmethod
    def _agent_memory_key(agent_name: str) -> str:
        """Return the Redis key for agent memory snapshots."""
        return f"agent:{agent_name}:memory"

    @staticmethod
    def _goal_spending_cap_key(goal_id: str) -> str:
        """Return the Redis key tracking goal-wide spending-cap backoff."""
        return f"goal:{goal_id}:spending_cap_until"

    def write_dag(self, dag: DagModel) -> None:
        """Persist DAG structure and initialize task states."""
        self.logger.debug(
            "Writing DAG for goal %s to Redis (%d nodes, %d edges)",
            dag.goal_id,
            len(dag.nodes),
            len(dag.edges),
        )
        pipeline = self.client.pipeline()
        nodes_key = self._nodes_key(dag.goal_id)
        deps_key = self._deps_key(dag.goal_id)
        pipeline.delete(nodes_key, deps_key)

        for node in dag.nodes:
            pipeline.hset(nodes_key, node.id, node.model_dump_json())
            deps = dag.dependencies_for(node.id)
            pipeline.hset(deps_key, node.id, json.dumps(deps))

            status = TaskStatus.READY if not deps else TaskStatus.BLOCKED
            state = TaskState(status=status)
            pipeline.hset(self._task_key(node.id), mapping=self._serialize_state(state))
        pipeline.execute()

    def read_dag(self, goal_id: str) -> DagModel | None:
        """Load DAG structure for a goal from Redis."""
        nodes_key = self._nodes_key(goal_id)
        deps_key = self._deps_key(goal_id)
        raw_nodes = self.client.hgetall(nodes_key)
        if not raw_nodes:
            return None
        nodes = [TaskNode.model_validate_json(value) for value in raw_nodes.values()]
        edges: list[dict[str, str]] = []
        raw_deps = self.client.hgetall(deps_key)
        for node_id_bytes, deps_json in raw_deps.items():
            node_id = node_id_bytes.decode("utf-8")
            for dep in json.loads(deps_json):
                edges.append({"source": dep, "target": node_id})
        return DagModel(goal_id=goal_id, nodes=nodes, edges=edges)

    def get_task_node(self, goal_id: str, task_id: str) -> TaskNode | None:
        """Load a single task node definition from the DAG."""
        raw = self.client.hget(self._nodes_key(goal_id), task_id)
        if not raw:
            return None
        return TaskNode.model_validate_json(raw)

    def update_task_node(self, goal_id: str, node: TaskNode) -> None:
        """Persist updated node metadata for an existing task."""
        self.client.hset(self._nodes_key(goal_id), node.id, node.model_dump_json())

    def create_remediation_task(
        self,
        *,
        goal_id: str,
        qa_task_id: str,
        dev_role: str,
        review_cycle: int,
        priority: int,
    ) -> str:
        """Append a remediation task that must complete before QA can retry."""
        dag = self.read_dag(goal_id)
        if not dag:
            raise ValueError(f"DAG missing for goal {goal_id}")
        remediation_id = f"{qa_task_id}__fix_{review_cycle}"
        existing_ids = {node.id for node in dag.nodes}
        suffix = 1
        while remediation_id in existing_ids:
            remediation_id = f"{qa_task_id}__fix_{review_cycle}_{suffix}"
            suffix += 1
        node = TaskNode(
            id=remediation_id,
            title=f"Address QA findings for {qa_task_id} (cycle {review_cycle})",
            priority=priority,
            expected_artifacts=["fix-report.md"],
            metadata={
                "phase": "development",
                "required_role": dev_role,
                "parent_task_id": qa_task_id,
                "review_cycle": str(review_cycle),
                "dev_role": dev_role,
            },
        )
        nodes_key = self._nodes_key(goal_id)
        deps_key = self._deps_key(goal_id)
        self.client.hset(nodes_key, remediation_id, node.model_dump_json())
        self.client.hset(deps_key, remediation_id, json.dumps([]))

        qa_deps_raw = self.client.hget(deps_key, qa_task_id)
        qa_deps = json.loads(qa_deps_raw) if qa_deps_raw else []
        if remediation_id not in qa_deps:
            qa_deps.append(remediation_id)
            self.client.hset(deps_key, qa_task_id, json.dumps(qa_deps))

        self.update_task_state(remediation_id, TaskState(status=TaskStatus.READY))
        return remediation_id

    def create_recovery_task(
        self,
        *,
        goal_id: str,
        blocked_task_id: str,
        owner_role: str,
        recovery_attempt: int,
        priority: int,
        title: str,
    ) -> str:
        """Append a recovery task and block the target task until it succeeds."""
        dag = self.read_dag(goal_id)
        if not dag:
            raise ValueError(f"DAG missing for goal {goal_id}")

        recovery_id = f"{blocked_task_id}__recover_{recovery_attempt}"
        existing_ids = {node.id for node in dag.nodes}
        suffix = 1
        while recovery_id in existing_ids:
            recovery_id = f"{blocked_task_id}__recover_{recovery_attempt}_{suffix}"
            suffix += 1

        node = TaskNode(
            id=recovery_id,
            title=title,
            priority=priority,
            expected_artifacts=["recovery-report.md"],
            metadata={
                "phase": "development",
                "required_role": owner_role,
                "parent_task_id": blocked_task_id,
                "recovery_for": blocked_task_id,
                "recovery_attempt": str(recovery_attempt),
            },
        )
        nodes_key = self._nodes_key(goal_id)
        deps_key = self._deps_key(goal_id)
        self.client.hset(nodes_key, recovery_id, node.model_dump_json())
        self.client.hset(deps_key, recovery_id, json.dumps([]))

        blocked_deps_raw = self.client.hget(deps_key, blocked_task_id)
        blocked_deps = json.loads(blocked_deps_raw) if blocked_deps_raw else []
        if recovery_id not in blocked_deps:
            blocked_deps.append(recovery_id)
            self.client.hset(deps_key, blocked_task_id, json.dumps(blocked_deps))

        self.update_task_state(recovery_id, TaskState(status=TaskStatus.READY))
        return recovery_id

    def get_task_state(self, task_id: str) -> TaskState | None:
        """Load mutable execution state for a task."""
        raw = self.client.hgetall(self._task_key(task_id))
        if not raw:
            return None
        normalized = {
            key.decode("utf-8"): value.decode("utf-8")
            for key, value in raw.items()
            if isinstance(value, bytes)
        }
        return self._deserialize_state(normalized)

    def update_task_state(self, task_id: str, state: TaskState) -> None:
        """Replace mutable execution state for a task."""
        key = self._task_key(task_id)
        self.client.delete(key)
        self.client.hset(key, mapping=self._serialize_state(state))

    def list_ready_tasks(self, goal_id: str) -> list[str]:
        """Return claimable tasks whose dependencies are satisfied."""
        dag = self.read_dag(goal_id)
        if not dag:
            self.logger.debug("No DAG found for goal %s", goal_id)
            return []
        recovered = self.recover_stale_tasks(goal_id)
        if recovered:
            self.logger.info(
                "Recovered %d stale running tasks for goal %s: %s",
                len(recovered),
                goal_id,
                ", ".join(sorted(recovered)),
            )
        ready: list[str] = []
        done_count = 0
        blocked_count = 0
        for node in sorted(dag.nodes, key=lambda n: n.priority, reverse=True):
            state = self.get_task_state(node.id)
            if not state:
                continue
            if state.status == TaskStatus.DONE:
                done_count += 1
                continue
            dependencies = dag.dependencies_for(node.id)
            if dependencies and not all(
                (self.get_task_state(dep) or TaskState(status=TaskStatus.READY)).status
                in {
                    TaskStatus.DEV_DONE,
                    TaskStatus.QA_PASSED,
                    TaskStatus.INTEGRATED,
                    TaskStatus.DONE,
                }
                for dep in dependencies
            ):
                blocked_count += 1
                continue
            if state.status in {TaskStatus.READY, TaskStatus.BLOCKED}:
                if state.status == TaskStatus.BLOCKED:
                    state = state.model_copy(update={"status": TaskStatus.READY})
                    self.update_task_state(node.id, state)
                ready.append(node.id)

        self.logger.debug(
            "Goal %s task states: %d ready, %d done, %d blocked (total=%d)",
            goal_id,
            len(ready),
            done_count,
            blocked_count,
            len(dag.nodes),
        )
        return ready

    def recover_stale_tasks(self, goal_id: str) -> list[str]:
        """Reset stale running/claimed tasks whose lock or lease has expired."""
        dag = self.read_dag(goal_id)
        if not dag:
            return []

        recovered: list[str] = []
        now = _now_utc()
        for node in dag.nodes:
            state = self.get_task_state(node.id)
            if not state:
                continue
            if state.status not in {TaskStatus.RUNNING, TaskStatus.CLAIMED}:
                continue

            lock_owner = self.lock_owner(node.id)
            lock_missing = lock_owner is None
            lease_expired = bool(state.lease_expires and state.lease_expires <= now)
            if not lock_missing and not lease_expired:
                continue

            recovered_state = state.model_copy(
                update={"status": TaskStatus.READY, "owner": None, "lease_expires": None}
            )
            self.update_task_state(node.id, recovered_state)
            recovered.append(node.id)

        return recovered

    def acquire_lock(self, task_id: str, agent_name: str, ttl: timedelta) -> bool:
        """Attempt to acquire an expiring lock for a task."""
        key = self._lock_key(task_id)
        acquired = bool(
            self.client.set(key, agent_name, nx=True, px=int(ttl.total_seconds() * 1000))
        )
        if acquired:
            self.logger.debug(
                "Lock acquired: %s by %s (TTL=%.1fs)", task_id, agent_name, ttl.total_seconds()
            )
        else:
            current_owner = self.client.get(key)
            owner_str = (
                current_owner.decode("utf-8") if isinstance(current_owner, bytes) else current_owner
            )
            self.logger.debug(
                "Lock already held: %s by %s", task_id, owner_str if owner_str else "unknown"
            )
        return acquired

    def renew_lock(self, task_id: str, agent_name: str, ttl: timedelta) -> bool:
        """Renew a lock if it is still held by the same agent."""
        key = self._lock_key(task_id)
        value = self.client.get(key)
        owner = value.decode("utf-8") if isinstance(value, bytes) else value
        if owner != agent_name:
            return False
        return bool(self.client.pexpire(key, int(ttl.total_seconds() * 1000)))

    def release_lock(self, task_id: str, agent_name: str) -> None:
        """Release a task lock if owned by the given agent."""
        key = self._lock_key(task_id)
        value = self.client.get(key)
        owner = value.decode("utf-8") if isinstance(value, bytes) else value
        if owner == agent_name:
            self.client.delete(key)

    def lock_owner(self, task_id: str) -> str | None:
        """Return the current lock owner for a task, if any."""
        value = self.client.get(self._lock_key(task_id))
        if not value:
            return None
        return value.decode("utf-8") if isinstance(value, bytes) else str(value)

    def register_agent(
        self, agent_name: str, pid: int, capabilities: dict[str, str] | None = None
    ) -> None:
        """Record agent process registration metadata."""
        payload = {
            "pid": str(pid),
            "timestamp": _now_utc().isoformat(),
        }
        if capabilities:
            payload.update({f"cap:{k}": v for k, v in capabilities.items()})
        self.client.hset(self._agent_registration_key(agent_name), mapping=payload)

    def heartbeat_agent(self, agent_name: str, ttl: timedelta) -> None:
        """Write and TTL-protect the latest agent heartbeat."""
        key = self._agent_heartbeat_key(agent_name)
        self.client.set(key, _now_utc().isoformat(), px=int(ttl.total_seconds() * 1000))

    def agent_last_heartbeat(self, agent_name: str) -> datetime | None:
        """Return the last heartbeat timestamp for an agent."""
        value = self.client.get(self._agent_heartbeat_key(agent_name))
        if not value:
            return None
        timestamp = value.decode("utf-8") if isinstance(value, bytes) else str(value)
        return datetime.fromisoformat(timestamp)

    def deregister_agent(self, agent_name: str) -> None:
        """Remove registration and heartbeat records for an agent."""
        self.client.delete(self._agent_registration_key(agent_name))
        self.client.delete(self._agent_heartbeat_key(agent_name))

    def store_memory_snapshot(self, agent_name: str, entries: Iterable[str]) -> None:
        """Persist an agent memory snapshot."""
        key = self._agent_memory_key(agent_name)
        serialized = json.dumps(list(entries))
        self.client.set(key, serialized)

    def load_memory_snapshot(self, agent_name: str) -> list[str]:
        """Load an agent memory snapshot."""
        value = self.client.get(self._agent_memory_key(agent_name))
        if not value:
            return []
        raw = value.decode("utf-8") if isinstance(value, bytes) else str(value)
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return []

    def set_goal_spending_cap_until(self, goal_id: str, until: datetime) -> None:
        """Persist goal-wide spending-cap backoff deadline."""
        now = _now_utc()
        if until.tzinfo is None:
            until = until.replace(tzinfo=timezone.utc)
        else:
            until = until.astimezone(timezone.utc)
        if until <= now:
            self.clear_goal_spending_cap(goal_id)
            return

        current_until = self.get_goal_spending_cap_until(goal_id)
        if current_until and current_until >= until:
            return

        ttl_ms = int(max(1000, (until - now).total_seconds() * 1000))
        self.client.set(self._goal_spending_cap_key(goal_id), until.isoformat(), px=ttl_ms)

    def get_goal_spending_cap_until(self, goal_id: str) -> datetime | None:
        """Return active goal-wide spending-cap deadline, if present."""
        value = self.client.get(self._goal_spending_cap_key(goal_id))
        if not value:
            return None
        raw = value.decode("utf-8") if isinstance(value, bytes) else str(value)
        try:
            until = datetime.fromisoformat(raw)
        except ValueError:
            self.clear_goal_spending_cap(goal_id)
            return None
        if until.tzinfo is None:
            until = until.replace(tzinfo=timezone.utc)
        else:
            until = until.astimezone(timezone.utc)
        if until <= _now_utc():
            self.clear_goal_spending_cap(goal_id)
            return None
        return until

    def clear_goal_spending_cap(self, goal_id: str) -> None:
        """Clear goal-wide spending-cap backoff state."""
        self.client.delete(self._goal_spending_cap_key(goal_id))

    @staticmethod
    def _serialize_state(state: TaskState) -> dict[str, str]:
        """Convert task state to a Redis hash mapping."""
        payload = state.model_dump(mode="json")
        return {
            key: json.dumps(value) if isinstance(value, (dict, list)) else (value or "")
            for key, value in payload.items()
            if value is not None
        }

    @staticmethod
    def _deserialize_state(raw: dict[str, str]) -> TaskState:
        """Parse Redis hash mapping into task state."""
        normalized: dict[str, object] = {}
        for key, value in raw.items():
            if key in {"artifacts"}:
                normalized[key] = json.loads(value or "[]")
            elif key in {"lease_expires"} and value:
                normalized[key] = datetime.fromisoformat(value)
            elif key == "status":
                normalized[key] = TaskStatus(value)
            elif key in {"owner", "progress", "last_error"}:
                normalized[key] = value or None
        try:
            return TaskState(**normalized)
        except ValidationError as exc:
            raise ValueError(f"Invalid task state payload: {raw}") from exc
