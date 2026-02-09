"""Heartbeat file parsing and directive handling."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class HeartbeatDirective:
    command: str
    args: list[str]

    def __str__(self) -> str:  # pragma: no cover - debugging helper
        arg_str = " ".join(self.args)
        return f"{self.command} {arg_str}".strip()


class HeartbeatParser:
    """Parse line-oriented heartbeat directives."""

    @staticmethod
    def parse(text: str) -> list[HeartbeatDirective]:
        directives: list[HeartbeatDirective] = []
        for raw_line in text.splitlines():
            line = raw_line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            command = parts[0].upper()
            directives.append(HeartbeatDirective(command=command, args=parts[1:]))
        return directives

    @staticmethod
    def read(path: Path) -> list[HeartbeatDirective]:
        if not path.exists():
            return []
        return HeartbeatParser.parse(path.read_text(encoding="utf-8"))

    @staticmethod
    def truncate(path: Path) -> None:
        path.write_text("# processed\n", encoding="utf-8")


class HeartbeatController:
    """Utility used by agents to react to directives."""

    def __init__(self, heartbeat_path: Path):
        self.heartbeat_path = heartbeat_path

    def consume(self) -> list[HeartbeatDirective]:
        directives = HeartbeatParser.read(self.heartbeat_path)
        if directives:
            HeartbeatParser.truncate(self.heartbeat_path)
        return directives
