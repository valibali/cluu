"""GDB attach modes.

Ports ``run_gdb_control`` from ``scripts/harness_run.sh``. Three modes:

* ``manual`` — print attach instructions and wait for the user to resume
  the target (we detect serial activity as the resume signal).
* ``auto-continue`` — attach, detach, quit. The target resumes execution
  on detach.
* ``script`` — run a GDB script file against the paused target.
"""

from __future__ import annotations

import logging
import shutil
import socket
import subprocess
import time
from pathlib import Path

from cluu_harness.config import GdbConfig
from cluu_harness.serial_stream import SerialStream

log = logging.getLogger(__name__)


class GdbAttachError(RuntimeError):
    """Raised on GDB attach / script failures."""


def wait_for_tcp_port(target: str, timeout_s: int) -> bool:
    """Poll ``host:port`` until it accepts a connection or timeout."""
    host, _, port_str = target.partition(":")
    try:
        port = int(port_str)
    except ValueError as exc:
        raise ValueError(f"invalid GDB target {target!r}") from exc
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.settimeout(1.0)
            try:
                s.connect((host, port))
                return True
            except OSError:
                time.sleep(1.0)
    return False


def run_gdb_control(cfg: GdbConfig, serial: SerialStream) -> None:
    """Run the GDB attach sequence (no-op if GDB is disabled)."""
    if not cfg.enabled:
        return
    if shutil.which(cfg.binary) is None:
        raise GdbAttachError(f"HARNESS_GDB_BIN not found: {cfg.binary}")
    if not wait_for_tcp_port(cfg.target, cfg.timeout_s):
        raise GdbAttachError(
            f"GDB stub {cfg.target} not reachable within {cfg.timeout_s}s"
        )

    if cfg.mode == "manual":
        _manual(cfg, serial)
    elif cfg.mode == "auto-continue":
        _auto_continue(cfg)
    elif cfg.mode == "script":
        _script(cfg)
    else:  # pragma: no cover — validated in GdbConfig.__post_init__
        raise GdbAttachError(f"unsupported HARNESS_GDB_MODE {cfg.mode!r}")


def _manual(cfg: GdbConfig, serial: SerialStream) -> None:
    print(f"QEMU is paused for GDB at {cfg.target}.")
    print("Attach manually and resume execution (example):")
    symbol_arg = f"{cfg.symbol} " if cfg.symbol else ""
    print(
        f"  {cfg.binary} -q {symbol_arg}"
        f"-ex 'target remote {cfg.target}'"
    )
    print(
        f"Waiting up to {cfg.manual_timeout_s}s for serial activity "
        "after resume..."
    )
    if not serial.wait_for_serial_activity(cfg.manual_timeout_s):
        raise GdbAttachError(
            "no serial activity observed after waiting for manual GDB resume"
        )


def _auto_continue(cfg: GdbConfig) -> None:
    log.info("auto-resuming paused QEMU via GDB detach (%s)", cfg.target)
    cmd: list[str] = [cfg.binary, "-q", "--batch", "-ex", "set pagination off"]
    if cfg.symbol:
        cmd.append(cfg.symbol)
    cmd.extend(
        [
            "-ex", f"target remote {cfg.target}",
            "-ex", "detach",
            "-ex", "quit",
        ]
    )
    _run_gdb(cmd)


def _script(cfg: GdbConfig) -> None:
    assert cfg.script is not None  # validated in GdbConfig
    script_path = Path(cfg.script)
    log.info("running GDB script: %s", script_path)
    cmd: list[str] = [cfg.binary, "-q", "-ex", "set pagination off"]
    if cfg.batch:
        cmd.append("--batch")
    if cfg.symbol:
        cmd.append(cfg.symbol)
    cmd.extend(
        [
            "-ex", f"target remote {cfg.target}",
            "-x", str(script_path),
        ]
    )
    _run_gdb(cmd)


def _run_gdb(cmd: list[str]) -> None:
    log.debug("gdb cmd: %s", " ".join(cmd))
    result = subprocess.run(cmd, check=False)  # noqa: S603
    if result.returncode != 0:
        raise GdbAttachError(
            f"GDB exited with {result.returncode}: {' '.join(cmd)}"
        )


__all__ = ["GdbAttachError", "run_gdb_control", "wait_for_tcp_port"]
