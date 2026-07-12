"""Suite runner: orchestrates build → launch → inject → wait → verify.

The runner is the Python equivalent of ``harness_run.sh``'s main body.
It is intentionally synchronous (per the AGENTS.md §7 sync constraint
for ``top``/``/proc``): the *event matcher* is threaded, but the runner
itself drives the steps in order.
"""

from __future__ import annotations

import functools
import logging
import time
from dataclasses import dataclass, field
from pathlib import Path

from cluu_harness.case_defaults import get_defaults
from cluu_harness.cases import Case
from cluu_harness.config import HarnessConfig
from cluu_harness.gdb import run_gdb_control
from cluu_harness.markers import (
    PostCheckContext,
    get_spec,
    run_post_check,
)
from cluu_harness.metrics import extract_fail_marker
from cluu_harness.qemu import QemuController
from cluu_harness.sendkey import command_to_sendkeys, unsupported_chars
from cluu_harness.serial_stream import SerialStream, WaitResult

log = logging.getLogger(__name__)


@dataclass
class CaseResult:
    """Outcome of running one case."""

    name: str
    passed: bool
    elapsed_s: float
    serial_log: Path
    missing_markers: list[str] = field(default_factory=list)
    fault_lines: list[str] = field(default_factory=list)
    fail_line: str | None = None
    post_check_message: str = ""
    metrics: dict[str, object] = field(default_factory=dict)
    error: str | None = None

    @property
    def status(self) -> str:
        return "PASS" if self.passed else "FAIL"


@dataclass
class SuiteResult:
    """Aggregate outcome of a suite run."""

    total: int = 0
    passed: int = 0
    failed: int = 0
    skipped: int = 0
    cases: list[CaseResult] = field(default_factory=list)
    duration_s: float = 0.0

    @property
    def passed_names(self) -> list[str]:
        return [c.name for c in self.cases if c.passed]

    @property
    def failed_names(self) -> list[str]:
        return [c.name for c in self.cases if not c.passed]


# ---------------------------------------------------------------------- #
# Single-case execution
# ---------------------------------------------------------------------- #

