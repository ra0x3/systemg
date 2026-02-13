import os
import sys
from pathlib import Path

import pytest

EXAMPLE_ROOT = Path(__file__).resolve().parents[1]

if str(EXAMPLE_ROOT) not in sys.path:
    sys.path.insert(0, str(EXAMPLE_ROOT))

from orchestrator.cache import RedisStore  # noqa: E402
from orchestrator.llm import ClaudeCLIClient  # noqa: E402

try:
    import fakeredis  # type: ignore
except ImportError:  # pragma: no cover
    fakeredis = None


def _require_fakeredis() -> "fakeredis.FakeRedis":  # type: ignore
    """Return fakeredis client or skip when unavailable."""
    if not fakeredis:
        pytest.skip("fakeredis not available")
    return fakeredis.FakeRedis(decode_responses=False)


@pytest.fixture()
def redis_store() -> RedisStore:
    """Provide a RedisStore backed by fakeredis."""
    client = _require_fakeredis()
    return RedisStore(client)


@pytest.fixture(scope="session")
def claude_client() -> ClaudeCLIClient:
    """Provide live Claude client when executable exists."""
    import shutil

    executable = shutil.which("claude")
    if not executable:
        raise RuntimeError("Claude CLI executable not found in PATH")
    return ClaudeCLIClient(executable=executable)


@pytest.fixture()
def assets_dir() -> Path:
    """Return path to static test assets."""
    return EXAMPLE_ROOT / "tests" / "assets"


def pytest_configure(config):  # pragma: no cover - pytest hook
    """Register custom pytest markers."""
    config.addinivalue_line("markers", "live_claude: marks tests that hit the real Claude API")


def pytest_collection_modifyitems(config, items):  # pragma: no cover - pytest hook
    """Skip live Claude tests unless explicitly enabled."""
    if os.getenv("RUN_LIVE_CLAUDE") == "1":
        return
    skip_live = pytest.mark.skip(reason="set RUN_LIVE_CLAUDE=1 to run live Claude tests")
    for item in items:
        if "live_claude" in item.keywords:
            item.add_marker(skip_live)
