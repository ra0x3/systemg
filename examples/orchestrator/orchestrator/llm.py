"""LLM interaction clients for orchestrator and agents."""

from __future__ import annotations

import json
import logging
import os
import re
import subprocess
import time
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from datetime import datetime, timedelta
from hashlib import sha256

from .constants import PROMPT_TIMEOUT_SECONDS
from .models import DagModel, TaskNode

logger = logging.getLogger(__name__)

try:
    import tiktoken
except ImportError:
    tiktoken = None


@dataclass
class TaskSelection:
    """Structured response for selecting the next task to execute."""

    selected_task_id: str | None
    justification: str
    confidence: float = 0.5


@dataclass
class TaskExecutionResult:
    """Structured response describing task execution outcomes."""

    status: str
    outputs: list[str]
    notes: str
    follow_ups: list[str]


@dataclass
class RecoveryDecision:
    """Structured response indicating whether remediation should be created."""

    recoverable: bool
    reason: str
    remediation_title: str
    remediation_steps: list[str]
    confidence: float = 0.0


@dataclass(frozen=True)
class Prompt:
    """Metadata for prompt text used in logs and timeout errors."""

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


@dataclass(frozen=True)
class LLMRuntimeConfig:
    """CLI/runtime configuration for provider-backed LLM clients."""

    provider: str = "claude"
    executable: str = "claude"
    extra_args: tuple[str, ...] = ()
    use_sysg_spawn: bool = False

    def cli_args(self) -> list[str]:
        """Render this config to process CLI args."""
        args = [
            "--llm-provider",
            self.provider,
            "--llm-cli",
            self.executable,
        ]
        for arg in self.extra_args:
            args.extend(["--llm-extra-arg", arg])
        if self.use_sysg_spawn:
            args.append("--llm-use-sysg")
        return args


