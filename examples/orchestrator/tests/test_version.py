"""Tests for instruction versioning system."""

from __future__ import annotations

import time

import fakeredis
import pytest

from orchestrator.version import InstructionStore, InstructionVersion


class TestInstructionVersion:
    """Test InstructionVersion dataclass."""

    def test_from_dict(self):
        """Test creating InstructionVersion from dictionary."""
        data = {
            "instructions": "test instructions",
            "hash": "abc123",
            "timestamp": 1234567890.0,
        }
        version = InstructionVersion.from_dict(data)
        assert version.instructions == "test instructions"
        assert version.hash == "abc123"
        assert version.timestamp == 1234567890.0

    def test_to_dict(self):
        """Test converting InstructionVersion to dictionary."""
        version = InstructionVersion(
            instructions="test instructions",
            hash="abc123",
            timestamp=1234567890.0,
        )
        data = version.to_dict()
        assert data["instructions"] == "test instructions"
        assert data["hash"] == "abc123"
        assert data["timestamp"] == 1234567890.0


class TestInstructionStore:
    """Test InstructionStore functionality."""

    @pytest.fixture
    def redis_client(self):
        """Create a fakeredis client for testing."""
        return fakeredis.FakeRedis(decode_responses=True)

    @pytest.fixture
    def store(self, redis_client):
        """Create an InstructionStore with fakeredis."""
        return InstructionStore(redis_client)

    def test_compute_hash(self, store):
        """Test SHA256 hash computation."""
        instructions = "test instructions"
        hash1 = store._compute_hash(instructions)
        hash2 = store._compute_hash(instructions)
        hash3 = store._compute_hash("different instructions")

        assert hash1 == hash2  # Same input produces same hash
        assert hash1 != hash3  # Different input produces different hash
        assert len(hash1) == 64  # SHA256 produces 64 hex characters

    def test_push_version(self, store):
        """Test pushing a new version."""
        instructions = "test instructions"
        instruction_id = store.push_version(instructions)

        assert instruction_id is not None
        assert store.exists(instruction_id)

    def test_push_version_with_custom_id(self, store):
        """Test pushing a version with custom ID."""
        instructions = "test instructions"
        custom_id = "custom:id"
        returned_id = store.push_version(instructions, custom_id)

        assert returned_id == custom_id
        assert store.exists(custom_id)

    def test_get_latest(self, store):
        """Test retrieving the latest version."""
        instructions1 = "version 1"
        instructions2 = "version 2"
        instruction_id = "test:id"

        # Push two versions
        store.push_version(instructions1, instruction_id)
        time.sleep(0.01)  # Ensure different timestamps
        store.push_version(instructions2, instruction_id)

        latest = store.get_latest(instruction_id)
        assert latest is not None
        assert latest.instructions == instructions2

    def test_get_latest_nonexistent(self, store):
        """Test retrieving latest for non-existent ID."""
        latest = store.get_latest("nonexistent:id")
        assert latest is None

    def test_get_history(self, store):
        """Test retrieving full version history."""
        instruction_id = "test:id"
        instructions = ["version 1", "version 2", "version 3"]

        # Push multiple versions
        for inst in instructions:
            store.push_version(inst, instruction_id)
            time.sleep(0.01)  # Ensure different timestamps

        history = store.get_history(instruction_id)
        assert len(history) == 3
        assert history[0].instructions == "version 1"
        assert history[1].instructions == "version 2"
        assert history[2].instructions == "version 3"

        # Verify timestamps are in order
        assert history[0].timestamp < history[1].timestamp
        assert history[1].timestamp < history[2].timestamp

    def test_get_history_nonexistent(self, store):
        """Test retrieving history for non-existent ID."""
        history = store.get_history("nonexistent:id")
        assert history == []

    def test_exists(self, store):
        """Test checking if instruction ID exists."""
        instruction_id = "test:id"
        assert not store.exists(instruction_id)

        store.push_version("test", instruction_id)
        assert store.exists(instruction_id)

    def test_delete(self, store):
        """Test deleting all versions for an ID."""
        instruction_id = "test:id"
        store.push_version("version 1", instruction_id)
        store.push_version("version 2", instruction_id)

        assert store.exists(instruction_id)
        deleted = store.delete(instruction_id)
        assert deleted is True
        assert not store.exists(instruction_id)

    def test_delete_nonexistent(self, store):
        """Test deleting non-existent ID."""
        deleted = store.delete("nonexistent:id")
        assert deleted is False

    def test_multiple_instruction_ids(self, store):
        """Test managing multiple instruction IDs independently."""
        id1 = "agent1:goal1"
        id2 = "agent2:goal2"

        store.push_version("instructions for agent 1", id1)
        store.push_version("instructions for agent 2", id2)

        version1 = store.get_latest(id1)
        version2 = store.get_latest(id2)

        assert version1.instructions == "instructions for agent 1"
        assert version2.instructions == "instructions for agent 2"

    def test_version_immutability(self, store):
        """Test that versions are immutable once pushed."""
        instruction_id = "test:id"
        original_instructions = "original instructions"

        store.push_version(original_instructions, instruction_id)
        store.get_latest(instruction_id)  # Ensure it's stored

        store.push_version("updated instructions", instruction_id)

        # Get history and verify first version unchanged
        history = store.get_history(instruction_id)
        assert history[0].instructions == original_instructions
        assert history[1].instructions == "updated instructions"
