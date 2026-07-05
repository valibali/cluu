"""pytest integration for the CLUU harness.

Fixtures:
* ``harness_config`` — a fresh :class:`HarnessConfig` from env vars.
* ``run_cluu_case`` — factory that runs a named case and returns the
  :class:`CaseResult`. Skips the test if QEMU/image is unavailable.

Cases are auto-registered by importing ``cluu_harness`` (which imports
``cluu_harness.catalog`` and fires the ``@cluu_case`` decorators).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from cluu_harness.cases import registry
from cluu_harness.config import HarnessConfig
from cluu_harness.suite import CaseResult, run_case


@pytest.fixture
def harness_config() -> HarnessConfig:
    """Fresh config from current env vars."""
    return HarnessConfig()


@pytest.fixture
def run_cluu_case(harness_config: HarnessConfig):
    """Factory: run a registered case by name, skip if QEMU unavailable."""
    import shutil

    if shutil.which("qemu-system-x86_64") is None:
        pytest.skip("qemu-system-x86_64 not on PATH")

    def _run(name: str) -> CaseResult:
        case = registry.get(name)
        return run_case(case, harness_config)

    return _run


@pytest.fixture(scope="session")
def ensure_registered():
    """Import cluu_harness so catalog decorators register everything."""
    import cluu_harness  # noqa: F401 — side-effect import
    return registry.names()


# Make pytest inject the python/ dir onto sys.path so the in-tree
# package is importable without an install step.
_ROOT = Path(__file__).resolve().parent.parent
_PYTHON_DIR = _ROOT / "python"
if str(_PYTHON_DIR) not in __import__("sys").path:
    __import__("sys").path.insert(0, str(_PYTHON_DIR))


# Convenience: expose run helpers as pytest fixtures for ad-hoc use.
@pytest.fixture
def cluu_run_case():
    from cluu_harness.suite import run_case as _run
    return _run


@pytest.fixture
def cluu_run_suite():
    from cluu_harness.suite import run_suite as _run
    return _run


# Re-export for test files that do `from conftest import *`.
__all__ = [
    "cluu_run_case",
    "cluu_run_suite",
    "ensure_registered",
    "harness_config",
    "run_cluu_case",
]
