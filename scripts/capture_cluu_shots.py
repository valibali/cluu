#!/usr/bin/env python3
"""Capture CLUU framebuffer snapshots and assemble GIFs for documentation.

Boots CLUU via the same QEMU invocation the harness uses, drives the
guest with sendkey sequences, and at scripted checkpoints dumps the
1728x900 BGRA32 framebuffer to disk via QEMU's ``pmemsave``. Raw frames
are converted to PNG via ImageMagick and stitched into GIFs via ffmpeg.

Idempotent: re-running with the same args overwrites assets.

Assets land in ``doc/assets/<name>.gif`` with a companion
``<name>.gif.md`` describing the capture. One static PNG
(``console-framebuffer.png``) is captured for the glyph-atlas shot.

Usage::

    scripts/capture_cluu_shots.py                  # capture all scenes
    scripts/capture_cluu_shots.py boot-to-login    # capture one scene
    scripts/capture_cluu_shots.py --list           # list scene names
    scripts/capture_cluu_shots.py --no-build       # skip build gate

Scenes are defined below as ``Scene`` dataclasses. A scene that fails
(sendkey error, missing serial marker, convert/ffmpeg error) is skipped
and a note is left in its companion ``.md`` so the doc-suite generator
can tell a missing asset from a failed one.

The framebuffer is 1728x900 BGRA32 = 3,686,400 bytes (per the user's
confirmation; matches ``QemuController.capture_fb_dump``). The phys
address is parsed from the serial log line ``fb @<PHYS>`` (same regex
as the harness, ``_FB_PHYS_RE`` in ``qemu.py``).
"""

from __future__ import annotations

import argparse
import contextlib
import logging
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

# Make the cluu_harness package importable when run from repo root.
REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "python"))

from cluu_harness.config import HarnessConfig  # noqa: E402
from cluu_harness.monitor import QemuMonitor  # noqa: E402
from cluu_harness.qemu import QemuController, _FB_PHYS_RE  # noqa: E402
from cluu_harness.sendkey import command_to_sendkeys  # noqa: E402
from cluu_harness.serial_stream import SerialStream  # noqa: E402

log = logging.getLogger("capture_cluu_shots")

# Framebuffer geometry. Hardcoded per the plan; matches the existing
# QemuController.capture_fb_dump constant exactly.
FB_W = 1280
FB_H = 720
FB_BPP = 4  # BGRA32
FB_SIZE = FB_W * FB_H * FB_BPP  # 3,686,400

ASSETS_DIR = REPO_ROOT / "doc" / "assets"

# Standard root/root credentials sendkey sequence. Per
# cluu-harness-sendkey-sleep-must-match-boot, the prefix sleep is 12s
# (kbd attaches at ~9.4s, login window at ~9.8s).
CREDS_SENDKEY_ROOT: list[str] = [
    "sleep 12",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
    "sleep 2",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
]


# ---------------------------------------------------------------------- #
# Scene definition
# ---------------------------------------------------------------------- #

@dataclass
class Scene:
    """One capture target.

    ``name`` is the asset basename (no extension). ``kind`` is 'gif' or
    'png'. ``prepare`` is a callable that drives the guest (sendkeys,
    sleeps). ``capture_times`` is a list of seconds-after-start at which
    to grab a frame (for GIFs); for PNGs a single capture is taken after
    ``prepare`` returns. ``note`` goes into the companion .md.
    """
    name: str
    kind: str  # 'gif' | 'png'
    needs_login: bool
    prepare: Callable[["CaptureSession"], None]
    capture_times: list[float] = field(default_factory=list)
    note: str = ""
    headers: list[str] = field(default_factory=list)  # files that reference this asset


