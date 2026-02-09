"""LLM interaction clients for orchestrator and agents."""

from __future__ import annotations

import json
import logging
import os
import subprocess
from collections.abc import Iterable
from dataclasses import dataclass

from .models import DagModel, TaskNode

logger = logging.getLogger(__name__)


@dataclass
class TaskSelection:
    selected_task_id: str | None
    justification: str
    confidence: float = 0.5


@dataclass
class TaskExecutionResult:
    status: str
    outputs: list[str]
    notes: str
    follow_ups: list[str]


class LLMClient:
    """Abstract interface for LLM interactions."""

    def create_goal_dag(
        self, instructions: str, *, goal_id: str
    ) -> DagModel:  # pragma: no cover - abstract
        raise NotImplementedError

    def select_next_task(
        self,
        ready_nodes: Iterable[TaskNode],
        *,
        memory: Iterable[str],
        goal_id: str,
        instructions: str,
    ) -> TaskSelection:  # pragma: no cover - abstract
        raise NotImplementedError

    def execute_task(
        self,
        task: TaskNode,
        *,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> TaskExecutionResult:  # pragma: no cover - abstract
        raise NotImplementedError

    def summarize_task(
        self,
        task: TaskNode,
        execution: TaskExecutionResult,
        *,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> str:  # pragma: no cover - abstract
        raise NotImplementedError


class ClaudeCLIClient(LLMClient):
    """Interact with the Claude CLI via subprocess calls."""

    def __init__(
        self,
        *,
        executable: str = "claude",
        extra_args: list[str] | None = None,
        use_sysg_spawn: bool = False,
    ) -> None:
        self.executable = executable
        self.extra_args = extra_args or []
        self.use_sysg_spawn = use_sysg_spawn

    def create_goal_dag(self, instructions: str, *, goal_id: str) -> DagModel:
        prompt = self._render_prompt(
            "You must derive a task DAG for the provided goal.",
            goal_id=goal_id,
            instructions=instructions,
            schema={
                "goal_id": goal_id,
                "nodes": [
                    {
                        "id": "task-001",
                        "title": "Describe the task",
                        "priority": 10,
                        "expected_artifacts": ["artifact/example"],
                        "metadata": {},
                    }
                ],
                "edges": [{"source": "task-001", "target": "task-002"}],
            },
        )
        payload = self._parse_json(self._invoke(prompt))
        payload.setdefault("goal_id", goal_id)
        return DagModel.model_validate(payload)

    def select_next_task(
        self,
        ready_nodes: Iterable[TaskNode],
        *,
        memory: Iterable[str],
        goal_id: str,
        instructions: str,
    ) -> TaskSelection:
        prompt = self._render_prompt(
            "Given the ready tasks and memory, choose the next task to execute.",
            goal_id=goal_id,
            instructions=instructions,
            context={
                "ready_tasks": [node.model_dump() for node in ready_nodes],
                "memory": list(memory),
            },
            schema={
                "selected_task_id": "task id or null",
                "justification": "reason",
                "confidence": 0.5,
            },
        )
        payload = self._parse_json(self._invoke(prompt))
        return TaskSelection(
            selected_task_id=payload.get("selected_task_id"),
            justification=payload.get("justification", ""),
            confidence=float(payload.get("confidence", 0.5)),
        )

    def execute_task(
        self,
        task: TaskNode,
        *,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> TaskExecutionResult:
        prompt = self._render_prompt(
            "Plan concrete steps to execute the specified task and describe resulting artifacts.",
            goal_id=goal_id,
            instructions=instructions,
            context={
                "task": task.model_dump(),
                "memory": list(memory),
            },
            schema={
                "status": "done|failed|blocked",
                "outputs": ["artifact-path"],
                "notes": "execution notes",
                "follow_ups": ["task-id"],
            },
        )
        payload = self._parse_json(self._invoke(prompt))
        return TaskExecutionResult(
            status=payload.get("status", "done"),
            outputs=list(payload.get("outputs", [])),
            notes=payload.get("notes", ""),
            follow_ups=list(payload.get("follow_ups", [])),
        )

    def summarize_task(
        self,
        task: TaskNode,
        execution: TaskExecutionResult,
        *,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> str:
        prompt = self._render_prompt(
            "Produce a concise summary (<=3 sentences) of the completed task for logging.",
            goal_id=goal_id,
            instructions=instructions,
            context={
                "task": {"id": task.id, "title": task.title},
                "execution": execution.__dict__,
                "memory": list(memory),
            },
            schema={"summary": "Concise summary text"},
        )
        payload = self._parse_json(self._invoke(prompt))
        summary = str(payload.get("summary", "")).strip()
        if not summary:
            raise ValueError("Claude CLI returned empty summary")
        return summary

    # ------------------------------------------------------------------
    def _render_prompt(
        self,
        task: str,
        *,
        goal_id: str,
        instructions: str,
        context: dict | None = None,
        schema: dict | None = None,
    ) -> str:
        sections = [
            task,
            f"Goal ID: {goal_id}",
            f"Instructions:\n{instructions.strip() or 'No instructions provided.'}",
        ]
        if context:
            sections.append(f"Context:\n{json.dumps(context, indent=2)}")
        if schema:
            sections.append(
                "Respond with strict JSON following this structure:\n"
                f"{json.dumps(schema, indent=2)}\n"
                "Output MUST be valid JSON with exactly these keys. Do not include commentary or code fences."
            )
        else:
            sections.append(
                "Respond succinctly. Output must be plain text without commentary or code fences."
            )
        return "\n\n".join(sections)

    def _invoke(self, prompt: str, *, timeout: int = 180) -> str:
        cmd = [self.executable]
        # Add permissions skip flag for claude CLI
        if self.executable == "claude" or self.executable.endswith("/claude"):
            cmd.append("--dangerously-skip-permissions")
        cmd.extend(["-p", prompt])
        if self.extra_args:
            cmd.extend(self.extra_args)
        env = os.environ.copy()

        if self.use_sysg_spawn:
            cmd = [
                "sysg",
                "spawn",
                "--name",
                f"claude-{os.getpid()}",
                "--parent-pid",
                str(os.getpid()),
                "--",
                *cmd,
            ]

        logger.debug(f"Executing command: {' '.join(cmd)}")
        logger.debug(f"Prompt length: {len(prompt)} chars")

        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                check=False,
                timeout=timeout,
                env=env,
            )
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError(f"Claude CLI timed out after {timeout}s: {cmd}") from exc

        logger.debug(f"Command exit code: {result.returncode}")
        logger.debug(f"stdout: {result.stdout[:500]}...")
        logger.debug(f"stderr: {result.stderr[:500]}...")

        if result.returncode != 0:
            raise RuntimeError(
                f"Claude CLI failed: {result.stderr.strip() or result.stdout.strip()}"
            )
        return result.stdout.strip()

    @staticmethod
    def _parse_json(text: str) -> dict:
        text = text.strip()
        try:
            return json.loads(text)
        except json.JSONDecodeError as exc:
            start = text.find("{")
            end = text.rfind("}")
            if start != -1 and end != -1 and end > start:
                candidate = text[start : end + 1]
                try:
                    return json.loads(candidate)
                except json.JSONDecodeError:
                    pass
            raise ValueError(f"Failed to parse Claude CLI output as JSON: {text}") from exc


class StubLLMClient(LLMClient):
    """Heuristic client for tests."""

    def __init__(self, dag_blueprint: DagModel | None = None):
        self.dag_blueprint = dag_blueprint

    def create_goal_dag(self, instructions: str, *, goal_id: str) -> DagModel:
        if self.dag_blueprint:
            return self.dag_blueprint
        nodes: list[TaskNode] = []
        edges: list[dict] = []
        for idx, line in enumerate(instructions.splitlines()):
            line = line.strip()
            if not line.startswith("-"):
                continue
            title = line.lstrip("- ") or f"Step {idx + 1}"
            node_id = f"task-{idx + 1:03d}"
            nodes.append(TaskNode(id=node_id, title=title, priority=len(nodes)))
            if len(nodes) > 1:
                edges.append({"source": nodes[-2].id, "target": node_id})
        if not nodes:
            nodes.append(TaskNode(id="task-001", title="Bootstrap goal", priority=0))
        return DagModel(goal_id=goal_id, nodes=nodes, edges=[dict(edge) for edge in edges])

    def select_next_task(
        self,
        ready_nodes: Iterable[TaskNode],
        *,
        memory: Iterable[str],
        goal_id: str,
        instructions: str,
    ) -> TaskSelection:
        first = next(iter(ready_nodes), None)
        if not first:
            return TaskSelection(selected_task_id=None, justification="No ready tasks")
        return TaskSelection(
            selected_task_id=first.id, justification="Highest priority ready node", confidence=0.9
        )

    def execute_task(
        self,
        task: TaskNode,
        *,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> TaskExecutionResult:
        outputs = [f"artifact://{task.id}.txt"]
        notes = f"Executed {task.title} for goal {goal_id}"
        return TaskExecutionResult(status="done", outputs=outputs, notes=notes, follow_ups=[])

    def summarize_task(
        self,
        task: TaskNode,
        execution: TaskExecutionResult,
        *,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> str:
        return (
            f"Task {task.id} completed with outputs {execution.outputs}. Notes: {execution.notes}"
        )
