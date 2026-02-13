"""Constants for orchestrator configuration."""

from datetime import timedelta

HEARTBEAT_INTERVAL_SECONDS = 30
INSTRUCTION_REFRESH_INTERVAL = timedelta(seconds=30)
DEFAULT_POLL_INTERVAL = 5.0
AGENT_LOOP_INTERVAL = 1.0
