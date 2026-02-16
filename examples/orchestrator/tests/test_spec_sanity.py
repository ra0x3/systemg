from pathlib import Path

EXAMPLE_ROOT = Path(__file__).resolve().parents[1]
SPEC_PATH = EXAMPLE_ROOT / "docs" / "ORCHESTRATOR_SPEC.md"
ASSETS_DIR = EXAMPLE_ROOT / "tests" / "assets"


def test_spec_includes_required_sections():
    """Spec document should include required section headings."""
    spec_text = SPEC_PATH.read_text(encoding="utf-8")
    expected_headings = [
        "## Naming Conventions",
        "## Agent Execution Model",
        "## Spawn Taxonomy",
        "## Heartbeat File Format",
        "## Implementation Phases",
        "## Schema Appendix",
    ]
    for heading in expected_headings:
        assert heading in spec_text, f"Missing heading: {heading}"


def test_asset_files_exist_and_match_format():
    """Fixture asset files should exist and follow basic format rules."""
    instructions_path = ASSETS_DIR / "INSTRUCTIONS.md"
    heartbeat_path = ASSETS_DIR / "instructions" / "heartbeat" / "agent-research.md"

    assert instructions_path.exists(), "Fixture instructions file missing"
    assert heartbeat_path.exists(), "Fixture heartbeat file missing"

    heartbeat_lines = [
        line.strip()
        for line in heartbeat_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    assert heartbeat_lines[0].startswith("#"), "Heartbeat fixture should begin with a comment"
    for directive in heartbeat_lines[1:]:
        parts = directive.split()
        assert parts[0].isupper(), f"Heartbeat directive should be uppercase: {directive}"
