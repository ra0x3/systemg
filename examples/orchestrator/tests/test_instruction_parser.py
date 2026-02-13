"""Tests for instruction parsing with markdown support."""

from __future__ import annotations

from pathlib import Path

import pytest

from orchestrator.instructions import InstructionParser


@pytest.fixture
def markdown_instructions(tmp_path: Path) -> Path:
    """Create a markdown instructions file with YAML code blocks."""
    instructions_path = tmp_path / "INSTRUCTIONS.md"
    instructions_path.write_text("""# Orchestrator Instructions

## Agent Configuration

This is some markdown text explaining the agents.

```yaml
agents:
  - name: test-agent-1
    goal: test-goal
    heartbeat: heartbeat/TEST1.md
    instructions: instructions/TEST1.md
    log-level: DEBUG
    cadence: 10s

  - name: test-agent-2
    goal: test-goal-2
    heartbeat: heartbeat/TEST2.md
    instructions: instructions/TEST2.md
    log-level: INFO
    cadence: 5s
```

Additional markdown content here.
""")
    # Create the referenced directories
    (tmp_path / "heartbeat").mkdir()
    (tmp_path / "instructions").mkdir()
    # Create the referenced files
    (tmp_path / "heartbeat" / "TEST1.md").write_text("heartbeat 1")
    (tmp_path / "heartbeat" / "TEST2.md").write_text("heartbeat 2")
    (tmp_path / "instructions" / "TEST1.md").write_text("instructions 1")
    (tmp_path / "instructions" / "TEST2.md").write_text("instructions 2")
    return instructions_path


@pytest.fixture
def pure_yaml_instructions(tmp_path: Path) -> Path:
    """Create a pure YAML instructions file for backward compatibility."""
    instructions_path = tmp_path / "INSTRUCTIONS.yaml"
    instructions_path.write_text("""agents:
  - name: yaml-agent
    goal: yaml-goal
    heartbeat: heartbeat/YAML.md
    instructions: instructions/YAML.md
    log-level: WARN
    cadence: 15s
""")
    # Create the referenced directories
    (tmp_path / "heartbeat").mkdir()
    (tmp_path / "instructions").mkdir()
    # Create the referenced files
    (tmp_path / "heartbeat" / "YAML.md").write_text("yaml heartbeat")
    (tmp_path / "instructions" / "YAML.md").write_text("yaml instructions")
    return instructions_path


class TestInstructionParser:
    """Test the InstructionParser with markdown support."""

    def test_get_instructions_returns_full_markdown(self, markdown_instructions: Path):
        """Test that get_instructions returns the full markdown content."""
        parser = InstructionParser(markdown_instructions)
        content = parser.get_instructions()

        assert "# Orchestrator Instructions" in content
        assert "## Agent Configuration" in content
        assert "```yaml" in content
        assert "test-agent-1" in content
        assert "Additional markdown content here." in content

    def test_parse_agents_from_markdown_code_blocks(self, markdown_instructions: Path):
        """Test parsing agent configurations from YAML code blocks in markdown."""
        parser = InstructionParser(markdown_instructions)
        agents = parser.parse_agents()

        assert len(agents) == 2

        # Check first agent
        agent1 = agents[0]
        assert agent1.name == "test-agent-1"
        assert agent1.goal_id == "test-goal"
        assert agent1.log_level == "DEBUG"
        assert agent1.cadence_seconds == 10
        assert agent1.heartbeat_path.name == "TEST1.md"
        assert agent1.instructions_path.name == "TEST1.md"

        # Check second agent
        agent2 = agents[1]
        assert agent2.name == "test-agent-2"
        assert agent2.goal_id == "test-goal-2"
        assert agent2.log_level == "INFO"
        assert agent2.cadence_seconds == 5

    def test_parse_agents_from_pure_yaml_fallback(self, pure_yaml_instructions: Path):
        """Test backward compatibility with pure YAML files."""
        parser = InstructionParser(pure_yaml_instructions)
        agents = parser.parse_agents()

        assert len(agents) == 1
        agent = agents[0]
        assert agent.name == "yaml-agent"
        assert agent.goal_id == "yaml-goal"
        assert agent.log_level == "WARN"
        assert agent.cadence_seconds == 15

    def test_parse_agents_with_missing_file(self, tmp_path: Path):
        """Test parsing when instructions file doesn't exist."""
        parser = InstructionParser(tmp_path / "nonexistent.md")

        assert parser.get_instructions() == ""
        assert parser.parse_agents() == []

    def test_parse_agents_with_invalid_yaml_in_markdown(self, tmp_path: Path):
        """Test error handling for invalid YAML in code blocks."""
        instructions_path = tmp_path / "INVALID.md"
        instructions_path.write_text("""# Invalid Instructions

```yaml
agents:
  - name: invalid
    : this is invalid
    [unclosed bracket
```
""")

        parser = InstructionParser(instructions_path)
        with pytest.raises(ValueError, match="Invalid YAML in code block"):
            parser.parse_agents()

    def test_parse_agents_with_multiple_yaml_blocks(self, tmp_path: Path):
        """Test that only the first YAML block is parsed for agent config."""
        instructions_path = tmp_path / "MULTI.md"
        instructions_path.write_text("""# Multiple YAML blocks

```yaml
agents:
  - name: first-block-agent
    goal: first-goal
    heartbeat: heartbeat/FIRST.md
    instructions: instructions/FIRST.md
```

Some other content

```yaml
agents:
  - name: second-block-agent
    goal: second-goal
    heartbeat: heartbeat/SECOND.md
    instructions: instructions/SECOND.md
```
""")
        # Create required files
        (tmp_path / "heartbeat").mkdir()
        (tmp_path / "instructions").mkdir()
        (tmp_path / "heartbeat" / "FIRST.md").write_text("first")
        (tmp_path / "instructions" / "FIRST.md").write_text("first")

        parser = InstructionParser(instructions_path)
        agents = parser.parse_agents()

        # Should only parse the first block
        assert len(agents) == 1
        assert agents[0].name == "first-block-agent"

    def test_parse_agents_with_missing_required_fields(self, tmp_path: Path):
        """Test error handling for missing required agent fields."""
        instructions_path = tmp_path / "INCOMPLETE.md"
        instructions_path.write_text("""# Incomplete Instructions

```yaml
agents:
  - name: incomplete-agent
    # Missing required fields: heartbeat, instructions
```
""")

        parser = InstructionParser(instructions_path)
        with pytest.raises(ValueError, match="Missing required agent attribute"):
            parser.parse_agents()

    def test_cadence_parsing_variations(self, tmp_path: Path):
        """Test various cadence formats are parsed correctly."""
        instructions_path = tmp_path / "CADENCE.md"
        instructions_path.write_text("""# Cadence Test

```yaml
agents:
  - name: agent-1
    heartbeat: h1.md
    instructions: i1.md
    cadence: 30s

  - name: agent-2
    heartbeat: h2.md
    instructions: i2.md
    cadence: 15

  - name: agent-3
    heartbeat: h3.md
    instructions: i3.md
    # No cadence, should default to 5
```
""")
        # Create required files
        for f in ["h1.md", "h2.md", "h3.md", "i1.md", "i2.md", "i3.md"]:
            (tmp_path / f).write_text("content")

        parser = InstructionParser(instructions_path)
        agents = parser.parse_agents()

        assert agents[0].cadence_seconds == 30
        assert agents[1].cadence_seconds == 15
        assert agents[2].cadence_seconds == 5
