"""pytest tests for the CLUU harness.

Cases are declared in ``cluu_harness.catalog`` (imported by the package
``__init__``), so both the CLI and pytest see the same registrations.
This file holds only the tests + parametrization.
"""

from __future__ import annotations

import pytest

from cluu_harness import registry
from cluu_harness.markers import MARKER_MODES

_ALL_CASE_NAMES = registry.names()


@pytest.mark.slow
@pytest.mark.parametrize("case_name", _ALL_CASE_NAMES)
def test_registered_case(case_name: str, run_cluu_case) -> None:
    """Run one registered case via QEMU. Deselected by default (slow)."""
    result = run_cluu_case(case_name)
    assert result.passed, (
        f"{case_name} failed: error={result.error} "
        f"missing={result.missing_markers} fail={result.fail_line} "
        f"slo={result.post_check_message}"
    )


@pytest.mark.smoke
def test_registry_populated() -> None:
    assert len(registry.names()) >= 10, "expected at least 10 registered cases"


@pytest.mark.smoke
@pytest.mark.parametrize("mode", sorted(MARKER_MODES))
def test_marker_mode_known(mode: str) -> None:
    spec = MARKER_MODES[mode]
    assert spec.name == mode


@pytest.mark.smoke
def test_case_defaults_exist_for_registered_modes() -> None:
    from cluu_harness.case_defaults import get_defaults

    for case in registry.all_cases():
        defaults = get_defaults(case.marker_mode)
        assert defaults is not None


@pytest.mark.smoke
def test_sendkey_translation_roundtrip() -> None:
    from cluu_harness.sendkey import command_to_sendkeys, unsupported_chars

    cmd = "ls /etc"
    assert unsupported_chars(cmd) == []
    keys = command_to_sendkeys(cmd)
    assert keys == ["l", "s", "spc", "shift-6", "e", "t", "c", "ret"]


@pytest.mark.smoke
def test_serial_stream_timeout_with_no_markers(tmp_path) -> None:
    from cluu_harness.serial_stream import SerialStream, WaitResult

    log_file = tmp_path / "serial.log"
    log_file.write_bytes(b"")
    with SerialStream(log_file, qemu_alive=lambda: True) as stream:
        outcome = stream.wait_for(["nonexistent"], timeout_s=1.0)
    assert outcome.result == WaitResult.TIMEOUT
    assert outcome.missing_markers == ["nonexistent"]


@pytest.mark.smoke
def test_serial_stream_event_matches(tmp_path) -> None:
    import threading
    import time as _time

    from cluu_harness.serial_stream import SerialStream, WaitResult

    log_file = tmp_path / "serial.log"
    log_file.write_bytes(b"")

    def _write_after_delay() -> None:
        _time.sleep(0.3)
        with open(log_file, "a", encoding="utf-8") as f:
            f.write("[USER] shell: ready\n")

    with SerialStream(log_file, qemu_alive=lambda: True) as stream:
        t = threading.Thread(target=_write_after_delay, daemon=True)
        t.start()
        outcome = stream.wait_for(["[USER] shell: ready"], timeout_s=3.0)
        t.join(timeout=2.0)
    assert outcome.result == WaitResult.MATCHED
    assert outcome.elapsed_s < 2.0


@pytest.mark.smoke
def test_metrics_p95() -> None:
    from cluu_harness.metrics import _percentile_p95

    assert _percentile_p95([]) is None
    assert _percentile_p95([100]) == 100
    assert _percentile_p95(list(range(1, 21))) == 19


@pytest.mark.smoke
def test_case_dataclass_defaults() -> None:
    from cluu_harness import Case

    c = Case(name="x", marker_mode="none")
    assert c.test_command_repeat == 1
    assert c.build_mode == "full"
    assert c.tags == []


@pytest.mark.smoke
def test_duplicate_case_registration_rejected() -> None:
    from cluu_harness import Case, registry

    registry.register(Case(name="dup_test", marker_mode="none"))
    with pytest.raises(ValueError, match="duplicate"):
        registry.register(Case(name="dup_test", marker_mode="none"))
    registry._cases.pop("dup_test", None)