def run_case(case: Case, cfg: HarnessConfig | None = None) -> CaseResult:
    """Run one case end-to-end. Returns a :class:`CaseResult`."""
    cfg = cfg or HarnessConfig()
    start = time.monotonic()
    # Apply per-case overrides onto a config copy (env-style).
    cfg = _apply_case_overrides(cfg, case)

    # usb_input_probe: usb-ehci + usb-kbd/mouse (both default to USB 2.0 high-speed)
    if case.marker_mode in ("xhciprobe", "usb_input_probe"):
        usb_args = "-device usb-ehci,id=ehci -device usb-kbd,bus=ehci.0 -device usb-mouse,bus=ehci.0"
        existing = cfg.qemu_extra_args.strip()
        cfg.qemu_extra_args = (existing + " " + usb_args).strip() if existing else usb_args

    # l2_net_boot / l2_socket_basic / l2_dhcp_ping / l2_wget / l2_curl / l2_dns: add virtio-net-pci NIC
    if case.marker_mode in ("l2_net_boot", "l2_dhcp_ping", "l2_socket_basic", "l2_net_denied", "l2_dns_basic", "l2_wget_basic", "l2_curl_basic"):
        cfg.cluu_net = True

    http_server_proc = None
    if case.marker_mode in ("l2_wget_basic", "l2_curl_basic"):
        import http.server
        import socketserver
        import tempfile
        import os
        import threading

        http_dir = tempfile.mkdtemp(prefix="cluu-http-")
        with open(os.path.join(http_dir, "index.html"), "w") as f:
            f.write("<html><body>CLUU test page</body></html>")
        handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=http_dir)
        socketserver.TCPServer.allow_reuse_address = True
        http_server_proc = socketserver.TCPServer(("127.0.0.1", 9876), handler)
        http_server_proc.timeout = 0.1
        t = threading.Thread(target=http_server_proc.serve_forever, daemon=True)
        t.start()
        log.info("host HTTP server started on 127.0.0.1:9876 (serving %s)", http_dir)

    spec = get_spec(cfg.marker_mode)
    required_markers = (
        case.required_markers_override
        if case.required_markers_override is not None
        else list(spec.required_markers)
    )
    if cfg.required_markers_override is not None:
        required_markers = list(cfg.required_markers_override)

    result = CaseResult(
        name=case.name, passed=False, elapsed_s=0.0, serial_log=cfg.serial_log
    )

    qemu = QemuController(cfg)
    try:
        # 1. Build (unless --no-build).
        if case.build_mode == "no_build":
            cfg.no_build = True
        qemu.build()

        # 2. Launch QEMU.
        qemu.launch()

        # 3. Wire the serial stream (event-driven tail).
        with SerialStream(
            cfg.serial_log, qemu_alive=lambda: qemu.is_alive
        ) as serial:
            serial.set_fault_pattern(cfg.fault_pattern)
            serial.set_fail_pattern(cfg.fail_pattern)

            # 4. GDB attach (if enabled).
            run_gdb_control(cfg.qemu_gdb, serial)

            # 5. Boot wait (optional fixed sleep before marker polling).
            if cfg.boot_wait_s > 0:
                log.info("waiting %ds before shell marker polling", cfg.boot_wait_s)
                time.sleep(cfg.boot_wait_s)

            # 6. SENDKEY_SEQUENCE_NOWAIT: fire creds before shell ready.
            sendkey_fired_nowait = False
            if cfg.keystroke_commands or case.keystroke_commands:
                # If we have typed commands, we need the shell ready first.
                pass
            if case.sendkey_sequence_nowait is True or (
                case.sendkey_sequence_nowait is None
                and _derive_nowait(cfg, case)
            ):
                _run_sendkey_sequence(qemu.monitor, _resolve_sequence(cfg, case))
                sendkey_fired_nowait = True

            # 7. Wait for shell ready (if we have typed commands or a
            #    non-nowait sendkey sequence).
            need_shell_ready = bool(
                _typed_commands(cfg, case) or _post_sendkey(cfg, case)
            ) or (
                _resolve_sequence(cfg, case)
                and not sendkey_fired_nowait
            )
            if need_shell_ready:
                outcome = serial.wait_for_shell_ready(cfg.shell_ready_wait_s)
                if outcome.result != WaitResult.MATCHED:
                    result.error = (
                        f"shell readiness not observed within "
                        f"{cfg.shell_ready_wait_s}s (result={outcome.result.value})"
                    )
                    result.missing_markers = outcome.missing_markers
                    result.fault_lines = outcome.fault_lines
                    result.elapsed_s = time.monotonic() - start
                    _dump_serial_tail(result, serial)
                    return result

            # 8. Type the test command(s) + extra keystroke commands.
            typed = _typed_commands(cfg, case)
            if typed:
                _type_commands(qemu.monitor, typed, cfg)

            # 9. Post-sendkey (e.g. ctrl-c for SIGINT cases).
            post = _post_sendkey(cfg, case)
            if post:
                time.sleep(cfg.post_sendkey_delay_s)
                qemu.monitor.send_key(post)

            # 10. Non-nowait sendkey sequence.
            if not sendkey_fired_nowait:
                seq = _resolve_sequence(cfg, case)
                if seq:
                    _run_sendkey_sequence(qemu.monitor, seq)

            # 11. Wait for required markers (event-driven). RUN_WAIT is
            #     the safety bound.
            outcome = serial.wait_for(
                required_markers,
                timeout_s=cfg.run_wait_s,
                expect_fault=cfg.expect_fault,
            )

            result.missing_markers = outcome.missing_markers
            result.fault_lines = outcome.fault_lines

            # 12. Decide pass/fail.
            if outcome.result == WaitResult.MATCHED:
                # Run per-mode post-checks (metrics / SLOs).
                ctx = PostCheckContext(
                    serial_text=serial.snapshot(),
                    spec=spec,
                    min_exit_cookies=cfg.min_exit_cookies,
                    limits=_slo_limits(cfg),
                )
                ok, msg = run_post_check(ctx)
                result.metrics = dict(ctx.metrics)
                result.post_check_message = msg
                # Probe-specific fail marker (e.g. "mapfail: FAIL").
                if spec.fail_marker:
                    fl = extract_fail_marker(ctx.serial_text, spec.fail_marker)
                    if fl is not None:
                        result.fail_line = fl
                        result.passed = False
                    else:
                        result.passed = ok
                else:
                    result.passed = ok
            elif outcome.result == WaitResult.TIMEOUT and not required_markers:
                # No markers required → timeout is the expected exit.
                result.passed = True
            else:
                result.passed = False
                result.error = outcome.result.value

            # 13. FB dump (if requested) before QEMU dies.
            if cfg.fb_dump_out is not None:
                qemu.capture_fb_dump(cfg.fb_dump_out)

            _dump_serial_tail(result, serial)
    except Exception as exc:  # noqa: BLE001
        result.passed = False
        result.error = f"{type(exc).__name__}: {exc}"
        log.exception("case %s raised", case.name)
    finally:
        qemu.cleanup()
        if http_server_proc is not None:
            http_server_proc.shutdown()
            http_server_proc.server_close()
        result.elapsed_s = time.monotonic() - start

    return result


