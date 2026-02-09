"""CLI entrypoint for orchestrator/agent roles."""

from __future__ import annotations

import argparse
import logging
import sys
from datetime import timedelta
from pathlib import Path

import redis

try:  # optional dependency for tests
    import fakeredis
except ImportError:  # pragma: no cover
    fakeredis = None  # type: ignore

from orchestrator.cache import RedisStore
from orchestrator.llm import ClaudeCLIClient
from orchestrator.orchestrator import Orchestrator, RealSpawnAdapter
from orchestrator.runtime import AgentRuntime

LOGGER = logging.getLogger(__name__)


def _build_parser() -> argparse.ArgumentParser:
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
    parser.add_argument("--goal-id", help="Goal identifier for the active DAG")
    parser.add_argument("--heartbeat", type=Path, help="Heartbeat file path for agent role")
    parser.add_argument(
        "--loop-interval", type=float, default=1.0, help="Agent loop interval in seconds"
    )
    parser.add_argument("--lease-ttl", type=float, default=30.0, help="Lease TTL in seconds")
    parser.add_argument(
        "--poll-interval", type=float, default=5.0, help="Orchestrator poll interval in seconds"
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
        help="Invoke Claude through `sysg spawn --oneshot` to capture stdout/stderr",
    )
    return parser


def _redis_client_from_url(url: str):
    if url.startswith("fakeredis://"):
        if not fakeredis:
            raise RuntimeError("fakeredis is not installed")
        return fakeredis.FakeRedis(decode_responses=False)
    return redis.Redis.from_url(url)


def _configure_logging(level: str) -> None:
    logging.basicConfig(
        level=getattr(logging, level.upper(), logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )


def run_cli(argv: list[str] | None = None) -> int:
    parser = _build_parser()
    args = parser.parse_args(argv)
    _configure_logging(args.log_level)

    client = _redis_client_from_url(args.redis_url)
    store = RedisStore(client)

    llm_client = ClaudeCLIClient(
        executable=args.claude_cli,
        extra_args=args.claude_extra_arg,
        use_sysg_spawn=args.claude_use_sysg,
    )

    if args.role == "agent":
        if not args.agent_name or not args.goal_id or not args.heartbeat:
            parser.error("Agent role requires --agent-name, --goal-id, and --heartbeat")
        runtime = AgentRuntime(
            agent_name=args.agent_name,
            goal_id=args.goal_id,
            instructions_path=args.instructions,
            heartbeat_path=args.heartbeat,
            redis_store=store,
            llm_client=llm_client,
            loop_interval=args.loop_interval,
            lease_ttl=timedelta(seconds=args.lease_ttl),
        )
        runtime.run()
        return 0

    orchestrator = Orchestrator(
        instructions_path=args.instructions,
        redis_store=store,
        redis_url=args.redis_url,
        llm_client=llm_client,
        spawn_adapter=RealSpawnAdapter(),
        poll_interval=args.poll_interval,
    )
    orchestrator.run()
    return 0


if __name__ == "__main__":
    sys.exit(run_cli())
