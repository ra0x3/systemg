from orchestrator.heartbeat import HeartbeatParser


def test_parse_heartbeat_directives():
    """Heartbeat parser should return uppercase commands and args."""
    text = """# comment\nPAUSE\nDROP-TASK task-1\n"""
    directives = HeartbeatParser.parse(text)
    assert directives[0].command == "PAUSE"
    assert directives[1].command == "DROP-TASK"
    assert directives[1].args == ["task-1"]
