"""CLI entrypoint for orchestrator/agent roles."""

from __future__ import annotations

import argparse
import logging
import shutil
import sys
from datetime import timedelta
from pathlib import Path

import redis

try:
    import fakeredis
except ImportError:
    fakeredis = None

from orchestrator.cache import RedisStore
from orchestrator.llm import LLMRuntimeConfig, create_llm_client
from orchestrator.logging_utils import CompactingHandler
from orchestrator.orchestrator import Orchestrator, RealSpawnAdapter
from orchestrator.runtime import AgentRuntime

LOGGER = logging.getLogger(__name__)


def _build_parser() -> argparse.ArgumentParser:
    """Build CLI argument parser."""
    parser = argparse.ArgumentParser(description="Systemg agent/orchestrator entrypoint")
    parser.add_argument(
        "--role", required=True, choices=["agent", "orchestrator"], help="Process role"
    )
    parser.add_argument(
        "--instructions", type=Path, required=True, help="Primary instructions file"
    )
    parser.add_argument("--redis-url", default="fakeredis://", help="Redis connection URL")
    parser.add_argument("--log-level", default="INFO", help="Python logging level")
    parser.add_argument("--agent-name", help="Agent identifier when running in agent mode")
    parser.add_argument("--agent-role", help="Agent role identifier when running in agent mode")
    parser.add_argument("--goal-id", help="Goal identifier for the active DAG")
    parser.add_argument("--heartbeat", type=Path, help="Heartbeat file path for agent role")
    parser.add_argument(
        "--loop-interval", type=float, default=1.0, help="Agent loop interval in seconds"
    )
    parser.add_argument("--lease-ttl", type=float, default=30.0, help="Lease TTL in seconds")
    parser.add_argument(
        "--poll-interval", type=float, default=5.0, help="Orchestrator poll interval in seconds"
    )
    parser.add_argument(
        "--heartbeat-interval",
        type=float,
        default=300.0,
        help="Agent heartbeat file read interval in seconds",
    )
    parser.add_argument(
        "--instruction-interval",
        type=float,
        default=300.0,
        help="Agent instructions reload interval in seconds",
    )
    parser.add_argument("--claude-cli", default="claude", help="Path to the Claude CLI executable")
    parser.add_argument(
        "--claude-extra-arg",
        action="append",
        default=[],
        help="Additional arguments for the Claude CLI",
    )
    parser.add_argument(
        "--claude-use-sysg",
        action="store_true",
        help="Invoke Claude through `sysg spawn --ttl` to capture stdout/stderr",
    )
    parser.add_argument(
        "--llm-provider",
        choices=["claude", "codex"],
        help="LLM provider used for orchestration and agents",
    )
    parser.add_argument("--llm-cli", help="Path to provider CLI executable")
    parser.add_argument(
        "--llm-extra-arg",
        action="append",
        default=[],
        help="Additional arguments for the selected LLM CLI",
    )
    parser.add_argument(
        "--llm-use-sysg",
        action="store_true",
        help="Invoke LLM CLI through `sysg spawn` for output capture",
    )
    return parser


def _redis_client_from_url(url: str):
    """Construct Redis client from connection URL."""
    if url.startswith("fakeredis://"):
        if not fakeredis:
            raise RuntimeError("fakeredis is not installed")
        return fakeredis.FakeRedis(decode_responses=False)
    return redis.Redis.from_url(url)


def _configure_logging(level: str) -> None:
    """Configure root logging format and level."""
    root = logging.getLogger()
    root.handlers.clear()
    root.setLevel(getattr(logging, level.upper(), logging.INFO))
    stream = logging.StreamHandler()
    stream.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))
    root.addHandler(CompactingHandler(stream))


def _resolve_llm_config(args: argparse.Namespace) -> LLMRuntimeConfig:
    """Resolve provider config from generic flags and legacy Claude aliases."""
    provider = (args.llm_provider or "claude").strip().lower()
    executable = (args.llm_cli or "").strip()
    extra_args = tuple(args.llm_extra_arg or [])
    use_sysg_spawn = bool(args.llm_use_sysg)

    if args.llm_provider is None:
        if args.claude_cli != "claude" or args.claude_extra_arg or args.claude_use_sysg:
            provider = "claude"
    if not executable:
        provider_binary = "claude" if provider == "claude" else "codex"
        if provider == "claude" and args.claude_cli != "claude":
            provider_binary = args.claude_cli
        executable = shutil.which(provider_binary) or provider_binary
    if not extra_args and args.claude_extra_arg and provider == "claude":
        extra_args = tuple(args.claude_extra_arg)
    if not use_sysg_spawn and args.claude_use_sysg and provider == "claude":
        use_sysg_spawn = True

    return LLMRuntimeConfig(
        provider=provider,
        executable=executable,
        extra_args=extra_args,
        use_sysg_spawn=use_sysg_spawn,
    )


def run_cli(argv: list[str] | None = None) -> int:
    """Execute CLI entrypoint logic and return process exit code."""
    parser = _build_parser()
    args = parser.parse_args(argv)
    _configure_logging(args.log_level)

    client = _redis_client_from_url(args.redis_url)
    store = RedisStore(client)
    llm_config = _resolve_llm_config(args)

    if args.role == "agent":
        if not args.agent_name or not args.goal_id or not args.heartbeat:
            parser.error("Agent role requires --agent-name, --goal-id, and --heartbeat")
        llm_client = create_llm_client(llm_config)
        runtime = AgentRuntime(
            agent_name=args.agent_name,
            agent_role=args.agent_role or args.agent_name,
            goal_id=args.goal_id,
            instructions_path=args.instructions,
            heartbeat_path=args.heartbeat,
            redis_store=store,
            llm_client=llm_client,
            loop_interval=args.loop_interval,
            lease_ttl=timedelta(seconds=args.lease_ttl),
            heartbeat_refresh_interval=timedelta(seconds=args.heartbeat_interval),
            instructions_refresh_interval=timedelta(seconds=args.instruction_interval),
        )
        runtime.run()
        return 0

    llm_client = create_llm_client(llm_config)
    orchestrator = Orchestrator(
        instructions_path=args.instructions,
        redis_store=store,
        redis_url=args.redis_url,
        llm_client=llm_client,
        spawn_adapter=RealSpawnAdapter(),
        poll_interval=args.poll_interval,
        heartbeat_interval=args.heartbeat_interval,
        instruction_interval=args.instruction_interval,
        llm_config=llm_config,
    )
    orchestrator.run()
    return 0


if __name__ == "__main__":
    sys.exit(run_cli())
