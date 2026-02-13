"""Agent memory abstraction."""

from __future__ import annotations

from collections import deque
from collections.abc import Iterable, Iterator


class Memory:
    """Bounded deque that can be snapshotted for Redis persistence."""

    def __init__(self, max_entries: int = 50):
        """Initialize bounded memory storage."""
        self._entries: deque[str] = deque(maxlen=max_entries)

    def append(self, entry: str) -> None:
        """Append one memory entry."""
        self._entries.append(entry)

    def extend(self, entries: Iterable[str]) -> None:
        """Append multiple memory entries."""
        for entry in entries:
            self.append(entry)

    def snapshot(self) -> list[str]:
        """Return a list snapshot of current entries."""
        return list(self._entries)

    def hydrate(self, entries: Iterable[str]) -> None:
        """Replace memory with provided entries."""
        self._entries.clear()
        self.extend(entries)

    def __iter__(self) -> Iterator[str]:
        """Iterate through memory entries."""
        return iter(self._entries)

    def __len__(self) -> int:
        """Return number of memory entries."""
        return len(self._entries)