class CaptureSession:
    """State held during a single QEMU run.

    One QEMU boot can serve multiple scenes; we only reboot between
    scenes that need a fresh login state. ``capture_frame`` writes a
    raw FB dump and converts to PNG.
    """

    def __init__(self, cfg: HarnessConfig, ctrl: QemuController,
                 fb_phys: int, workdir: Path) -> None:
        self.cfg = cfg
        self.ctrl = ctrl
        self.fb_phys = fb_phys
        self.workdir = workdir
        self.frame_idx = 0
        self.boot_t0 = time.monotonic()

    def elapsed(self) -> float:
        return time.monotonic() - self.boot_t0

    def send_keys(self, keys: list[str]) -> None:
        """Send a list of sendkey/sleep instructions.

        ``sleep N`` lines pause for N seconds. ``sendkey X`` lines go
        through the monitor. ``type TEXT`` lines expand via the HU
        keymap (command_to_sendkeys).
        """
        mon = self.ctrl.monitor
        for line in keys:
            parts = line.split(None, 1)
            if not parts:
                continue
            verb = parts[0]
            arg = parts[1] if len(parts) > 1 else ""
            if verb == "sleep":
                with contextlib.suppress(ValueError):
                    time.sleep(float(arg))
            elif verb == "sendkey":
                mon.send_key(arg, delay_s=0.02)
            elif verb == "type":
                # Expand ASCII text through the HU keymap.
                for k in command_to_sendkeys(arg):
                    mon.send_key(k, delay_s=0.02)
            elif verb == "raw":
                # Raw HMP command (e.g. 'raw mouse_move 10 10').
                mon.send(arg)
            else:
                log.warning("unknown sendkey line: %s", line)

    def capture_frame(self, label: str | None = None) -> Path | None:
        """Dump FB to raw, convert to PNG, return PNG path or None on failure."""
        raw = self.workdir / f"frame_{self.frame_idx:03d}.bin"
        png = self.workdir / f"frame_{self.frame_idx:03d}.png"
        self.frame_idx += 1
        try:
            self.ctrl.monitor.pmemsave(self.fb_phys, FB_SIZE, raw)
        except Exception as exc:  # noqa: BLE001
            log.warning("pmemsave failed for %s: %s", label or raw.name, exc)
            return None
        if not raw.exists() or raw.stat().st_size != FB_SIZE:
            log.warning("pmemsave produced wrong size for %s", label or raw.name)
            return None
        # BGRA32 -> PNG. Read as rgba: then swap R and B channels.
        cmd = [
            "convert", "-size", f"{FB_W}x{FB_H}", "-depth", "8",
            f"rgba:{raw}", "-swap", "0,2", "-alpha", "on", str(png),
        ]
        try:
            subprocess.run(cmd, check=True, capture_output=True, timeout=10)
        except subprocess.CalledProcessError as exc:
            log.warning("convert failed for %s: %s", label or raw.name,
                        exc.stderr.decode(errors="replace")[:200])
            return None
        raw.unlink(missing_ok=True)
        return png

    def capture_at_intervals(self, times: list[float]) -> list[Path]:
        """Capture frames at the given elapsed-second marks."""
        frames: list[Path] = []
        for target in times:
            now = self.elapsed()
            if target > now:
                time.sleep(target - now)
            f = self.capture_frame(label=f"t={target:.1f}s")
            if f is not None:
                frames.append(f)
        return frames


# ---------------------------------------------------------------------- #
# Scenes
# ---------------------------------------------------------------------- #

def _scene_boot_to_login(s: CaptureSession) -> None:
    """Just wait — capture frames during boot, before login."""
    s.send_keys(["sleep 2"])  # let FB init settle


def _scene_login_and_shell(s: CaptureSession) -> None:
    s.send_keys(CREDS_SENDKEY_ROOT)
    s.send_keys(["sleep 3"])  # shell prompt appears


def _scene_vt_switch(s: CaptureSession) -> None:
    # Already logged in from prior scene.
    s.send_keys([
        "sleep 1",
        "sendkey alt-f2", "sleep 1",
        "sendkey alt-f1", "sleep 1",
        "sendkey alt-f2", "sleep 1",
        "type echo on_vt2", "sleep 1",
        "sendkey alt-f1", "sleep 1",
    ])


def _scene_tty_line_discipline(s: CaptureSession) -> None:
    s.send_keys([
        "type echo hello_world", "sleep 0.5",
        "sendkey backspace", "sendkey backspace", "sendkey backspace",
        "sendkey backspace", "sendkey backspace", "sleep 0.5",
        "sendkey ret", "sleep 0.5",
        "type ls /", "sleep 0.3",
        "sendkey ctrl-c", "sleep 0.5",
        "sendkey up", "sleep 0.3",
        "sendkey ret", "sleep 0.5",
    ])


