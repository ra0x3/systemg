"""Typed domain models for the systemg agent runtime."""

from __future__ import annotations

from collections.abc import Iterable, Sequence
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from enum import Enum
from pathlib import Path

from pydantic import BaseModel, Field, RootModel, field_validator, model_validator


class TaskStatus(str, Enum):
    READY = "ready"
    CLAIMED = "claimed"
    RUNNING = "running"
    BLOCKED = "blocked"
    DEV_DONE = "dev_done"
    QA_FAILED = "qa_failed"
    QA_PASSED = "qa_passed"
    INTEGRATED = "integrated"
    DONE = "done"
    FAILED = "failed"


class TaskNode(BaseModel):
    id: str
    title: str
    priority: int = Field(ge=0)
    expected_artifacts: list[str] = Field(default_factory=list)
    metadata: dict[str, str] = Field(default_factory=dict)


class TaskEdge(BaseModel):
    source: str
    target: str


class DagModel(BaseModel):
    goal_id: str
    nodes: list[TaskNode]
    edges: list[TaskEdge]

    @model_validator(mode="after")
    def validate_nodes_for_edges(self):  # type: ignore[override]
        """Ensure every edge references existing node IDs."""
        node_ids = {node.id for node in self.nodes}
        for edge in self.edges:
            if edge.source not in node_ids:
                raise ValueError(f"Edge source {edge.source} missing from DAG nodes")
            if edge.target not in node_ids:
                raise ValueError(f"Edge target {edge.target} missing from DAG nodes")
        return self

    def dependencies_for(self, node_id: str) -> list[str]:
        """Return upstream dependencies for a node."""
        return [edge.source for edge in self.edges if edge.target == node_id]


class TaskState(BaseModel):
    status: TaskStatus
    owner: str | None = None
    lease_expires: datetime | None = None
    progress: str | None = None
    artifacts: list[str] = Field(default_factory=list)
    last_error: str | None = None

    def with_owner(self, owner: str, lease_expires: datetime) -> TaskState:
        """Return a copy updated with ownership metadata."""
        return self.model_copy(update={"owner": owner, "lease_expires": lease_expires})

    def as_running(self, owner: str, lease_expires: datetime) -> TaskState:
        """Return a copy marked as running for an owner."""
        return self.model_copy(
            update={"status": TaskStatus.RUNNING, "owner": owner, "lease_expires": lease_expires}
        )

    def as_done(self, progress: str, artifacts: Sequence[str] | None = None) -> TaskState:
        """Return a copy marked as done with outputs."""
        return self.model_copy(
            update={
                "status": TaskStatus.DONE,
                "progress": progress,
                "artifacts": list(artifacts or []),
                "lease_expires": None,
                "owner": None,
            }
        )

    def as_failed(self, error: str) -> TaskState:
        """Return a copy marked as failed with error text."""
        return self.model_copy(
            update={
                "status": TaskStatus.FAILED,
                "last_error": error,
                "owner": None,
                "lease_expires": None,
            }
        )


class GoalDescriptor(BaseModel):
    goal_id: str
    title: str
    priority: int = Field(ge=0, default=0)
    status: TaskStatus = TaskStatus.READY


class AgentDescriptor(BaseModel):
    name: str
    role: str | None = None
    goal_id: str
    instructions_path: Path
    heartbeat_path: Path
    log_level: str = Field(default="INFO")
    cadence_seconds: int = Field(default=5, ge=1)

    @field_validator("instructions_path", "heartbeat_path", mode="before")
    def _coerce_path(cls, value):  # type: ignore[override]
        """Coerce path-like values into `Path` instances."""
        return Path(value)

    def cname(self) -> str:
        """Return canonical name for this descriptor."""
        return f"{self.name}:{self.goal_id}"

    @property
    def effective_role(self) -> str:
        """Return the declared role, defaulting to agent name."""
        return self.role or self.name


class InstructionSet(RootModel[list[str]]):
    """Simple wrapper for instruction lines."""

    def as_text(self) -> str:
        """Join instruction lines into one text block."""
        return "\n".join(self.root)


@dataclass
class MemorySnapshot:
    entries: list[str] = field(default_factory=list)

    def append(self, entry: str, *, max_entries: int = 50) -> None:
        """Append entry with bounded retention."""
        self.entries.append(entry)
        if len(self.entries) > max_entries:
            excess = len(self.entries) - max_entries
            del self.entries[0:excess]

    def merge(self, other: Iterable[str]) -> None:
        """Append entries from another iterable snapshot."""
        for item in other:
            self.append(item)


DEFAULT_LEASE_TTL = timedelta(seconds=30)
