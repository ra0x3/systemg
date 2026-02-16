from __future__ import annotations

from datetime import datetime

import orchestrator.llm as llm_module
from orchestrator.constants import PROMPT_TIMEOUT_SECONDS
from orchestrator.llm import (
    ClaudeCLIClient,
    CodexCLIClient,
    LLMRuntimeConfig,
    Prompt,
    create_llm_client,
)
from orchestrator.models import TaskNode


def test_execute_task_retries_after_empty_output(monkeypatch):
    """Client should retry when Claude emits empty output first."""
    client = ClaudeCLIClient(executable="claude")
    responses = iter(
        [
            "",
            '{"status":"done","outputs":["artifact://x"],"notes":"ok","follow_ups":[]}',
        ]
    )

    def fake_invoke(prompt: str, *, timeout: int = 180, operation: str = "llm_call") -> str:
        return next(responses)

    monkeypatch.setattr(client, "_invoke", fake_invoke)

    result = client.execute_task(
        TaskNode(id="task-1", title="T", priority=1),
        goal_id="goal-1",
        instructions="test",
        memory=[],
    )
    assert result.status == "done"
    assert result.outputs == ["artifact://x"]


def test_execute_task_retries_after_missing_required_keys(monkeypatch):
    """Client should retry when JSON shape is missing required keys."""
    client = ClaudeCLIClient(executable="claude")
    responses = iter(
        [
            '{"status":"done","outputs":["artifact://x"],"notes":"ok"}',
            '{"status":"done","outputs":["artifact://x"],"notes":"ok","follow_ups":[]}',
        ]
    )

    def fake_invoke(prompt: str, *, timeout: int = 180, operation: str = "llm_call") -> str:
        return next(responses)

    monkeypatch.setattr(client, "_invoke", fake_invoke)

    result = client.execute_task(
        TaskNode(id="task-2", title="T2", priority=2),
        goal_id="goal-2",
        instructions="test",
        memory=[],
    )
    assert result.status == "done"
    assert result.follow_ups == []


def test_execute_task_raises_after_retry_exhaustion(monkeypatch):
    """Client should fail after exhausting retries for invalid output."""
    client = ClaudeCLIClient(executable="claude")
    responses = iter(["", "", ""])

    def fake_invoke(prompt: str, *, timeout: int = 180, operation: str = "llm_call") -> str:
        return next(responses)

    monkeypatch.setattr(client, "_invoke", fake_invoke)

    try:
        client.execute_task(
            TaskNode(id="task-3", title="T3", priority=3),
            goal_id="goal-3",
            instructions="test",
            memory=[],
        )
    except ValueError as exc:
        assert "Failed to obtain valid JSON response from Claude" in str(exc)
    else:
        raise AssertionError("Expected ValueError for repeated invalid JSON output")


def test_prompt_hash_stable():
    """Prompt IDs should be deterministic for identical text."""
    first = Prompt.from_text("same input")
    second = Prompt.from_text("same input")
    assert first.id == second.id
    assert first.token_estimate == second.token_estimate


def test_prompt_falls_back_without_tiktoken(monkeypatch):
    """Token estimation should still work without tiktoken."""
    monkeypatch.setattr(llm_module, "tiktoken", None)
    prompt = Prompt.from_text("abcd efgh")
    assert prompt.token_estimate >= 1
    assert prompt.tokenizer.startswith("fallback:")


def test_execute_task_uses_900_second_timeout(monkeypatch):
    """Execution prompt should use the 15-minute timeout budget."""
    client = ClaudeCLIClient(executable="claude")
    observed: list[tuple[int, str]] = []

    def fake_invoke(prompt: str, *, timeout: int = 180, operation: str = "llm_call") -> str:
        observed.append((timeout, operation))
        return '{"status":"done","outputs":[],"notes":"ok","follow_ups":[]}'

    monkeypatch.setattr(client, "_invoke", fake_invoke)

    client.execute_task(
        TaskNode(id="task-timeout", title="T", priority=1),
        goal_id="goal-timeout",
        instructions="test",
        memory=[],
    )
    assert observed
    assert observed[0] == (900, "execute_task")


def test_create_goal_dag_uses_prompt_timeout_constant(monkeypatch):
    """DAG prompt should use the shared prompt timeout constant."""
    client = ClaudeCLIClient(executable="claude")
    observed: list[tuple[int, str]] = []

    def fake_invoke(prompt: str, *, timeout: int = 180, operation: str = "llm_call") -> str:
        observed.append((timeout, operation))
        return '{"goal_id":"goal-const","nodes":[],"edges":[]}'

    monkeypatch.setattr(client, "_invoke", fake_invoke)

    client.create_goal_dag("test", goal_id="goal-const")
    assert observed
    assert observed[0] == (PROMPT_TIMEOUT_SECONDS, "create_goal_dag")


def test_summarize_task_uses_prompt_timeout_constant(monkeypatch):
    """Summary prompt should use the shared prompt timeout constant."""
    client = ClaudeCLIClient(executable="claude")
    observed: list[tuple[int, str]] = []

    def fake_invoke(prompt: str, *, timeout: int = 180, operation: str = "llm_call") -> str:
        observed.append((timeout, operation))
        return '{"summary":"done"}'

    monkeypatch.setattr(client, "_invoke", fake_invoke)

    client.summarize_task(
        TaskNode(id="task-summary", title="T", priority=1),
        llm_module.TaskExecutionResult(status="done", outputs=[], notes="ok", follow_ups=[]),
        goal_id="goal-summary",
        instructions="test",
        memory=[],
    )
    assert observed
    assert observed[0] == (PROMPT_TIMEOUT_SECONDS, "summarize_task")


