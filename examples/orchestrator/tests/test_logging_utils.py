import logging
from io import StringIO

from orchestrator.logging_utils import CompactingHandler


class _CaptureHandler(logging.Handler):
    def __init__(self):
        super().__init__(level=logging.INFO)
        self.messages: list[str] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.messages.append(record.getMessage())


def _build_logger() -> tuple[logging.Logger, _CaptureHandler, CompactingHandler]:
    capture = _CaptureHandler()
    handler = CompactingHandler(capture)
    logger = logging.getLogger("test.compacting")
    logger.handlers.clear()
    logger.setLevel(logging.INFO)
    logger.propagate = False
    logger.addHandler(handler)
    return logger, capture, handler


def test_compacts_consecutive_duplicates() -> None:
    logger, capture, handler = _build_logger()
    logger.info("same line")
    logger.info("same line")
    logger.info("same line")
    logger.info("next line")
    handler.flush()
    assert capture.messages == ["+3 same line", "next line"]


def test_passes_distinct_lines_without_compaction() -> None:
    logger, capture, handler = _build_logger()
    logger.info("a")
    logger.info("b")
    logger.info("a")
    handler.flush()
    assert capture.messages == ["a", "b", "a"]


def test_uses_rendered_message_text_for_matching() -> None:
    logger, capture, handler = _build_logger()
    logger.info("value=%s", "x")
    logger.info("value=%s", "x")
    handler.flush()
    assert capture.messages == ["+2 value=x"]


def test_flush_emits_pending_record() -> None:
    logger, capture, handler = _build_logger()
    logger.info("pending")
    handler.flush()
    assert capture.messages == ["pending"]


def test_warning_emits_immediately_without_flush() -> None:
    logger, capture, handler = _build_logger()
    logger.info("pending")
    logger.warning("spending cap reached")
    assert capture.messages == ["pending", "spending cap reached"]
    handler.flush()
    assert capture.messages == ["pending", "spending cap reached"]


def test_realistic_formatted_output_compacts_repeated_agent_line() -> None:
    """Compaction should work with production-like formatter metadata."""
    stream = StringIO()
    delegate = logging.StreamHandler(stream)
    delegate.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))
    handler = CompactingHandler(delegate)
    logger = logging.getLogger("AgentRuntime[core-infra-dev]")
    logger.handlers.clear()
    logger.setLevel(logging.INFO)
    logger.propagate = False
    logger.addHandler(handler)

    repeated = (
        "[core-infra-dev] No eligible ready tasks for role core-infra-dev "
        "(global_ready=3) on goal orchestrator-ui"
    )
    logger.info(repeated)
    logger.info(repeated)
    logger.info(repeated)
    logger.info("[core-infra-dev] Found 3 ready tasks for goal orchestrator-ui")
    handler.flush()

    output = stream.getvalue().splitlines()
    assert len(output) == 2
    assert output[0].endswith(f"INFO AgentRuntime[core-infra-dev]: +3 {repeated}")
    assert output[1].endswith(
        "INFO AgentRuntime[core-infra-dev]: [core-infra-dev] Found 3 ready tasks for goal orchestrator-ui"
    )


def test_realistic_interleaved_agents_do_not_false_merge() -> None:
    """Interleaved agent lines should remain distinct when not consecutive-equal."""
    stream = StringIO()
    delegate = logging.StreamHandler(stream)
    delegate.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))
    handler = CompactingHandler(delegate)
    logger = logging.getLogger("orchestrator.sim")
    logger.handlers.clear()
    logger.setLevel(logging.INFO)
    logger.propagate = False
    logger.addHandler(handler)

    lines = [
        "[owner] Found 3 ready tasks for goal orchestrator-ui",
        "[team-lead] Found 3 ready tasks for goal orchestrator-ui",
        "[owner] Found 3 ready tasks for goal orchestrator-ui",
        "[features-dev] No eligible ready tasks for role features-dev (global_ready=3) on goal orchestrator-ui",
        "[owner] No eligible ready tasks for role owner (global_ready=3) on goal orchestrator-ui",
        "[features-dev] No eligible ready tasks for role features-dev (global_ready=3) on goal orchestrator-ui",
    ]
    for line in lines:
        logger.info(line)
    handler.flush()

    output = stream.getvalue().splitlines()
    assert len(output) == len(lines)
    assert all("+" not in line.split(": ", 1)[1] for line in output)
