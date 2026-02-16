"""Constants for orchestrator configuration."""

from datetime import timedelta

PROMPT_TIMEOUT_SECONDS = 900
"""Default timeout in seconds for each LLM prompt operation."""

HEARTBEAT_INTERVAL_SECONDS = 120
"""Default interval in seconds for reading heartbeat directive files."""

HEARTBEAT_REFRESH_INTERVAL = timedelta(seconds=HEARTBEAT_INTERVAL_SECONDS)
"""Timedelta form of heartbeat refresh cadence."""

INSTRUCTION_REFRESH_INTERVAL = timedelta(seconds=120)
"""Default interval as timedelta for reloading instruction files."""

DEFAULT_POLL_INTERVAL = 5.0
"""Default orchestrator reconcile-loop sleep interval in seconds."""

AGENT_LOOP_INTERVAL = 1.0
"""Default agent main-loop sleep interval in seconds."""
