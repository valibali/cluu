"""Event-driven serial log tail and matcher.

This is the heart of the gen2 harness. The bash version polls the
serial log with ``grep -Fq`` every 0.5s. This module instead:

* Tails the serial file in a background thread (no busy-poll).
* Notifies subscribers via callbacks the instant a new line appears.
* Lets a caller ``wait_for`` a marker / fault / fail pattern, returning
  as soon as the pattern matches — timeouts are *safety bounds*, never
  the pass/fail criterion.

Knowledge-vault reference: ``cluu-harness-serial-is-streaming`` — the
serial log is a live stream, and a short ``RUN_WAIT`` only means QEMU
was killed mid-boot, not that the kernel failed to boot.
"""

from __future__ import annotations

import contextlib
import logging
import re
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path

log = logging.getLogger(__name__)


class WaitResult(Enum):
    """Why a ``wait_for`` returned."""

    MATCHED = "matched"        # pattern appeared
    FAULT = "fault"            # fault pattern appeared (and not expected)
    FAIL = "fail"              # explicit fail marker appeared
    TIMEOUT = "timeout"        # safety bound elapsed
    QEMU_DIED = "qemu_died"    # QEMU process exited before any match


@dataclass
class WaitForOutcome:
    result: WaitResult
    elapsed_s: float
    matched_line: str | None = None
    matched_pattern: str | None = None
    missing_markers: list[str] = field(default_factory=list)
    fault_lines: list[str] = field(default_factory=list)