# ---------------------------------------------------------------------- #
# Suite execution
# ---------------------------------------------------------------------- #

def run_suite(
    cases: list[Case],
    cfg: HarnessConfig | None = None,
    *,
    stop_on_fail: bool = False,
) -> SuiteResult:
    """Run a list of cases, return aggregate result."""
    cfg = cfg or HarnessConfig()
    suite = SuiteResult()
    suite_start = time.monotonic()
    for case in cases:
        suite.total += 1
        log.info("=== Harness case: %s ===", case.name)
        result = run_case(case, cfg)
        suite.cases.append(result)
        if result.passed:
            suite.passed += 1
            log.info("=== Harness case PASS: %s ===", case.name)
        else:
            suite.failed += 1
            log.warning("=== Harness case FAIL: %s ===", case.name)
            if stop_on_fail:
                break
    suite.duration_s = time.monotonic() - suite_start
    return suite


# ---------------------------------------------------------------------- #
# Helpers
# ---------------------------------------------------------------------- #

def _apply_case_overrides(cfg: HarnessConfig, case: Case) -> HarnessConfig:
    """Build a config copy with case-level overrides applied."""
    # Use object.__new__ to bypass __post_init__ validation, then copy.
    new = HarnessConfig.__new__(HarnessConfig)
    new.__dict__.update(cfg.__dict__)
    new.marker_mode = case.marker_mode
    if case.run_wait_s is not None:
        new.run_wait_s = case.run_wait_s
    if case.shell_ready_wait_s is not None:
        new.shell_ready_wait_s = case.shell_ready_wait_s
    if case.test_command is not None:
        new.test_command = case.test_command
    if case.post_sendkey is not None:
        new.post_sendkey = case.post_sendkey
    if case.expect_fault:
        new.expect_fault = True
    if case.keystroke_commands:
        new.keystroke_commands = [*new.keystroke_commands, *case.keystroke_commands]
    # Re-derive defaults from marker_mode if test_command is still __AUTO__.
    if new.test_command == "__AUTO__":
        defaults = get_defaults(case.marker_mode)
        new.test_command = defaults.test_command
        if case.sendkey_sequence_nowait is None:
            # Will be resolved via _derive_nowait below.
            pass
        if not case.sendkey_sequence and defaults.sendkey_sequence:
            # Stored on the case, not the config, so _resolve_sequence sees it.
            case.sendkey_sequence = list(defaults.sendkey_sequence)
        if case.run_wait_s is None and defaults.run_wait_s is not None:
            new.run_wait_s = defaults.run_wait_s
        if case.post_sendkey is None and defaults.post_sendkey:
            new.post_sendkey = defaults.post_sendkey
        if not case.keystroke_commands and defaults.keystroke_commands:
            new.keystroke_commands = [
                *new.keystroke_commands,
                *defaults.keystroke_commands,
            ]
    return new


def _derive_nowait(cfg: HarnessConfig, case: Case) -> bool:
    if case.sendkey_sequence_nowait is not None:
        return case.sendkey_sequence_nowait
    defaults = get_defaults(case.marker_mode)
    return defaults.sendkey_sequence_nowait


