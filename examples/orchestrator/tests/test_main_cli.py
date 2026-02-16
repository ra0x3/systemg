import main as orchestrator_main


def test_parser_defaults_for_refresh_intervals():
    """CLI should default both refresh intervals to five minutes."""
    parser = orchestrator_main._build_parser()
    args = parser.parse_args(
        [
            "--role",
            "orchestrator",
            "--instructions",
            "INSTRUCTIONS.md",
        ]
    )
    assert args.heartbeat_interval == 300.0
    assert args.instruction_interval == 300.0


def test_parser_accepts_refresh_interval_overrides():
    """CLI should parse user-provided refresh intervals."""
    parser = orchestrator_main._build_parser()
    args = parser.parse_args(
        [
            "--role",
            "orchestrator",
            "--instructions",
            "INSTRUCTIONS.md",
            "--heartbeat-interval",
            "45",
            "--instruction-interval",
            "90",
        ]
    )
    assert args.heartbeat_interval == 45.0
    assert args.instruction_interval == 90.0
