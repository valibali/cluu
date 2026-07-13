"""Configuration: paths and env-var defaults.

Ported from the env-defaults block at the top of ``scripts/harness_run.sh``.
All values are overridable via environment variables to keep the bash
harness's ``KEY=VALUE ./harness_run.sh`` ergonomics.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path


def _env_path(name: str, default: Path) -> Path:
    raw = os.environ.get(name)
    return Path(raw) if raw else default


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    try:
        return int(raw)
    except ValueError as exc:
        raise ValueError(f"{name} must be an integer, got {raw!r}") from exc


def _env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    try:
        return float(raw)
    except ValueError as exc:
        raise ValueError(f"{name} must be a number, got {raw!r}") from exc


def _env_str(name: str, default: str) -> str:
    return os.environ.get(name, default)


def _env_bool(name: str, default: bool = False) -> bool:
    raw = os.environ.get(name)
    if raw is None:
        return default
    return raw == "1"


def _opt_int(name: str) -> int | None:
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return None
    try:
        return int(raw)
    except ValueError as exc:
        raise ValueError(f"{name} must be an integer, got {raw!r}") from exc


def _load_keystroke_commands() -> list[str]:
    """Load KEYSTROKE_COMMANDS and KEYSTROKE_COMMANDS_FILE into a flat list.

    Blank lines and ``#`` comments are stripped, mirroring
    ``append_keystroke_command`` in the bash harness.
    """
    cmds: list[str] = []
    file_path = os.environ.get("KEYSTROKE_COMMANDS_FILE")
    if file_path:
        path = Path(file_path)
        if not path.is_file():
            raise FileNotFoundError(f"KEYSTROKE_COMMANDS_FILE not found: {path}")
        for line in path.read_text(encoding="utf-8").splitlines():
            line = line.rstrip("\r")
            if not line or line.startswith("#"):
                continue
            cmds.append(line)
    inline = os.environ.get("KEYSTROKE_COMMANDS")
    if inline:
        for line in inline.splitlines():
            line = line.rstrip("\r")
            if not line or line.startswith("#"):
                continue
            cmds.append(line)
    return cmds


@dataclass
class GdbConfig:
    """GDB attach behaviour (mirrors ``HARNESS_GDB_*`` env vars)."""

    enabled: bool = field(default_factory=lambda: _env_bool("QEMU_GDB"))
    # When True, start QEMU with `-s` only (no pause) — useful for
    # attaching a debugger to an already-running guest.
    server_only: bool = field(default_factory=lambda: _env_bool("QEMU_GDB_SERVER"))
    mode: str = field(default_factory=lambda: _env_str("HARNESS_GDB_MODE", "manual"))
    binary: str = field(default_factory=lambda: _env_str("HARNESS_GDB_BIN", "gdb"))
    target: str = field(default_factory=lambda: _env_str("HARNESS_GDB_TARGET", "localhost:1234"))
    timeout_s: int = field(default_factory=lambda: _env_int("HARNESS_GDB_TIMEOUT", 20))
    manual_timeout_s: int = field(
        default_factory=lambda: _env_int("HARNESS_GDB_MANUAL_TIMEOUT", 120)
    )
    script: str | None = field(default_factory=lambda: os.environ.get("HARNESS_GDB_SCRIPT"))
    symbol: str | None = field(default_factory=lambda: os.environ.get("HARNESS_GDB_SYMBOL"))
    batch: bool = field(default_factory=lambda: _env_bool("HARNESS_GDB_BATCH", True))

    VALID_MODES = frozenset({"manual", "auto-continue", "script"})

    def __post_init__(self) -> None:
        if self.mode not in self.VALID_MODES:
            raise ValueError(
                f"HARNESS_GDB_MODE={self.mode!r} unsupported "
                f"(must be one of {sorted(self.VALID_MODES)})"
            )
        if self.mode == "script" and not self.script:
            raise ValueError("HARNESS_GDB_MODE=script requires HARNESS_GDB_SCRIPT")
        if self.enabled and self.mode == "script" and self.script:
            path = Path(self.script)
            if not path.is_file():
                raise FileNotFoundError(f"HARNESS_GDB_SCRIPT not found: {path}")


@dataclass
class HarnessConfig:
    """All harness knobs. Fields map 1:1 to the bash env vars."""

    project_root: Path = field(default_factory=lambda: Path(__file__).resolve().parents[2])

    # Paths
    serial_log: Path = field(
        default_factory=lambda: _env_path("SERIAL_LOG", Path("/tmp/cluu-serial-com2.log"))
    )
    monitor_sock: Path = field(
        default_factory=lambda: _env_path("MONITOR_SOCK", Path("/tmp/cluu-qemu-monitor.sock"))
    )
    ovmf: Path = field(
        default_factory=lambda: _env_path("OVMF", Path("/usr/share/ovmf/OVMF.fd"))
    )
    img: Path = field(default_factory=lambda: _env_path("IMG", Path("target/cluu.img")))
    user_disk: Path = field(
        default_factory=lambda: _env_path("USER_DISK", Path("target/userdisk.img"))
    )
    fb_dump_out: Path | None = field(
        default_factory=lambda: _env_path("FB_DUMP_OUT", Path("")) or None
        if os.environ.get("FB_DUMP_OUT")
        else None
    )

    # Build
    no_build: bool = False
    force_build: bool = field(default_factory=lambda: _env_bool("HARNESS_FORCE_BUILD"))
    clean_rebuild: bool = field(default_factory=lambda: _env_bool("HARNESS_CLEAN_REBUILD"))

    # Boot / shell timing (safety bounds, NOT pass criterion)
    boot_wait_s: int = field(default_factory=lambda: _env_int("BOOT_WAIT", 0))
    shell_ready_wait_s: int = field(default_factory=lambda: _env_int("SHELL_READY_WAIT", 60))
    shell_ready_wait_max_s: int = field(
        default_factory=lambda: _env_int("SHELL_READY_WAIT_MAX", 90)
    )
    allow_slow_shell_wait: bool = field(
        default_factory=lambda: _env_bool("ALLOW_SLOW_SHELL_WAIT")
    )
    run_wait_s: int = field(default_factory=lambda: _env_int("RUN_WAIT", 12))

    # Keystroke injection
    key_delay_s: float = field(default_factory=lambda: _env_float("KEY_DELAY", 0.05))
    command_gap_s: float = field(default_factory=lambda: _env_float("COMMAND_GAP", 1.0))
    post_sendkey: str | None = field(default_factory=lambda: os.environ.get("POST_SENDKEY"))
    post_sendkey_delay_s: float = field(
        default_factory=lambda: _env_float("POST_SENDKEY_DELAY", 1.0)
    )
    fast_keystrokes: bool = field(default_factory=lambda: _env_bool("FAST_KEYSTROKES"))

    # Marker / fault policy
    marker_mode: str = field(default_factory=lambda: _env_str("MARKER_MODE", "legacy_p1"))
    required_markers_override: list[str] | None = field(
        default_factory=lambda: (
            os.environ["REQUIRED_MARKERS"].splitlines()
            if os.environ.get("REQUIRED_MARKERS")
            else None
        )
    )
    expect_fault: bool = field(default_factory=lambda: _env_bool("EXPECT_FAULT"))

    # Fault / fail patterns (regex, case-insensitive for faults)
    fault_pattern: str = r"PAGE_FAULT|GENERAL_PROTECTION|DOUBLE_FAULT|INVALID_OPCODE"
    fail_pattern: str = r"\[FAIL\]|test FAILED|PANIC|panic"

    # SLO knobs (None means "skip check")
    min_exit_cookies: int = field(default_factory=lambda: _env_int("MIN_EXIT_COOKIES", 3))
    max_delta_spaces: int | None = field(
        default_factory=lambda: _opt_int("MAX_DELTA_SPACES")
    )
    max_delta_tokens: int | None = field(
        default_factory=lambda: _opt_int("MAX_DELTA_TOKENS")
    )
    max_delta_endpoints: int | None = field(
        default_factory=lambda: _opt_int("MAX_DELTA_ENDPOINTS")
    )
    max_delta_pmm_used_frames: int | None = field(
        default_factory=lambda: _opt_int("MAX_DELTA_PMM_USED_FRAMES")
    )
    max_ipc_wait_p95_ms: int | None = field(
        default_factory=lambda: _opt_int("MAX_IPC_WAIT_P95_MS")
    )
    max_ipc_wait_p99_ms: int | None = field(
        default_factory=lambda: _opt_int("MAX_IPC_WAIT_P99_MS")
    )
    max_ipc_scan_avg_steps_x100: int | None = field(
        default_factory=lambda: _opt_int("MAX_IPC_SCAN_AVG_STEPS_X100")
    )
    max_ipc_queue_bytes_peak: int | None = field(
        default_factory=lambda: _opt_int("MAX_IPC_QUEUE_BYTES_PEAK")
    )
    max_ipc_queue_messages_peak: int | None = field(
        default_factory=lambda: _opt_int("MAX_IPC_QUEUE_MESSAGES_PEAK")
    )
    min_noop_spawn_samples: int = field(
        default_factory=lambda: _env_int("MIN_NOOP_SPAWN_SAMPLES", 8)
    )
    min_noop_map_elf_samples: int = field(
        default_factory=lambda: _env_int("MIN_NOOP_MAP_ELF_SAMPLES", 8)
    )
    max_noop_spawn_reply_p95_cycles: int | None = field(
        default_factory=lambda: _opt_int("MAX_NOOP_SPAWN_REPLY_P95_CYCLES")
    )
    max_noop_map_elf_reply_p95_cycles: int | None = field(
        default_factory=lambda: _opt_int("MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES")
    )
    max_fb_blit_wc_cycles: int | None = field(
        default_factory=lambda: _opt_int("MAX_FB_BLIT_WC_CYCLES")
    )

    # QEMU extras
    qemu_extra_args: str = field(default_factory=lambda: _env_str("QEMU_EXTRA_ARGS", ""))
    qemu_gdb: GdbConfig = field(default_factory=GdbConfig)

    # Virtio-net NIC: when True, QEMU gets -netdev user + virtio-net-pci
    # (vectors=0 = legacy INTX, matching virtio-blk).
    cluu_net: bool = field(default_factory=lambda: _env_bool("CLUU_NET"))

    # Test command (None = unset; "__AUTO__" = derive from marker_mode)
    test_command: str | None = field(
        default_factory=lambda: os.environ.get("TEST_COMMAND", "__AUTO__")
    )
    test_command_repeat: int = field(
        default_factory=lambda: _env_int("TEST_COMMAND_REPEAT", 1)
    )
    keystroke_commands: list[str] = field(default_factory=_load_keystroke_commands)

    # Autoexec (build-time procmgr shell autostart override)
    autoexec_cmd: str | None = field(
        default_factory=lambda: os.environ.get("HARNESS_AUTOEXEC_CMD")
        or os.environ.get("HARNESS_AUTOSTART_CMD")
    )

    def __post_init__(self) -> None:
        if self.shell_ready_wait_s < 1:
            raise ValueError("SHELL_READY_WAIT must be a positive integer")
        if self.shell_ready_wait_max_s < 1:
            raise ValueError("SHELL_READY_WAIT_MAX must be a positive integer")
        if (
            not self.allow_slow_shell_wait
            and self.shell_ready_wait_s > self.shell_ready_wait_max_s
        ):
            raise ValueError(
                f"SHELL_READY_WAIT={self.shell_ready_wait_s}s exceeds policy max "
                f"{self.shell_ready_wait_max_s}s "
                "(set ALLOW_SLOW_SHELL_WAIT=1 for explicit debug sessions)"
            )
        if self.test_command_repeat < 1:
            raise ValueError("TEST_COMMAND_REPEAT must be a positive integer")
        # Resolve relative image/disk paths against project root.
        self.img = self._resolve(self.img)
        self.user_disk = self._resolve(self.user_disk)

    def _resolve(self, p: Path) -> Path:
        return p if p.is_absolute() else (self.project_root / p)


__all__ = ["GdbConfig", "HarnessConfig"]