def test_seconds_until_local_reset_parses_8pm():
    """Reset parser should return seconds until the next local 8pm."""
    now = datetime.fromisoformat("2026-02-15T17:28:01-08:00")
    seconds = ClaudeCLIClient._seconds_until_local_reset_from_message(
        "Spending cap reached resets 8pm",
        now=now,
    )
    assert seconds is not None
    assert int(seconds) == (2 * 3600 + 31 * 60 + 59)


def test_seconds_until_local_reset_rolls_to_tomorrow():
    """Reset parser should roll to next day when reset time already passed."""
    now = datetime.fromisoformat("2026-02-15T21:00:00-08:00")
    seconds = ClaudeCLIClient._seconds_until_local_reset_from_message(
        "Spending cap reached resets 8pm",
        now=now,
    )
    assert seconds is not None
    assert int(seconds) == 23 * 3600


def test_invoke_sleeps_and_retries_on_spending_cap(monkeypatch):
    """Client should sleep until reset and retry when spending cap is reached."""
    client = ClaudeCLIClient(executable="claude")
    sleeps: list[float] = []

    class FakeProcess:
        def __init__(self, *, returncode: int, stdout: str, stderr: str):
            self.returncode = returncode
            self._stdout = stdout
            self._stderr = stderr

        def poll(self):
            return self.returncode

        def communicate(self):
            return self._stdout, self._stderr

        def kill(self):
            return None

    processes = iter(
        [
            FakeProcess(returncode=1, stdout="", stderr="Spending cap reached resets 8pm"),
            FakeProcess(
                returncode=0,
                stdout='{"status":"done","outputs":[],"notes":"ok","follow_ups":[]}',
                stderr="",
            ),
        ]
    )

    def fake_popen(*args, **kwargs):
        return next(processes)

    def fake_sleep(seconds: float):
        sleeps.append(seconds)

    monkeypatch.setattr(llm_module.subprocess, "Popen", fake_popen)
    monkeypatch.setattr(llm_module.time, "sleep", fake_sleep)
    monkeypatch.setattr(
        client,
        "_seconds_until_local_reset_from_message",
        lambda text: 12.0,  # type: ignore[method-assign]
    )

    result = client._invoke("{}", operation="execute_task")
    assert '"status":"done"' in result
    assert sleeps == [12.0]


def test_claude_code_spending_cap_regex():
    """Spending-cap detector should match Claude Code reset phrasing."""
    assert ClaudeCLIClient._is_spending_cap_error("Spending cap reached resets 8pm")


def test_invoke_sleeps_when_spending_cap_only_in_stdout(monkeypatch):
    """Spending-cap detection should consider stdout even when stderr is non-empty."""
    client = ClaudeCLIClient(executable="claude")
    sleeps: list[float] = []

    class FakeProcess:
        def __init__(self, *, returncode: int, stdout: str, stderr: str):
            self.returncode = returncode
            self._stdout = stdout
            self._stderr = stderr

        def poll(self):
            return self.returncode

        def communicate(self):
            return self._stdout, self._stderr

        def kill(self):
            return None

    processes = iter(
        [
            FakeProcess(
                returncode=1,
                stdout="Spending cap reached resets 8pm",
                stderr="wrapper stderr",
            ),
            FakeProcess(
                returncode=0,
                stdout='{"status":"done","outputs":[],"notes":"ok","follow_ups":[]}',
                stderr="",
            ),
        ]
    )

    monkeypatch.setattr(llm_module.subprocess, "Popen", lambda *args, **kwargs: next(processes))
    monkeypatch.setattr(llm_module.time, "sleep", lambda seconds: sleeps.append(seconds))
    monkeypatch.setattr(
        client,
        "_seconds_until_local_reset_from_message",
        lambda text: 7.0,  # type: ignore[method-assign]
    )

    result = client._invoke("{}", operation="execute_task")
    assert '"status":"done"' in result
    assert sleeps == [7.0]


def test_create_llm_client_supports_codex_provider():
    """Factory should build a Codex CLI client from config."""
    config = LLMRuntimeConfig(provider="codex", executable="codex", extra_args=("--x",))
    client = create_llm_client(config)
    assert isinstance(client, CodexCLIClient)


def test_invoke_adds_provider_specific_bypass_args(monkeypatch):
    """Claude and Codex should each receive their own supported bypass arg."""
    observed_cmds: list[list[str]] = []

    class FakeProcess:
        returncode = 0

        def poll(self):
            return self.returncode

        def communicate(self):
            return ('{"status":"done","outputs":[],"notes":"ok","follow_ups":[]}', "")

        def kill(self):
            return None

    def fake_popen(cmd, **kwargs):
        observed_cmds.append(cmd)
        return FakeProcess()

    monkeypatch.setattr(llm_module.subprocess, "Popen", fake_popen)

    claude = ClaudeCLIClient(executable="claude")
    codex = CodexCLIClient(executable="codex")

    claude._invoke("{}", operation="execute_task")
    codex._invoke("{}", operation="execute_task")

    assert observed_cmds[0][1] == "--dangerously-skip-permissions"
    assert observed_cmds[1][1] == "--dangerously-bypass-approvals-and-sandbox"