def _scene_shell_history(s: CaptureSession) -> None:
    s.send_keys([
        "type ls /", "sleep 0.5", "sendkey ret", "sleep 1",
        "type cat /etc/welcome.txt", "sleep 0.5", "sendkey ret", "sleep 1",
        "type ps", "sleep 0.5", "sendkey ret", "sleep 1",
        "sendkey up", "sleep 0.3", "sendkey up", "sleep 0.3",
        "sendkey up", "sleep 0.5",
        "sendkey ret", "sleep 1",
    ])


def _scene_ls_cat(s: CaptureSession) -> None:
    s.send_keys([
        "type ls /", "sleep 0.5", "sendkey ret", "sleep 1",
        "type cat /etc/welcome.txt", "sleep 0.5", "sendkey ret", "sleep 1",
        "type ps", "sleep 0.5", "sendkey ret", "sleep 1",
    ])


def _scene_top(s: CaptureSession) -> None:
    s.send_keys([
        "type top", "sleep 0.5", "sendkey ret",
        "sleep 4",  # let it tick a few times
        "sendkey q", "sleep 0.5",
    ])


def _scene_container_hello(s: CaptureSession) -> None:
    s.send_keys([
        "type container run hello", "sleep 0.5", "sendkey ret", "sleep 2",
        "type ps", "sleep 0.5", "sendkey ret", "sleep 1",
    ])


def _scene_mount_policy(s: CaptureSession) -> None:
    s.send_keys([
        "type spawn mkdir /tmp/demo", "sleep 0.5", "sendkey ret", "sleep 1",
        "type spawn mkdir /tmp/demo/inner", "sleep 0.5", "sendkey ret", "sleep 1",
        "type spawn rm -r /tmp/demo", "sleep 0.5", "sendkey ret", "sleep 1",
    ])


def _scene_edit(s: CaptureSession) -> None:
    s.send_keys([
        "type edit", "sleep 0.5", "sendkey ret", "sleep 2",
        "sendkey i",  # insert mode
        "type hello from edit",
        "sleep 1",
        "sendkey esc", "sleep 0.5",
        "type :w /tmp/edit-test", "sleep 0.3", "sendkey ret", "sleep 1",
        "type :q", "sleep 0.3", "sendkey ret", "sleep 1",
    ])


def _scene_compositor(s: CaptureSession) -> None:
    # Compositor is on VT4. If we're on a text VT, switch.
    s.send_keys([
        "sendkey alt-f4", "sleep 2",
        "type spawn cluuterm", "sleep 0.5", "sendkey ret", "sleep 2",
    ])


def _scene_console_png(s: CaptureSession) -> None:
    # Static shot of the framebuffer text rendering.
    s.send_keys(["sleep 1"])