def _resolve_sequence(cfg: HarnessConfig, case: Case) -> list[str]:
    if case.sendkey_sequence:
        return case.sendkey_sequence
    return get_defaults(case.marker_mode).sendkey_sequence


def _typed_commands(cfg: HarnessConfig, case: Case) -> list[str]:
    cmds: list[str] = []
    cmds.extend(cfg.keystroke_commands)
    if case.test_command and case.test_command != "__AUTO__":
        for _ in range(case.test_command_repeat):
            cmds.append(case.test_command)
    elif cfg.test_command and cfg.test_command != "__AUTO__":
        for _ in range(case.test_command_repeat):
            cmds.append(cfg.test_command)
    return cmds


def _post_sendkey(cfg: HarnessConfig, case: Case) -> str | None:
    return case.post_sendkey or cfg.post_sendkey


def _type_commands(monitor, cmds: list[str], cfg: HarnessConfig) -> None:
    """Type each command via the HU sendkey translator."""
    from cluu_harness.monitor import QemuMonitor  # noqa: F401 (type hint)

    for i, cmd in enumerate(cmds):
        bad = unsupported_chars(cmd)
        if bad:
            log.warning("unsupported chars in %r: %s — skipping", cmd, bad)
            continue
        keys = command_to_sendkeys(cmd)
        log.info("sending command %d/%d: %r", i + 1, len(cmds), cmd)
        if cfg.fast_keystrokes:
            monitor.send_keys_batch(keys)
        else:
            for k in keys:
                monitor.send_key(k, delay_s=cfg.key_delay_s)
        if i + 1 < len(cmds):
            time.sleep(cfg.command_gap_s)


def _run_sendkey_sequence(monitor, sequence: list[str]) -> None:
    """Execute a raw SENDKEY_SEQUENCE (sendkey/mouse_move/sleep lines)."""
    for line in sequence:
        line = line.strip()
        if not line:
            continue
        if line.startswith("sendkey "):
            monitor.send_key(line.removeprefix("sendkey "))
        elif line.startswith("mouse_move "):
            args = line.removeprefix("mouse_move ").split()
            monitor.mouse_move(int(args[0]), int(args[1]))
        elif line.startswith("mouse_button "):
            monitor.mouse_button(int(line.removeprefix("mouse_button ")))
        elif line.startswith("sleep "):
            time.sleep(float(line.removeprefix("sleep ")))
        else:
            log.warning("unknown SENDKEY_SEQUENCE line: %s", line)


def _slo_limits(cfg: HarnessConfig) -> dict[str, int | None]:
    return {
        "MAX_DELTA_SPACES": cfg.max_delta_spaces,
        "MAX_DELTA_TOKENS": cfg.max_delta_tokens,
        "MAX_DELTA_ENDPOINTS": cfg.max_delta_endpoints,
        "MAX_DELTA_PMM_USED_FRAMES": cfg.max_delta_pmm_used_frames,
        "MAX_IPC_WAIT_P95_MS": cfg.max_ipc_wait_p95_ms,
        "MAX_IPC_WAIT_P99_MS": cfg.max_ipc_wait_p99_ms,
        "MAX_IPC_SCAN_AVG_STEPS_X100": cfg.max_ipc_scan_avg_steps_x100,
        "MAX_IPC_QUEUE_BYTES_PEAK": cfg.max_ipc_queue_bytes_peak,
        "MAX_IPC_QUEUE_MESSAGES_PEAK": cfg.max_ipc_queue_messages_peak,
        "MIN_NOOP_SPAWN_SAMPLES": cfg.min_noop_spawn_samples,
        "MIN_NOOP_MAP_ELF_SAMPLES": cfg.min_noop_map_elf_samples,
        "MAX_NOOP_SPAWN_REPLY_P95_CYCLES": cfg.max_noop_spawn_reply_p95_cycles,
        "MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES": cfg.max_noop_map_elf_reply_p95_cycles,
    }


def _dump_serial_tail(result: CaseResult, serial: SerialStream) -> None:
    """Emit the last 200 serial lines for diagnostics on failure."""
    if not result.passed:
        log.info("----- serial tail (last 200 lines) -----\n%s", serial.tail(200))
        log.info("----------------------------------------")


__all__ = ["CaseResult", "SuiteResult", "run_case", "run_suite"]