class SerialStream:
    """Background-thread tail of the QEMU COM2 serial log."""

    def __init__(
        self,
        serial_log: Path,
        qemu_alive: Callable[[], bool] | None = None,
        poll_interval_s: float = 0.2,
    ) -> None:
        self.serial_log = serial_log
        self._qemu_alive = qemu_alive
        self._poll_interval_s = poll_interval_s
        self._lines: list[str] = []
        self._lock = threading.Lock()
        self._cond = threading.Condition(self._lock)
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._started = False
        # Pre-compiled patterns for fault/fail detection.
        self._fault_re: re.Pattern[str] | None = None
        self._fail_re: re.Pattern[str] | None = None

    # ------------------------------------------------------------------ #
    # Lifecycle
    # ------------------------------------------------------------------ #
    def start(self) -> None:
        if self._started:
            return
        self._started = True
        # Truncate the log so stale data from a previous run can't match.
        with contextlib.suppress(OSError):
            self.serial_log.write_bytes(b"")
        self._thread = threading.Thread(
            target=self._tail, name="cluu-serial-tail", daemon=True
        )
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=2.0)
            self._thread = None

    def __enter__(self) -> SerialStream:
        self.start()
        return self

    def __exit__(self, *_exc: object) -> None:
        self.stop()

    # ------------------------------------------------------------------ #
    # Pattern config
    # ------------------------------------------------------------------ #
    def set_fault_pattern(self, pattern: str) -> None:
        self._fault_re = re.compile(pattern, re.IGNORECASE)

    def set_fail_pattern(self, pattern: str) -> None:
        self._fail_re = re.compile(pattern)

    # ------------------------------------------------------------------ #
    # Snapshot
    # ------------------------------------------------------------------ #
    def snapshot(self) -> str:
        """Return the full serial log captured so far."""
        with self._lock:
            return "".join(self._lines)

    def tail(self, n: int = 200) -> str:
        with self._lock:
            return "".join(self._lines[-n:])

    def line_count(self) -> int:
        with self._lock:
            return len(self._lines)

    # ------------------------------------------------------------------ #
    # Waiting (the gen2 primitive)
    # ------------------------------------------------------------------ #
    def wait_for(
        self,
        required_markers: list[str],
        *,
        timeout_s: float,
        expect_fault: bool = False,
        fault_re: re.Pattern[str] | None = None,
        fail_re: re.Pattern[str] | None = None,
        early_exit_on_all_markers: bool = True,
    ) -> WaitForOutcome:
        """Wait until all ``required_markers`` appear in the serial stream.

        Returns as soon as one of these is true (whichever fires first):

        * All markers present → ``WaitResult.MATCHED``
        * Fault pattern matches and ``not expect_fault`` → ``WaitResult.FAULT``
        * Fail pattern matches → ``WaitResult.FAIL``
        * QEMU process dies → ``WaitResult.QEMU_DIED``
        * ``timeout_s`` elapses → ``WaitResult.TIMEOUT``

        The timeout is a safety bound, NOT the pass criterion — see
        ``cluu-harness-serial-is-streaming``.
        """
        fault_re = fault_re or self._fault_re
        fail_re = fail_re or self._fail_re
        start = time.monotonic()
        deadline = start + timeout_s
        seen: set[str] = set()
        fault_lines: list[str] = []

        with self._cond:
            while True:
                # Check current state against markers + patterns.
                state = self._check_state(
                    required_markers, seen, fault_re, fail_re, fault_lines
                )

                if state == "matched" and early_exit_on_all_markers:
                    return WaitForOutcome(
                        result=WaitResult.MATCHED,
                        elapsed_s=time.monotonic() - start,
                        missing_markers=[],
                    )
                if state == "fault" and not expect_fault:
                    return WaitForOutcome(
                        result=WaitResult.FAULT,
                        elapsed_s=time.monotonic() - start,
                        fault_lines=list(fault_lines),
                    )
                if state == "fail":
                    return WaitForOutcome(
                        result=WaitResult.FAIL,
                        elapsed_s=time.monotonic() - start,
                        fault_lines=list(fault_lines),
                    )

                # QEMU died?
                if self._qemu_alive is not None and not self._qemu_alive():
                    missing = [m for m in required_markers if m not in seen]
                    return WaitForOutcome(
                        result=WaitResult.QEMU_DIED,
                        elapsed_s=time.monotonic() - start,
                        missing_markers=missing,
                        fault_lines=list(fault_lines),
                    )

                # Timeout?
                now = time.monotonic()
                if now >= deadline:
                    missing = [m for m in required_markers if m not in seen]
                    if not required_markers:
                        # No markers → timeout is the expected exit.
                        return WaitForOutcome(
                            result=WaitResult.TIMEOUT, elapsed_s=now - start
                        )
                    return WaitForOutcome(
                        result=WaitResult.TIMEOUT,
                        elapsed_s=now - start,
                        missing_markers=missing,
                        fault_lines=list(fault_lines),
                    )

                # Wait for new lines or a short timeout to re-check qemu_alive.
                remaining = deadline - now
                wait_s = min(self._poll_interval_s, remaining)
                self._cond.wait(timeout=wait_s)

    def wait_for_shell_ready(self, timeout_s: float) -> WaitForOutcome:
        """Convenience: wait for the ``[USER] shell: ready`` marker."""
        return self.wait_for(["[USER] shell: ready"], timeout_s=timeout_s)

    def wait_for_serial_activity(self, timeout_s: float) -> bool:
        """Return True if any serial output appears within ``timeout_s``."""
        start = time.monotonic()
        deadline = start + timeout_s
        with self._cond:
            while time.monotonic() < deadline:
                if self._lines:
                    return True
                if self._qemu_alive is not None and not self._qemu_alive():
                    return False
                self._cond.wait(timeout=self._poll_interval_s)
        return False

    # ------------------------------------------------------------------ #
    # Internal: tail thread
    # ------------------------------------------------------------------ #
    def _tail(self) -> None:
        """Background: follow ``serial_log`` like ``tail -f``."""
        # Wait for the file to appear (QEMU creates it on startup).
        wait_deadline = time.monotonic() + 10.0
        while not self.serial_log.exists():
            if self._stop.is_set() or time.monotonic() > wait_deadline:
                return
            time.sleep(0.1)

        with open(self.serial_log, encoding="utf-8", errors="replace") as f:
            while not self._stop.is_set():
                line = f.readline()
                if line:
                    with self._cond:
                        self._lines.append(line)
                        self._cond.notify_all()
                else:
                    # No data; check if QEMU is still alive.
                    if self._qemu_alive is not None and not self._qemu_alive():
                        return
                    time.sleep(self._poll_interval_s)

    def _check_state(
        self,
        required_markers: list[str],
        seen: set[str],
        fault_re: re.Pattern[str] | None,
        fail_re: re.Pattern[str] | None,
        fault_lines: list[str],
    ) -> str:
        """Inspect the buffer. Returns 'matched', 'fault', 'fail', or 'pending'.

        Caller MUST hold ``self._cond`` (i.e. ``self._lock``) — this method
        does not re-acquire to avoid a re-entrant-lock deadlock.
        """
        lines = list(self._lines)

        # Update seen-set and scan for fault/fail in one pass.
        for line in lines:
            for marker in required_markers:
                if marker not in seen and marker in line:
                    seen.add(marker)
            if fault_re is not None and fault_re.search(line):
                fault_lines.append(line)
                if any(m not in seen for m in required_markers):
                    return "fault"
            if fail_re is not None and fail_re.search(line):
                return "fail"

        if required_markers and all(m in seen for m in required_markers):
            return "matched"
        if not required_markers and not lines:
            return "pending"
        return "pending"


__all__ = ["SerialStream", "WaitForOutcome", "WaitResult"]
