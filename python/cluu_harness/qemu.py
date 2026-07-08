"""QEMU lifecycle: build, launch, monitor, teardown.

Ports the QEMU invocation, build gating, and cleanup logic from
``scripts/harness_run.sh``. The gen2 differences:

* Serial output is tailed by :class:`cluu_harness.serial_stream.SerialStream`
  in a background thread; we do NOT ``grep`` the file.
* Keystroke injection uses the persistent :class:`QemuMonitor` socket.
* Build gating checks ``find ... -newer $IMG`` in Python.
* FB dump (``pmemsave``) happens in cleanup before QEMU is killed.
"""

from __future__ import annotations

import contextlib
import logging
import os
import re
import shutil
import signal
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

from cluu_harness.config import HarnessConfig
from cluu_harness.monitor import QemuMonitor

log = logging.getLogger(__name__)


_FB_PHYS_RE = re.compile(r"fb @([0-9A-Fa-f]+)")


@dataclass
class QemuRun:
    """A live QEMU process plus its monitor handle."""

    process: subprocess.Popen[bytes]
    monitor: QemuMonitor
    started_monotonic: float


class QemuController:
    """Owns the QEMU process, monitor socket, and build gating."""

    def __init__(self, cfg: HarnessConfig) -> None:
        self.cfg = cfg
        self._run: QemuRun | None = None

    # ------------------------------------------------------------------ #
    # Build
    # ------------------------------------------------------------------ #
    def build(self, *, force: bool = False) -> float:
        """Run ``cargo xtask build`` (with toolchain prep if missing).

        Returns elapsed seconds. Skips the build if the image is newer
        than all sources, mirroring the bash harness's find-based gate.
        """
        if self.cfg.no_build and not force:
            log.info("build skipped (--no-build)")
            return 0.0

        if not force and not self.cfg.force_build and self._image_is_fresh():
            log.info("build skipped (image newer than all sources); "
                     "set HARNESS_FORCE_BUILD=1 to override")
            return 0.0

        start = time.monotonic()
        root = self.cfg.project_root
        if self.cfg.clean_rebuild:
            log.info("clean rebuild of CLUU")
            shutil.rmtree(root / "target" / "newlib-build", ignore_errors=True)
            shutil.rmtree(root / "target" / "sysroot" / "x86_64-cluu-elf", ignore_errors=True)
            self._run_cmd(["make", "clean"], cwd=root)
            self._run_cmd(["cargo", "xtask", "build-newlib"], cwd=root)
            self._run_cmd(["cargo", "xtask", "build-syscalls"], cwd=root)
            self._run_cmd(["cargo", "xtask", "build-crt0"], cwd=root)
        else:
            log.info("incremental full build of CLUU")
            self._ensure_toolchain_prereqs()
            # Always rebuild syscalls/crt0 to pick up libcluu changes.
            self._run_cmd(["cargo", "xtask", "build-syscalls"], cwd=root)
            self._run_cmd(["cargo", "xtask", "build-crt0"], cwd=root)
        self._run_cmd(["cargo", "xtask", "build"], cwd=root)
        elapsed = time.monotonic() - start
        log.info("build complete (%.1fs)", elapsed)
        return elapsed

    def _image_is_fresh(self) -> bool:
        img = self.cfg.img
        if not img.exists():
            return False
        # Mirror the bash harness's find-newer gate.
        for root_dir in ("kernel", "userspace", "scripts", "xtask", "Cargo.toml", "Cargo.lock"):
            path = self.cfg.project_root / root_dir
            if self._has_newer(path, img):
                return False
        return True

    @staticmethod
    def _has_newer(root: Path, reference: Path) -> bool:
        if not root.exists():
            return False
        if root.is_file():
            return root.stat().st_mtime > reference.stat().st_mtime
        for dirpath, _dirs, files in os.walk(root):
            # Skip target/ and .git/ subtrees.
            if "target" in Path(dirpath).parts or ".git" in Path(dirpath).parts:
                continue
            for f in files:
                p = Path(dirpath) / f
                try:
                    if p.stat().st_mtime > reference.stat().st_mtime:
                        return True
                except OSError:
                    continue
        return False

    def _ensure_toolchain_prereqs(self) -> None:
        root = self.cfg.project_root
        needs_prep = not all(
            (root / "target" / "sysroot" / "lib" / name).exists()
            for name in ("libcluu_syscalls.a", "crt0.o")
        ) or not (root / "target" / "sysroot" / "x86_64-cluu-elf" / "lib" / "libc.a").exists()
        if needs_prep:
            log.info("preparing toolchain/sysroot artifacts")
            self._run_cmd(["cargo", "xtask", "build-newlib"], cwd=root)
            self._run_cmd(["cargo", "xtask", "build-syscalls"], cwd=root)
            self._run_cmd(["cargo", "xtask", "build-crt0"], cwd=root)

    @staticmethod
    def _run_cmd(cmd: list[str], cwd: Path) -> None:
        log.debug("running %s (cwd=%s)", " ".join(cmd), cwd)
        result = subprocess.run(cmd, cwd=cwd, check=False)  # noqa: S603
        if result.returncode != 0:
            raise RuntimeError(
                f"command {' '.join(cmd)!r} exited with {result.returncode}"
            )

    # ------------------------------------------------------------------ #
    # Launch
    # ------------------------------------------------------------------ #
    def launch(self) -> QemuRun:
        """Start QEMU headless and connect to the monitor socket."""
        cfg = self.cfg
        if not cfg.img.exists():
            raise RuntimeError(f"{cfg.img} not found — build failed?")

        # Clear old logs / sockets.
        with contextlib.suppress(OSError):
            cfg.serial_log.write_bytes(b"")
        cfg.monitor_sock.unlink(missing_ok=True)

        args = self._qemu_args()
        log.info("starting QEMU (headless)")
        log.debug("qemu args: %s", " ".join(args))
        proc = subprocess.Popen(  # noqa: S603
            args,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        # Give QEMU a moment to create the monitor socket.
        time.sleep(2.0)
        if proc.poll() is not None:
            tail = cfg.serial_log.read_text(encoding="utf-8", errors="replace")
            raise RuntimeError(f"QEMU exited prematurely. Serial tail:\n{tail}")

        monitor = QemuMonitor(cfg.monitor_sock, connect_timeout_s=5.0)
        monitor.connect()
        self._run = QemuRun(
            process=proc, monitor=monitor, started_monotonic=time.monotonic()
        )
        return self._run

    def _qemu_args(self) -> list[str]:
        cfg = self.cfg
        args = [
            "qemu-system-x86_64",
            "-bios", str(cfg.ovmf),
            "-machine", "q35",
            "-m", "1G",
            "-accel", os.environ.get("QEMU_ACCEL", "kvm"),
            "-cpu", os.environ.get("QEMU_CPU", "host"),
            "-drive", f"file={cfg.img},format=raw,if=ide,index=0",
            "-drive", f"file={cfg.user_disk},format=raw,if=none,id=userblk",
            "-device",
            "virtio-blk-pci,drive=userblk,disable-legacy=on,"
            "disable-modern=off,vectors=0",
            "-display", "none",
            "-no-reboot",
            "-no-shutdown",
            "-serial", "null",
            "-serial", f"file:{cfg.serial_log}",
            "-monitor", f"unix:{cfg.monitor_sock},server,nowait",
        ]
        if cfg.qemu_gdb.enabled and not cfg.qemu_gdb.server_only:
            log.info("QEMU_GDB=1: enabling -S -s (wait for GDB on tcp:1234)")
            args.extend(["-S", "-s"])
        elif cfg.qemu_gdb.server_only:
            log.info("QEMU_GDB_SERVER=1: enabling -s (GDB server, no pause)")
            args.append("-s")
        if cfg.qemu_extra_args:
            args.extend(cfg.qemu_extra_args.split())
        if cfg.autoexec_cmd:
            # Forwarded to the build via env; xtask reads it. We just
            # pass it through the environment of the build step.
            os.environ["HARNESS_AUTOEXEC_CMD"] = cfg.autoexec_cmd
        return args

    # ------------------------------------------------------------------ #
    # Lifecycle helpers
    # ------------------------------------------------------------------ #
    @property
    def is_alive(self) -> bool:
        return self._run is not None and self._run.process.poll() is None

    @property
    def monitor(self) -> QemuMonitor:
        if self._run is None:
            raise RuntimeError("QEMU not launched")
        return self._run.monitor

    def kill(self) -> None:
        """Terminate QEMU gracefully (SIGTERM, then SIGKILL)."""
        if self._run is None:
            return
        proc = self._run.process
        if proc.poll() is None:
            log.info("killing QEMU (pid %d)", proc.pid)
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
            except ProcessLookupError:
                return
            try:
                proc.wait(timeout=5.0)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5.0)

    # ------------------------------------------------------------------ #
    # FB dump (pmemsave before kill)
    # ------------------------------------------------------------------ #
    def capture_fb_dump(self, out_path: Path) -> bool:
        """If the serial log mentions a framebuffer phys addr, dump it."""
        if self._run is None or not self._run.monitor:
            return False
        text = self.cfg.serial_log.read_text(encoding="utf-8", errors="replace")
        m = _FB_PHYS_RE.search(text)
        if m is None:
            log.warning("FB_DUMP: SKIP — fb_phys not found in %s", self.cfg.serial_log)
            return False
        phys = int(m.group(1), 16)
        log.info("FB_DUMP: capturing phys=0x%x -> %s", phys, out_path)
        # 1280x720 BGRA32 = 3686400 bytes (per knowledge note).
        self._run.monitor.pmemsave(phys, 3686400, out_path)
        return True

    # ------------------------------------------------------------------ #
    # Cleanup
    # ------------------------------------------------------------------ #
    def cleanup(self, fb_dump_out: Path | None = None) -> None:
        if fb_dump_out is not None and self.is_alive:
            try:
                self.capture_fb_dump(fb_dump_out)
            except Exception:  # noqa: BLE001
                log.exception("FB dump failed (continuing)")
        if self._run is not None:
            try:
                self._run.monitor.quit()
            except Exception:  # noqa: BLE001
                self._run.monitor.close()
        self.kill()
        self.cfg.monitor_sock.unlink(missing_ok=True)
        self._run = None

    def __enter__(self) -> QemuController:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.cleanup()


__all__ = ["QemuController", "QemuRun"]