def _estimate_token_count(text: str) -> tuple[int, str]:
    """Estimate token count using tiktoken when available."""
    if tiktoken is not None:
        encoding = tiktoken.get_encoding("cl100k_base")
        return len(encoding.encode(text)), "tiktoken:cl100k_base"
    return max(1, len(text) // 4), "fallback:chars_div_4"


class LLMClient:
    """Abstract interface for LLM interactions."""

    def create_goal_dag(self, instructions: str, *, goal_id: str) -> DagModel:
        """Create a DAG model for the goal from instructions."""
        raise NotImplementedError

    def select_next_task(
        self,
        ready_nodes: Iterable[TaskNode],
        *,
        memory: Iterable[str],
        goal_id: str,
        instructions: str,
    ) -> TaskSelection:
        """Select the next task from currently ready nodes."""
        raise NotImplementedError

    def execute_task(
        self,
        task: TaskNode,
        *,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> TaskExecutionResult:
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
    ) -> str:
        """Summarize task execution for progress tracking."""
        raise NotImplementedError

    def assess_recovery(
        self,
        task: TaskNode,
        *,
        error: str,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> RecoveryDecision:
        """Assess if an execution error should produce remediation work."""
        raise NotImplementedError

    def set_spending_cap_callback(self, callback: Callable[[float], None] | None) -> None:
        """Install optional callback for provider spending-cap backoff events."""
        return None


class ClaudeCLIClient(LLMClient):
    """Interact with the Claude CLI via subprocess calls."""

    PROGRESS_LOG_INTERVAL_SECONDS = 30
    _SPENDING_CAP_PATTERN = re.compile(r"spending cap reached", re.IGNORECASE)
    _AUTO_BYPASS_ARGS = {
        "claude": "--dangerously-skip-permissions",
        "codex": "--dangerously-bypass-approvals-and-sandbox",
    }
    _RESET_TIME_PATTERN = re.compile(
        r"resets?\s+(?:at\s+)?(\d{1,2})(?::(\d{2}))?\s*([ap]m)\b",
        re.IGNORECASE,
    )

    def __init__(
        self,
        *,
        executable: str = "claude",
        extra_args: list[str] | None = None,
        use_sysg_spawn: bool = False,
        on_spending_cap: Callable[[float], None] | None = None,
    ) -> None:
        """Configure Claude CLI invocation parameters."""
        self.executable = executable
        self.extra_args = extra_args or []
        self.use_sysg_spawn = use_sysg_spawn
        self._on_spending_cap = on_spending_cap

    def set_spending_cap_callback(self, callback: Callable[[float], None] | None) -> None:
        """Install callback invoked before sleeping on spending-cap errors."""
        self._on_spending_cap = callback

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
            invoke_timeout=PROMPT_TIMEOUT_SECONDS,
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

    def assess_recovery(
        self,
        task: TaskNode,
        *,
        error: str,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> RecoveryDecision:
        """Ask Claude whether an error is recoverable and how to remediate."""
        schema = {
            "recoverable": True,
            "reason": "short reason",
            "remediation_title": "short remediation task title",
            "remediation_steps": ["step 1", "step 2"],
            "confidence": 0.0,
        }
        prompt = self._render_prompt(
            "Classify the task error as recoverable or terminal. "
            "Prefer recoverable for environment/setup issues. "
            "If recoverable, propose concise remediation steps.",
            goal_id=goal_id,
            instructions=instructions,
            context={
                "task": task.model_dump(),
                "error": error[:1200],
                "memory": list(memory),
            },
            schema=schema,
        )
        payload = self._invoke_json_with_retries(
            prompt,
            required_keys=set(schema.keys()),
            operation="assess_recovery",
        )
        return RecoveryDecision(
            recoverable=bool(payload.get("recoverable")),
            reason=str(payload.get("reason", "")).strip(),
            remediation_title=str(payload.get("remediation_title", "")).strip(),
            remediation_steps=[str(step) for step in payload.get("remediation_steps", [])],
            confidence=float(payload.get("confidence", 0.0)),
        )

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

    def _invoke(
        self,
        prompt: str,
        *,
        timeout: int = PROMPT_TIMEOUT_SECONDS,
        operation: str = "llm_call",
    ) -> str:
        """Run CLI command and return stdout text."""
        prompt_meta = Prompt.from_text(prompt)
        cmd = [self.executable]
        executable_name = os.path.basename(self.executable)
        bypass_arg = self._AUTO_BYPASS_ARGS.get(executable_name)
        if bypass_arg:
            cmd.append(bypass_arg)
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

        provider_name = self.executable
        logger.info(
            "Invoking %s for %s Prompt(%s): chars=%d tokens~=%d tokenizer=%s timeout=%ss",
            provider_name,
            operation,
            prompt_meta.id,
            prompt_meta.char_count,
            prompt_meta.token_estimate,
            prompt_meta.tokenizer,
            timeout,
        )
        logger.debug("Executing command: %s", " ".join(cmd))

        while True:
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
            stderr_text = (stderr or "").strip()
            logger.info(
                "%s response for %s Prompt(%s): chars=%d tokens~=%d tokenizer=%s duration_ms=%d "
                "exit_code=%d",
                provider_name,
                operation,
                prompt_meta.id,
                len(response_text),
                response_tokens,
                response_tokenizer,
                elapsed_ms,
                process.returncode,
            )

            logger.debug("stderr: %s", (stderr_text or "")[:500])

            error_text = stderr_text or response_text
            combined_error_text = "\n".join(
                part for part in (stderr_text, response_text) if part
            ).strip()
            if process.returncode == 0:
                return response_text
            if self._is_spending_cap_error(combined_error_text or error_text):
                wait_seconds = self._seconds_until_local_reset_from_message(
                    combined_error_text or error_text
                )
                if wait_seconds is None:
                    logger.error(
                        "Spending cap reached but reset time could not be parsed: %s",
                        combined_error_text or error_text,
                    )
                else:
                    reset_at = datetime.now().astimezone() + timedelta(seconds=wait_seconds)
                    logger.warning(
                        "%s spending cap reached for %s Prompt(%s): %s. "
                        "Sleeping %.0fs until local reset at %s before retry.",
                        provider_name,
                        operation,
                        prompt_meta.id,
                        combined_error_text or error_text,
                        wait_seconds,
                        reset_at.isoformat(timespec="seconds"),
                    )
                    if self._on_spending_cap is not None:
                        self._on_spending_cap(wait_seconds)
                    time.sleep(wait_seconds)
                    continue
            raise RuntimeError(
                f"{provider_name} CLI failed for {operation} Prompt({prompt_meta.id}): {error_text}"
            )

    @classmethod
    def _is_spending_cap_error(cls, text: str) -> bool:
        """Return whether stderr/stdout indicates Claude spending cap exhaustion."""
        return bool(cls._SPENDING_CAP_PATTERN.search(text))

    @classmethod
    def _seconds_until_local_reset_from_message(
        cls, text: str, *, now: datetime | None = None
    ) -> float | None:
        """Parse a reset time from Claude message and return sleep seconds."""
        match = cls._RESET_TIME_PATTERN.search(text)
        if not match:
            return None
        hour_text, minute_text, period = match.groups()
        hour = int(hour_text)
        minute = int(minute_text) if minute_text is not None else 0
        if hour < 1 or hour > 12 or minute < 0 or minute > 59:
            return None

        period_normalized = period.lower()
        hour_24 = hour % 12
        if period_normalized == "pm":
            hour_24 += 12

        local_now = (now or datetime.now().astimezone()).astimezone()
        reset_at = local_now.replace(hour=hour_24, minute=minute, second=0, microsecond=0)
        if reset_at <= local_now:
            reset_at += timedelta(days=1)
        return (reset_at - local_now).total_seconds()

    def _invoke_json_with_retries(
        self,
        prompt: str,
        *,
        required_keys: set[str],
        operation: str,
        invoke_timeout: int = PROMPT_TIMEOUT_SECONDS,
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

    def assess_recovery(
        self,
        task: TaskNode,
        *,
        error: str,
        goal_id: str,
        instructions: str,
        memory: Iterable[str],
    ) -> RecoveryDecision:
        """Return a deterministic non-recoverable decision for generic tests."""
        return RecoveryDecision(
            recoverable=False,
            reason=f"No recovery policy for: {error[:120]}",
            remediation_title="",
            remediation_steps=[],
            confidence=0.0,
        )


class CodexCLIClient(ClaudeCLIClient):
    """Interact with the Codex CLI via subprocess calls."""

    def __init__(
        self,
        *,
        executable: str = "codex",
        extra_args: list[str] | None = None,
        use_sysg_spawn: bool = False,
        on_spending_cap: Callable[[float], None] | None = None,
    ) -> None:
        """Configure Codex CLI invocation parameters."""
        super().__init__(
            executable=executable,
            extra_args=extra_args,
            use_sysg_spawn=use_sysg_spawn,
            on_spending_cap=on_spending_cap,
        )


def create_llm_client(
    config: LLMRuntimeConfig,
    *,
    on_spending_cap: Callable[[float], None] | None = None,
) -> LLMClient:
    """Construct a provider-specific LLM client from runtime config."""
    provider = config.provider.strip().lower()
    if provider == "claude":
        return ClaudeCLIClient(
            executable=config.executable,
            extra_args=list(config.extra_args),
            use_sysg_spawn=config.use_sysg_spawn,
            on_spending_cap=on_spending_cap,
        )
    if provider == "codex":
        return CodexCLIClient(
            executable=config.executable,
            extra_args=list(config.extra_args),
            use_sysg_spawn=config.use_sysg_spawn,
            on_spending_cap=on_spending_cap,
        )
    raise ValueError(f"Unsupported LLM provider: {config.provider}")
