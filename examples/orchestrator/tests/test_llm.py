from __future__ import annotations

import orchestrator.llm as llm_module
from orchestrator.llm import ClaudeCLIClient, Prompt
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


def test_execute_task_uses_300_second_timeout(monkeypatch):
    """Execution prompt should use the longer timeout budget."""
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
    assert observed[0] == (300, "execute_task")
