"""Redis-backed persistence helpers for the agent runtime."""

from __future__ import annotations

import json
from collections.abc import Iterable
from datetime import datetime, timedelta, timezone

from pydantic import ValidationError

from .version import BaseLogger

try:
    import redis  # type: ignore # noqa: F401
except ImportError as exc:  # pragma: no cover - redis installed via uv
    raise RuntimeError("redis package is required for the agent runtime") from exc

from .models import DagModel, TaskNode, TaskState, TaskStatus

LocalRedisClient = "redis.Redis"


def _now_utc() -> datetime:
    return datetime.now(timezone.utc)


class RedisStore(BaseLogger):
    """Serialization helpers on top of a Redis client."""

    def __init__(self, client: LocalRedisClient):
        super().__init__(f"{self.__class__.__name__}")
        self.client = client

    @staticmethod
    def _nodes_key(goal_id: str) -> str:
        return f"dag:{goal_id}:nodes"

    @staticmethod
    def _deps_key(goal_id: str) -> str:
        return f"dag:{goal_id}:deps"

    @staticmethod
    def _task_key(task_id: str) -> str:
        return f"task:{task_id}"

    @staticmethod
    def _lock_key(task_id: str) -> str:
        return f"task:{task_id}:lock"

    @staticmethod
    def _agent_registration_key(agent_name: str) -> str:
        return f"agent:{agent_name}:registered"

    @staticmethod
    def _agent_heartbeat_key(agent_name: str) -> str:
        return f"agent:{agent_name}:heartbeat"

    @staticmethod
    def _agent_memory_key(agent_name: str) -> str:
        return f"agent:{agent_name}:memory"

    def write_dag(self, dag: DagModel) -> None:
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
        raw = self.client.hget(self._nodes_key(goal_id), task_id)
        if not raw:
            return None
        return TaskNode.model_validate_json(raw)

    def get_task_state(self, task_id: str) -> TaskState | None:
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
        self.client.hset(self._task_key(task_id), mapping=self._serialize_state(state))

    def list_ready_tasks(self, goal_id: str) -> list[str]:
        dag = self.read_dag(goal_id)
        if not dag:
            self.logger.debug("No DAG found for goal %s", goal_id)
            return []
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
                == TaskStatus.DONE
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

    def acquire_lock(self, task_id: str, agent_name: str, ttl: timedelta) -> bool:
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
        key = self._lock_key(task_id)
        value = self.client.get(key)
        owner = value.decode("utf-8") if isinstance(value, bytes) else value
        if owner != agent_name:
            return False
        return bool(self.client.pexpire(key, int(ttl.total_seconds() * 1000)))

    def release_lock(self, task_id: str, agent_name: str) -> None:
        key = self._lock_key(task_id)
        value = self.client.get(key)
        owner = value.decode("utf-8") if isinstance(value, bytes) else value
        if owner == agent_name:
            self.client.delete(key)

    def lock_owner(self, task_id: str) -> str | None:
        value = self.client.get(self._lock_key(task_id))
        if not value:
            return None
        return value.decode("utf-8") if isinstance(value, bytes) else str(value)

    def register_agent(
        self, agent_name: str, pid: int, capabilities: dict[str, str] | None = None
    ) -> None:
        payload = {
            "pid": str(pid),
            "timestamp": _now_utc().isoformat(),
        }
        if capabilities:
            payload.update({f"cap:{k}": v for k, v in capabilities.items()})
        self.client.hset(self._agent_registration_key(agent_name), mapping=payload)

    def heartbeat_agent(self, agent_name: str, ttl: timedelta) -> None:
        key = self._agent_heartbeat_key(agent_name)
        self.client.set(key, _now_utc().isoformat(), px=int(ttl.total_seconds() * 1000))

    def agent_last_heartbeat(self, agent_name: str) -> datetime | None:
        value = self.client.get(self._agent_heartbeat_key(agent_name))
        if not value:
            return None
        timestamp = value.decode("utf-8") if isinstance(value, bytes) else str(value)
        return datetime.fromisoformat(timestamp)

    def deregister_agent(self, agent_name: str) -> None:
        self.client.delete(self._agent_registration_key(agent_name))
        self.client.delete(self._agent_heartbeat_key(agent_name))

    def store_memory_snapshot(self, agent_name: str, entries: Iterable[str]) -> None:
        key = self._agent_memory_key(agent_name)
        serialized = json.dumps(list(entries))
        self.client.set(key, serialized)

    def load_memory_snapshot(self, agent_name: str) -> list[str]:
        value = self.client.get(self._agent_memory_key(agent_name))
        if not value:
            return []
        raw = value.decode("utf-8") if isinstance(value, bytes) else str(value)
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return []

    @staticmethod
    def _serialize_state(state: TaskState) -> dict[str, str]:
        payload = state.model_dump(mode="json")
        return {
            key: json.dumps(value) if isinstance(value, (dict, list)) else (value or "")
            for key, value in payload.items()
            if value is not None
        }

    @staticmethod
    def _deserialize_state(raw: dict[str, str]) -> TaskState:
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
        except ValidationError as exc:  # pragma: no cover - guardrail
            raise ValueError(f"Invalid task state payload: {raw}") from exc
