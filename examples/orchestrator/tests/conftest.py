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
    if not fakeredis:
        pytest.skip("fakeredis not available")
    return fakeredis.FakeRedis(decode_responses=False)


@pytest.fixture()
def redis_store() -> RedisStore:
    client = _require_fakeredis()
    return RedisStore(client)


@pytest.fixture(scope="session")
def claude_client() -> ClaudeCLIClient:
    import shutil

    executable = shutil.which("claude")
    if not executable:
        raise RuntimeError("Claude CLI executable not found in PATH")
    return ClaudeCLIClient(executable=executable)


@pytest.fixture()
def assets_dir() -> Path:
    return EXAMPLE_ROOT / "tests" / "assets"


def pytest_configure(config):  # pragma: no cover - pytest hook
    config.addinivalue_line("markers", "live_claude: marks tests that hit the real Claude API")
