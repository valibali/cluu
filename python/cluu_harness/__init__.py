"""CLUU gen2 test harness.

Event/log-capture-based QEMU integration test harness for the CLUU
microkernel. Python port of ``scripts/harness_run.sh`` et al., designed
for extensibility:

* Test cases are declared via the :func:`cluu_case` decorator or the
  :class:`Case` dataclass — no shell case-file editing required.
* Serial output is tailed in a background thread; markers and faults
  are matched as they stream in. Timeouts are safety bounds only, never
  the pass/fail criterion.
* QEMU monitor (unix socket), GDB attach, and framebuffer dump are
  first-class building blocks.

See ``README.md`` for the quick start.
"""

from __future__ import annotations

# Import the catalog to register built-in cases. Side-effect import —
# the decorators populate ``registry`` on import.
from cluu_harness import catalog  # noqa: F401, E402
from cluu_harness.cases import Case, cluu_case, registry
from cluu_harness.config import HarnessConfig
from cluu_harness.suite import SuiteResult, run_case, run_suite

__all__ = [
    "Case",
    "HarnessConfig",
    "SuiteResult",
    "catalog",
    "cluu_case",
    "registry",
    "run_case",
    "run_suite",
]

__version__ = "0.1.0"
