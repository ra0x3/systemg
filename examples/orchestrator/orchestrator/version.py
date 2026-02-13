"""Instruction versioning."""

from __future__ import annotations

import hashlib
import json
import logging
import time
from dataclasses import asdict, dataclass
from typing import Any

import redis


class BaseLogger:
    """Base logger mixin for consistent logging across components."""

    def __init__(self, name: str | None = None):
        """Initialize logger instance with optional explicit name."""
        self.logger = logging.getLogger(name or self.__class__.__name__)


@dataclass
class InstructionVersion:
    """Represents a versioned instruction set."""

    instructions: str
    hash: str
    timestamp: float

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> InstructionVersion:
        """Create InstructionVersion from dictionary."""
        return cls(
            instructions=data["instructions"],
            hash=data["hash"],
            timestamp=data["timestamp"],
        )

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization."""
        return asdict(self)


class InstructionStore(BaseLogger):
    """Redis-backed store for versioned instructions."""

    def __init__(self, redis_client: redis.Redis, key_prefix: str = "inst"):
        """Initialize the instruction store.

        Args:
            redis_client: Redis client instance
            key_prefix: Prefix for Redis keys (default: "inst")
        """
        super().__init__(f"{self.__class__.__name__}")
        self.redis = redis_client
        self.key_prefix = key_prefix
        self.logger.info("InstructionStore initialized with prefix: %s", key_prefix)

    def _compute_hash(self, instructions: str) -> str:
        """Compute SHA256 hash of instructions.

        Args:
            instructions: Instruction text to hash

        Returns:
            Hex digest of SHA256 hash
        """
        return hashlib.sha256(instructions.encode()).hexdigest()

    def _make_key(self, instruction_id: str) -> str:
        """Create Redis key for instruction ID.

        Args:
            instruction_id: Instruction hash/ID

        Returns:
            Full Redis key
        """
        return f"{self.key_prefix}:{instruction_id}"

    def push_version(self, instructions: str, instruction_id: str | None = None) -> str:
        """Push a new version of instructions.

        Args:
            instructions: Instruction text
            instruction_id: Optional specific ID, defaults to hash of instructions

        Returns:
            The instruction ID used
        """
        if instruction_id is None:
            instruction_id = self._compute_hash(instructions)

        version = InstructionVersion(
            instructions=instructions,
            hash=self._compute_hash(instructions),
            timestamp=time.time(),
        )

        key = self._make_key(instruction_id)
        version_json = json.dumps(version.to_dict())

        self.redis.rpush(key, version_json)
        self.logger.info(
            "Pushed new version for ID %s (hash: %s, timestamp: %.2f)",
            instruction_id,
            version.hash,
            version.timestamp,
        )

        return instruction_id

    def get_latest(self, instruction_id: str) -> InstructionVersion | None:
        """Get the latest version for an instruction ID.

        Args:
            instruction_id: Instruction ID to retrieve

        Returns:
            Latest InstructionVersion or None if not found
        """
        key = self._make_key(instruction_id)
        latest_json = self.redis.lindex(key, -1)

        if latest_json is None:
            self.logger.warning("No versions found for instruction ID: %s", instruction_id)
            return None

        data = json.loads(latest_json)
        version = InstructionVersion.from_dict(data)
        self.logger.debug(
            "Retrieved latest version for ID %s (hash: %s, timestamp: %.2f)",
            instruction_id,
            version.hash,
            version.timestamp,
        )
        return version

    def get_history(self, instruction_id: str) -> list[InstructionVersion]:
        """Get full version history for an instruction ID.

        Args:
            instruction_id: Instruction ID to retrieve history for

        Returns:
            List of all versions, oldest to newest
        """
        key = self._make_key(instruction_id)
        all_versions_json = self.redis.lrange(key, 0, -1)

        if not all_versions_json:
            self.logger.warning("No history found for instruction ID: %s", instruction_id)
            return []

        versions = [InstructionVersion.from_dict(json.loads(v)) for v in all_versions_json]
        self.logger.debug(
            "Retrieved %d versions for instruction ID: %s", len(versions), instruction_id
        )
        return versions

    def exists(self, instruction_id: str) -> bool:
        """Check if an instruction ID exists.

        Args:
            instruction_id: Instruction ID to check

        Returns:
            True if the ID exists, False otherwise
        """
        key = self._make_key(instruction_id)
        exists = bool(self.redis.exists(key))
        self.logger.debug("Instruction ID %s exists: %s", instruction_id, exists)
        return exists

    def delete(self, instruction_id: str) -> bool:
        """Delete all versions for an instruction ID.

        Args:
            instruction_id: Instruction ID to delete

        Returns:
            True if deleted, False if not found
        """
        key = self._make_key(instruction_id)
        deleted = bool(self.redis.delete(key))
        if deleted:
            self.logger.info("Deleted all versions for instruction ID: %s", instruction_id)
        else:
            self.logger.warning("No versions found to delete for ID: %s", instruction_id)
        return deleted