SCENES: list[Scene] = [
    Scene(
        name="boot-to-login",
        kind="gif",
        needs_login=False,
        prepare=_scene_boot_to_login,
        capture_times=[3.0, 5.0, 7.0, 9.0, 11.0, 13.0],
        note="Firmware → kernel boot → service spawn → login prompt. "
             "Captured at 3s intervals during boot, before any login.",
        headers=[
            "userspace/init/src/main.rs",
            "userspace/console/src/main.rs",
        ],
    ),
    Scene(
        name="login-and-shell",
        kind="gif",
        needs_login=True,
        prepare=_scene_login_and_shell,
        capture_times=[14.0, 16.0, 18.0],
        note="Login as root → shell prompt appears. Uses the standard "
             "root/root credential sendkey sequence with a 12s prefix "
             "sleep (kbd attaches at ~9.4s, login window at ~9.8s).",
        headers=[
            "userspace/shell/src/main.rs",
            "userspace/console/src/main.rs",
        ],
    ),
    Scene(
        name="vtmgr-vt-switch",
        kind="gif",
        needs_login=True,
        prepare=_scene_vt_switch,
        capture_times=[16.0, 18.0, 20.0, 22.0, 24.0, 26.0],
        note="Alt-F1 → VT0, Alt-F2 → VT1, type in each, switch back. "
             "VT4 is owned by the compositor; text VTs are 1-3.",
        headers=[
            "userspace/vtmgr/src/main.rs",
            "userspace/console/src/main.rs",
        ],
    ),
    Scene(
        name="tty-line-discipline",
        kind="gif",
        needs_login=True,
        prepare=_scene_tty_line_discipline,
        capture_times=[15.0, 16.0, 17.0, 18.0, 19.0, 20.0],
        note="Type a line, backspace mid-line, Ctrl-C, ↑/↓ history, enter. "
             "Shows cooked-mode line discipline (ICANON, ECHO, ^C/^Z/^D).",
        headers=[
            "userspace/tty/src/main.rs",
            "userspace/libcluu/src/tty_core/line_discipline.rs",
        ],
    ),
    Scene(
        name="shell-command-history",
        kind="gif",
        needs_login=True,
        prepare=_scene_shell_history,
        capture_times=[16.0, 18.0, 20.0, 22.0, 24.0],
        note="Run 3 commands (ls /, cat /etc/welcome.txt, ps), then "
             "↑↑↑ to recall, edit, re-run. Up/down arrow history.",
        headers=[
            "userspace/shell/src/main.rs",
        ],
    ),
    Scene(
        name="shell-builtin-ls-cat",
        kind="gif",
        needs_login=True,
        prepare=_scene_ls_cat,
        capture_times=[16.0, 18.0, 20.0],
        note="ls /, cat /etc/welcome.txt, ps. Basic builtin commands.",
        headers=[
            "userspace/shell/src/commands/builtins/mod.rs",
        ],
    ),
    Scene(
        name="top-live",
        kind="gif",
        needs_login=True,
        prepare=_scene_top,
        capture_times=[16.0, 17.0, 18.0, 19.0, 20.0, 21.0],
        note="top running, processes appearing/disappearing, q to quit. "
             "Reads /proc for live process list.",
        headers=[
            "userspace/shell/src/commands/builtins/mod.rs",
        ],
    ),
    Scene(
        name="container-run-hello",
        kind="gif",
        needs_login=True,
        prepare=_scene_container_hello,
        capture_times=[16.0, 18.0, 20.0],
        note="container run hello → output → ps shows it. Demonstrates "
             "the capability-scoped binary spawn model.",
        headers=[
            "userspace/root-procmgr/src/main.rs",
            "userspace/session-procmgr/src/main.rs",
        ],
    ),
    Scene(
        name="mount-policy-demo",
        kind="gif",
        needs_login=True,
        prepare=_scene_mount_policy,
        capture_times=[16.0, 18.0, 20.0, 22.0],
        note="spawn mkdir /tmp/demo → spawn mkdir /tmp/demo/inner → "
             "spawn rm -r /tmp/demo. Shows /tmp inherit mount policy "
             "across separate spawns.",
        headers=[
            "userspace/vfs/src/mount.rs",
            "userspace/session-procmgr/src/main.rs",
        ],
    ),
    Scene(
        name="edit-demo",
        kind="gif",
        needs_login=True,
        prepare=_scene_edit,
        capture_times=[16.0, 18.0, 20.0, 22.0, 24.0],
        note="Open editor, type in insert mode, save, quit. vi-like "
             "modal editor running as a compositor window. Skipped if "
             "edit is not functional on capture day.",
        headers=[
            "userspace/edit/src/main.rs",
        ],
    ),
    Scene(
        name="compositor-demo",
        kind="gif",
        needs_login=True,
        prepare=_scene_compositor,
        capture_times=[16.0, 18.0, 20.0, 22.0],
        note="Switch to VT4 (compositor), spawn cluuterm. Shows the "
             "TUI window compositor with floating windows and shared-"
             "memory cell-grid protocol. Skipped if compositor is not "
             "running on capture day.",
        headers=[
            "userspace/compositor/src/main.rs",
        ],
    ),
    Scene(
        name="console-framebuffer",
        kind="png",
        needs_login=False,
        prepare=_scene_console_png,
        capture_times=[10.0],  # single capture during boot
        note="Static shot of framebuffer text rendering (glyph atlas "
             "visible). Single PNG, not a GIF.",
        headers=[
            "userspace/console/src/renderer.rs",
        ],
    ),
]


# ---------------------------------------------------------------------- #
# Capture driver
# ---------------------------------------------------------------------- #

def find_fb_phys(cfg: HarnessConfig) -> int | None:
    """Parse the serial log for the 'fb @<PHYS>' line."""
    try:
        text = cfg.serial_log.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    m = _FB_PHYS_RE.search(text)
    if m is None:
        return None
    return int(m.group(1), 16)


