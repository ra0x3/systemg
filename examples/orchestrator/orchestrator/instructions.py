"""Instruction parsing utilities."""

from __future__ import annotations

import re
from pathlib import Path

import yaml

from .models import AgentDescriptor


class InstructionParser:
    """Parses orchestrator markdown/YAML instruction files into descriptors."""

    def __init__(self, instructions_path: Path):
        """Bind parser to an instructions source path."""
        self.instructions_path = instructions_path

    def get_instructions(self) -> str:
        """Read the markdown instructions file and return its contents."""
        if not self.instructions_path.exists():
            return ""
        return self.instructions_path.read_text(encoding="utf-8")

    def parse_agents(self) -> list[AgentDescriptor]:
        """Extract agent configurations from YAML code blocks in markdown."""
        if not self.instructions_path.exists():
            return []

        raw_text = self.instructions_path.read_text(encoding="utf-8")

        yaml_pattern = r"```ya?ml\s*\n(.*?)\n```"
        matches = re.findall(yaml_pattern, raw_text, re.DOTALL)

        if not matches:
            try:
                data = yaml.safe_load(raw_text) or []
            except yaml.YAMLError as exc:
                raise ValueError(
                    f"No YAML code blocks found and content is not valid YAML: {exc}"
                ) from exc
        else:
            try:
                data = yaml.safe_load(matches[0]) or []
            except yaml.YAMLError as exc:
                raise ValueError(f"Invalid YAML in code block: {exc}") from exc

        records = data
        if isinstance(data, dict):
            records = data.get("agents", [])
        if not isinstance(records, list):
            raise ValueError("Instructions must contain a list of agents")

        base_dir = self.instructions_path.parent
        descriptors: list[AgentDescriptor] = []
        for entry in records:
            if not isinstance(entry, dict):
                continue
            try:
                descriptor = AgentDescriptor(
                    name=entry["name"],
                    role=entry.get("role"),
                    goal_id=entry.get("goal") or entry.get("goal_id") or "goal-default",
                    instructions_path=(base_dir / entry["instructions"]).resolve(),
                    heartbeat_path=(base_dir / entry["heartbeat"]).resolve(),
                    log_level=entry.get("log-level", "INFO"),
                    cadence_seconds=_parse_cadence(
                        entry.get("cadence", entry.get("cadence_seconds", "5s"))
                    ),
                )
            except KeyError as missing:
                raise ValueError(f"Missing required agent attribute: {missing}") from missing
            descriptors.append(descriptor)
        return descriptors


def _parse_cadence(value: str | int | None) -> int:
    """Normalize cadence values to integer seconds."""
    if value is None:
        return 5
    if isinstance(value, int):
        return max(1, value)
    text = str(value).strip().lower()
    if text.endswith("s"):
        text = text[:-1]
    try:
        return max(1, int(text))
    except ValueError:
        return 5
