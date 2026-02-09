"""Agent memory abstraction."""

from __future__ import annotations

from collections import deque
from collections.abc import Iterable, Iterator


class Memory:
    """Bounded deque that can be snapshotted for Redis persistence."""

    def __init__(self, max_entries: int = 50):
        self._entries: deque[str] = deque(maxlen=max_entries)

    def append(self, entry: str) -> None:
        self._entries.append(entry)

    def extend(self, entries: Iterable[str]) -> None:
        for entry in entries:
            self.append(entry)

    def snapshot(self) -> list[str]:
        return list(self._entries)

    def hydrate(self, entries: Iterable[str]) -> None:
        self._entries.clear()
        self.extend(entries)

    def __iter__(self) -> Iterator[str]:
        return iter(self._entries)

    def __len__(self) -> int:
        return len(self._entries)