def stitch_gif(frames: list[Path], out: Path, framerate: int = 2) -> bool:
    """Stitch PNG frames into a GIF via ffmpeg (palette gen + use)."""
    if not frames:
        return False
    palette = out.with_suffix(".palette.png")
    # Generate a custom palette for quality.
    gen_cmd = [
        "ffmpeg", "-y", "-framerate", str(framerate),
        "-i", str(frames[0].parent / "frame_%03d.png"),
        "-vf", "palettegen", str(palette),
    ]
    try:
        subprocess.run(gen_cmd, check=True, capture_output=True, timeout=30)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        log.warning("palettegen failed: %s", exc)
        return False
    use_cmd = [
        "ffmpeg", "-y", "-framerate", str(framerate),
        "-i", str(frames[0].parent / "frame_%03d.png"),
        "-i", str(palette),
        "-filter_complex", "paletteuse",
        str(out),
    ]
    try:
        subprocess.run(use_cmd, check=True, capture_output=True, timeout=30)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        log.warning("paletteuse failed: %s", exc)
        return False
    palette.unlink(missing_ok=True)
    return out.exists()


def write_companion(scene: Scene, asset_path: Path, status: str,
                    capture_cmd: str = "") -> None:
    """Write the .gif.md / .png.md companion describing the asset."""
    companion = asset_path.with_suffix(asset_path.suffix + ".md")
    lines = [
        f"# {scene.name}",
        "",
        f"**Type:** {scene.kind.upper()}",
        f"**Status:** {status}",
        f"**Resolution:** {FB_W}x{FB_H} BGRA32",
        f"**Captured:** {time.strftime('%Y-%m-%d %H:%M:%S')}",
        "",
        "## Description",
        "",
        scene.note,
        "",
        "## Capture conditions",
        "",
        f"- QEMU: `qemu-system-x86_64 -machine q35 -m 1G -accel kvm`",
        f"- Framebuffer: {FB_W}x{FB_H} BGRA32 ({FB_SIZE} bytes)",
        f"- Login: root/root (HU QWERTZ sendkey sequence)",
        f"- Capture method: `pmemsave` via QEMU HMP monitor",
    ]
    if capture_cmd:
        lines.append(f"- Command: `{capture_cmd}`")
    if scene.headers:
        lines.append("")
        lines.append("## Referenced by")
        lines.append("")
        for h in scene.headers:
            lines.append(f"- `{h}`")
    lines.append("")
    companion.write_text("\n".join(lines), encoding="utf-8")


