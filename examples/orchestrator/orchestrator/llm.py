"""LLM interaction clients for orchestrator and agents."""

from __future__ import annotations

import json
import logging
import os
import subprocess
import time
from hashlib import sha256
from collections.abc import Iterable
from dataclasses import dataclass

from .models import DagModel, TaskNode

logger = logging.getLogger(__name__)

try:
    import tiktoken
except ImportError:  # pragma: no cover - optional dependency
    tiktoken = None  # type: ignore[assignment]


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


@dataclass(frozen=True)
class Prompt:
    id: str
    text: str
    char_count: int
    token_estimate: int
    tokenizer: str

    @classmethod
    def from_text(cls, text: str) -> Prompt:
        """Build a prompt descriptor with stable ID and token estimate."""
        prompt_id = sha256(text.encode("utf-8")).hexdigest()[:12]
        char_count = len(text)
        token_estimate, tokenizer = _estimate_token_count(text)
        return cls(
            id=prompt_id,
            text=text,
            char_count=char_count,
            token_estimate=token_estimate,
            tokenizer=tokenizer,
        )


def _estimate_token_count(text: str) -> tuple[int, str]:
    """Estimate token count using tiktoken when available."""
    if tiktoken is not None:
        encoding = tiktoken.get_encoding("cl100k_base")
        return len(encoding.encode(text)), "tiktoken:cl100k_base"
    # Simple fallback heuristic when tiktoken is unavailable.
    return max(1, len(text) // 4), "fallback:chars_div_4"


class LLMClient:
    """Abstract interface for LLM interactions."""

    def create_goal_dag(
        self, instructions: str, *, goal_id: str
    ) -> DagModel:  # pragma: no cover - abstract
        """Create a DAG model for the goal from instructions."""
        raise NotImplementedError

    def select_next_task(
        self,
        ready_nodes: Iterable[TaskNode],
        *,
        memory: Iterable[str],
        goal_id: str,
        instructions: str,
    ) -> TaskSelection:  # pragma: no cover - abstract
        """Select the next task from currently ready nodes."""
        raise NotImplementedError

    def execute_task(
        self,
        task: TaskNode,
        *,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> TaskExecutionResult:  # pragma: no cover - abstract
        """Execute a task and return structured execution output."""
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
        """Summarize task execution for progress tracking."""
        raise NotImplementedError


class ClaudeCLIClient(LLMClient):
    """Interact with the Claude CLI via subprocess calls."""

    EXECUTION_TIMEOUT_SECONDS = 300
    PROGRESS_LOG_INTERVAL_SECONDS = 30

    def __init__(
        self,
        *,
        executable: str = "claude",
        extra_args: list[str] | None = None,
        use_sysg_spawn: bool = False,
    ) -> None:
        """Configure Claude CLI invocation parameters."""
        self.executable = executable
        self.extra_args = extra_args or []
        self.use_sysg_spawn = use_sysg_spawn

    def create_goal_dag(self, instructions: str, *, goal_id: str) -> DagModel:
        """Request a DAG from Claude and validate it."""
        schema = {
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
        }
        prompt = self._render_prompt(
            "You must derive a task DAG for the provided goal.",
            goal_id=goal_id,
            instructions=instructions,
            schema=schema,
        )
        payload = self._invoke_json_with_retries(
            prompt,
            required_keys=set(schema.keys()),
            operation="create_goal_dag",
        )
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
        """Request next task selection from Claude."""
        schema = {
            "selected_task_id": "task id or null",
            "justification": "reason",
            "confidence": 0.5,
        }
        prompt = self._render_prompt(
            "Given the ready tasks and memory, choose the next task to execute.",
            goal_id=goal_id,
            instructions=instructions,
            context={
                "ready_tasks": [node.model_dump() for node in ready_nodes],
                "memory": list(memory),
            },
            schema=schema,
        )
        payload = self._invoke_json_with_retries(
            prompt,
            required_keys=set(schema.keys()),
            operation="select_next_task",
        )
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
        """Request structured task execution output from Claude."""
        schema = {
            "status": "done|failed|blocked",
            "outputs": ["artifact-path"],
            "notes": "execution notes",
            "follow_ups": ["task-id"],
        }
        prompt = self._render_prompt(
            "Plan concrete steps to execute the specified task and describe resulting artifacts.",
            goal_id=goal_id,
            instructions=instructions,
            context={
                "task": task.model_dump(),
                "memory": list(memory),
            },
            schema=schema,
        )
        payload = self._invoke_json_with_retries(
            prompt,
            required_keys=set(schema.keys()),
            operation="execute_task",
            invoke_timeout=self.EXECUTION_TIMEOUT_SECONDS,
        )
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
        """Request a concise completion summary from Claude."""
        schema = {"summary": "Concise summary text"}
        prompt = self._render_prompt(
            "Produce a concise summary (<=3 sentences) of the completed task for logging.",
            goal_id=goal_id,
            instructions=instructions,
            context={
                "task": {"id": task.id, "title": task.title},
                "execution": execution.__dict__,
                "memory": list(memory),
            },
            schema=schema,
        )
        payload = self._invoke_json_with_retries(
            prompt,
            required_keys=set(schema.keys()),
            operation="summarize_task",
        )
        summary = str(payload.get("summary", "")).strip()
        if not summary:
            raise ValueError("Claude CLI returned empty summary")
        return summary

    def _render_prompt(
        self,
        task: str,
        *,
        goal_id: str,
        instructions: str,
        context: dict | None = None,
        schema: dict | None = None,
    ) -> str:
        """Build a prompt with instructions, context, and schema hints."""
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
                "Output MUST be one JSON object only.\n"
                "First character must be '{' and last character must be '}'.\n"
                "Output MUST use exactly these keys; no additional keys.\n"
                "Do not include commentary, markdown, code fences, or surrounding text."
            )
        else:
            sections.append(
                "Respond succinctly. Output must be plain text without commentary or code fences."
            )
        return "\n\n".join(sections)

    def _invoke(self, prompt: str, *, timeout: int = 180, operation: str = "llm_call") -> str:
        """Run CLI command and return stdout text."""
        prompt_meta = Prompt.from_text(prompt)
        cmd = [self.executable]
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

        logger.info(
            "Invoking Claude for %s Prompt(%s): chars=%d tokens~=%d tokenizer=%s timeout=%ss",
            operation,
            prompt_meta.id,
            prompt_meta.char_count,
            prompt_meta.token_estimate,
            prompt_meta.tokenizer,
            timeout,
        )
        logger.debug("Executing command: %s", " ".join(cmd))

        started = time.monotonic()
        deadline = started + timeout
        process = subprocess.Popen(
            cmd,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
        next_progress = started + self.PROGRESS_LOG_INTERVAL_SECONDS

        while process.poll() is None:
            now = time.monotonic()
            if now >= deadline:
                process.kill()
                process.communicate()
                raise RuntimeError(
                    "Claude CLI timed out after "
                    f"{timeout}s for {operation} Prompt({prompt_meta.id})"
                )
            if now >= next_progress:
                remaining = int(max(0, deadline - now))
                logger.info(
                    "Waiting for LLM response for Prompt(%s): %ds left",
                    prompt_meta.id,
                    remaining,
                )
                next_progress = now + self.PROGRESS_LOG_INTERVAL_SECONDS
            time.sleep(0.5)

        stdout, stderr = process.communicate()
        finished = time.monotonic()
        elapsed_ms = int((finished - started) * 1000)
        response_text = stdout.strip()
        response_tokens, response_tokenizer = _estimate_token_count(response_text or " ")
        logger.info(
            "Claude response for %s Prompt(%s): chars=%d tokens~=%d tokenizer=%s duration_ms=%d "
            "exit_code=%d",
            operation,
            prompt_meta.id,
            len(response_text),
            response_tokens,
            response_tokenizer,
            elapsed_ms,
            process.returncode,
        )

        logger.debug("stderr: %s", (stderr or "")[:500])

        if process.returncode != 0:
            raise RuntimeError(
                "Claude CLI failed for "
                f"{operation} Prompt({prompt_meta.id}): "
                f"{(stderr or '').strip() or response_text}"
            )
        return response_text

    def _invoke_json_with_retries(
        self,
        prompt: str,
        *,
        required_keys: set[str],
        operation: str,
        invoke_timeout: int = 180,
        max_attempts: int = 3,
    ) -> dict:
        """Invoke Claude and enforce strict JSON response shape with retries."""
        current_prompt = prompt
        last_error: ValueError | None = None
        for attempt in range(1, max_attempts + 1):
            raw = self._invoke(current_prompt, timeout=invoke_timeout, operation=operation)
            try:
                payload = self._parse_json(raw)
                self._validate_payload_keys(payload, required_keys=required_keys)
                return payload
            except ValueError as exc:
                last_error = exc
                snippet = raw[:240].replace("\n", "\\n")
                logger.warning(
                    "Invalid JSON response from Claude (attempt %d/%d): %s; len=%d; snippet=%r",
                    attempt,
                    max_attempts,
                    exc,
                    len(raw),
                    snippet,
                )
                if attempt >= max_attempts:
                    break
                current_prompt = self._build_repair_prompt(
                    original_prompt=prompt,
                    bad_output=raw,
                    required_keys=required_keys,
                )
        raise ValueError("Failed to obtain valid JSON response from Claude") from last_error

    @staticmethod
    def _build_repair_prompt(
        *,
        original_prompt: str,
        bad_output: str,
        required_keys: set[str],
    ) -> str:
        """Build corrective prompt for invalid JSON output retries."""
        ordered_keys = sorted(required_keys)
        return (
            "Your previous response violated the JSON contract.\n"
            f"Required keys (exactly): {ordered_keys}\n"
            "Return exactly one JSON object. No prose, no markdown, no code fences.\n"
            "If a value is unknown, use null or an empty string/list as appropriate.\n\n"
            "Original prompt:\n"
            f"{original_prompt}\n\n"
            "Previous invalid output:\n"
            f"{bad_output}"
        )

    @staticmethod
    def _validate_payload_keys(payload: dict, *, required_keys: set[str]) -> None:
        """Require payload to include exactly required keys."""
        actual_keys = set(payload.keys())
        missing = sorted(required_keys - actual_keys)
        extra = sorted(actual_keys - required_keys)
        if missing or extra:
            details: list[str] = []
            if missing:
                details.append(f"missing keys={missing}")
            if extra:
                details.append(f"extra keys={extra}")
            raise ValueError(f"Invalid JSON schema: {'; '.join(details)}")

    @staticmethod
    def _parse_json(text: str) -> dict:
        """Parse JSON output, with fallback for wrapped payloads."""
        text = text.strip()
        if not text:
            raise ValueError("Claude CLI returned empty output")
        try:
            payload = json.loads(text)
            if not isinstance(payload, dict):
                raise ValueError("Claude CLI returned JSON that is not an object")
            return payload
        except json.JSONDecodeError as exc:
            start = text.find("{")
            end = text.rfind("}")
            if start != -1 and end != -1 and end > start:
                candidate = text[start : end + 1]
                try:
                    payload = json.loads(candidate)
                    if not isinstance(payload, dict):
                        raise ValueError("Claude CLI returned JSON that is not an object")
                    return payload
                except json.JSONDecodeError:
                    pass
            raise ValueError(f"Failed to parse Claude CLI output as JSON: {text}") from exc


class StubLLMClient(LLMClient):
    """Heuristic client for tests."""

    def __init__(self, dag_blueprint: DagModel | None = None):
        """Initialize optional fixed DAG blueprint for tests."""
        self.dag_blueprint = dag_blueprint

    def create_goal_dag(self, instructions: str, *, goal_id: str) -> DagModel:
        """Generate a simple deterministic DAG from bullet list text."""
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
        """Pick the first ready node for deterministic test behavior."""
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
        """Return a deterministic successful execution result."""
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
        """Return a deterministic summary string for tests."""
        return (
            f"Task {task.id} completed with outputs {execution.outputs}. Notes: {execution.notes}"
        )