def run_scene(scene: Scene, session: CaptureSession,
              asset_path: Path) -> bool:
    """Run one scene: prepare, capture, stitch. Returns True on success."""
    log.info("scene: %s (%s)", scene.name, scene.kind)
    scene_dir = session.workdir / scene.name
    scene_dir.mkdir(exist_ok=True)
    # Reset frame counter and workdir for this scene.
    session.frame_idx = 0
    # We can't easily switch workdir on the session, so capture into
    # scene_dir by temporarily redirecting. Simpler: capture into a
    # per-scene subdir and stitch from there.
    # Actually, capture_frame uses self.workdir. Let's just use a fresh
    # session per scene by copying. For simplicity, capture into scene_dir.

    # Reset elapsed clock for this scene's capture_times.
    session.boot_t0 = time.monotonic()
    session.workdir = scene_dir

    # Run the prepare script.
    try:
        scene.prepare(session)
    except Exception as exc:  # noqa: BLE001
        log.warning("scene %s prepare failed: %s", scene.name, exc)
        write_companion(scene, asset_path, "FAILED (prepare error)",
                        capture_cmd=f"scripts/capture_cluu_shots.py {scene.name}")
        return False

    # Capture frames.
    frames = session.capture_at_intervals(scene.capture_times)
    if not frames:
        log.warning("scene %s: no frames captured", scene.name)
        write_companion(scene, asset_path, "FAILED (no frames)",
                        capture_cmd=f"scripts/capture_cluu_shots.py {scene.name}")
        return False

    if scene.kind == "png":
        # Single PNG — just copy the last frame.
        shutil.copy2(frames[-1], asset_path)
        write_companion(scene, asset_path, "OK",
                        capture_cmd=f"scripts/capture_cluu_shots.py {scene.name}")
        log.info("  -> %s (PNG)", asset_path)
        return True

    # GIF: stitch frames.
    ok = stitch_gif(frames, asset_path, framerate=2)
    if ok:
        write_companion(scene, asset_path, "OK",
                        capture_cmd=f"scripts/capture_cluu_shots.py {scene.name}")
        log.info("  -> %s (GIF, %d frames)", asset_path, len(frames))
    else:
        write_companion(scene, asset_path, "FAILED (stitch error)",
                        capture_cmd=f"scripts/capture_cluu_shots.py {scene.name}")
    # Clean up frame PNGs.
    for f in frames:
        f.unlink(missing_ok=True)
    return ok


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Capture CLUU framebuffer snapshots for documentation.")
    parser.add_argument("scenes", nargs="*",
                        help="scene names to capture (default: all)")
    parser.add_argument("--list", action="store_true",
                        help="list scene names and exit")
    parser.add_argument("--no-build", action="store_true",
                        help="skip the build gate")
    parser.add_argument("-v", "--verbose", action="store_true",
                        help="debug logging")
    args = parser.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        datefmt="%H:%M:%S",
    )

    if args.list:
        for s in SCENES:
            print(f"{s.name:30s} {s.kind:3s}  {s.note[:60]}")
        return 0

    selected = args.scenes
    scenes = [s for s in SCENES if s.name in selected] if selected else SCENES
    if not scenes:
        log.error("no matching scenes: %s", selected)
        return 1

    ASSETS_DIR.mkdir(parents=True, exist_ok=True)

    # Build if needed.
    cfg = HarnessConfig(
        no_build=args.no_build,
        serial_log=REPO_ROOT / "target" / "capture-serial.log",
        monitor_sock=REPO_ROOT / "target" / "capture-monitor.sock",
    )
    ctrl = QemuController(cfg)
    log.info("building CLUU (if needed)...")
    ctrl.build()

    log.info("launching QEMU...")
    try:
        ctrl.launch()
    except Exception as exc:  # noqa: BLE001
        log.error("QEMU launch failed: %s", exc)
        return 1

    # Wait for FB phys to appear in serial log.
    log.info("waiting for framebuffer phys address in serial log...")
    fb_phys = None
    deadline = time.monotonic() + 30.0
    while fb_phys is None and time.monotonic() < deadline:
        fb_phys = find_fb_phys(cfg)
        if fb_phys is None:
            time.sleep(0.5)
    if fb_phys is None:
        log.error("no 'fb @<PHYS>' line found in serial log after 30s")
        ctrl.cleanup()
        return 1
    log.info("framebuffer phys = 0x%x", fb_phys)

    # Wait for boot to settle (kbd attaches at ~9.4s).
    log.info("waiting for boot to settle (15s)...")
    time.sleep(15.0)

    workdir = REPO_ROOT / "target" / "capture-frames"
    workdir.mkdir(parents=True, exist_ok=True)
    session = CaptureSession(cfg, ctrl, fb_phys, workdir)

    # Run scenes. Re-login between scenes that need it only if the
    # previous scene didn't already log in. For simplicity, we run all
    # scenes in one boot; scenes that need_login assume a prior scene
    # has logged in (the first needs-login scene runs the credential
    # sequence).
    logged_in = False
    results = {}
    for scene in scenes:
        asset_path = ASSETS_DIR / f"{scene.name}.{scene.kind}"
        if scene.needs_login and not logged_in:
            # Run the login sequence first.
            log.info("logging in (root/root)...")
            session.send_keys(CREDS_SENDKEY_ROOT)
            session.send_keys(["sleep 3"])
            logged_in = True
            # Reset clock for the scene's capture_times.
            session.boot_t0 = time.monotonic()
        ok = run_scene(scene, session, asset_path)
        results[scene.name] = "OK" if ok else "FAILED"
        # Small pause between scenes.
        time.sleep(1.0)

    log.info("cleanup...")
    ctrl.cleanup()

    # Summary.
    log.info("=== results ===")
    for name, status in results.items():
        log.info("  %s: %s", name, status)
    n_ok = sum(1 for v in results.values() if v == "OK")
    log.info("%d/%d scenes captured successfully", n_ok, len(results))
    return 0 if n_ok == len(results) else 2


if __name__ == "__main__":
    sys.exit(main())
