"""Multimedia baseline runner: QEMU thread CPU + guest probe collection.

Runs a harness case for an extended period, polls QEMU per-thread CPU via
``/proc/<pid>/task/*/stat``, and parses serial probe markers emitted by the
compositor and SDL2 shim (gated behind the ``bench`` cargo feature).

Produces ≥3 equal-duration sample windows per state and computes median/p95
for vCPU vs display/main thread CPU%, guest stage cycles, bytes/frame, frame
cadence, and damage area.
"""

from __future__ import annotations

import logging
import os
import re
import threading
import time
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from statistics import median
from typing import Sequence

from cluu_harness.cases import registry
from cluu_harness.config import HarnessConfig
from cluu_harness.suite import run_case

log = logging.getLogger(__name__)

_CLK_TCK = os.sysconf(os.sysconf_names["SC_CLK_TCK"])

_PROBE_RE = re.compile(r"(BENCH_\w+): (.+)")
_KV_RE = re.compile(r"(\w+)=(\S+)")
_TS_RE = re.compile(r"^\[\s*(\d+\.\d+)\]")
_RECT_RE = re.compile(r"rect=(\d+)x(\d+)")


@dataclass
class ThreadSample:
    timestamp: float
    threads: dict[int, tuple[str, int, int]]


@dataclass
class WindowResult:
    index: int
    start_s: float
    duration_s: float
    thread_cpu_pct: dict[str, float]
    probe_values: dict[str, dict[str, list[int]]]
    frame_count: int
    fps: float


@dataclass
class StateResult:
    state: str
    display: str
    windows: list[WindowResult] = field(default_factory=list)
    passed: bool = False
    error: str | None = None

    def aggregate(self, probe: str, key: str) -> list[int]:
        vals: list[int] = []
        for w in self.windows:
            vals.extend(w.probe_values.get(probe, {}).get(key, []))
        return vals

    def thread_cpu_series(self, thread_class: str) -> list[float]:
        return [w.thread_cpu_pct.get(thread_class, 0.0) for w in self.windows]


def _percentile_p95(values: Sequence[int | float]) -> float | None:
    if not values:
        return None
    vals = sorted(values)
    n = len(vals)
    rank = max(1, (95 * n + 99) // 100)
    return float(vals[rank - 1])


def _median(values: Sequence[int | float]) -> float | None:
    if not values:
        return None
    return float(median(values))


def _read_thread_stat(pid: int) -> dict[int, tuple[str, int, int]]:
    result: dict[int, tuple[str, int, int]] = {}
    task_dir = Path(f"/proc/{pid}/task")
    try:
        tids = [int(e.name) for e in os.scandir(task_dir) if e.name.isdigit()]
    except (OSError, FileNotFoundError):
        return result
    for tid in tids:
        try:
            raw = (task_dir / str(tid) / "stat").read_text()
            # field 2 is (comm), fields 14/15 are utime/stime
            rparen = raw.rindex(")")
            comm = raw[raw.index("(") + 1 : rparen]
            rest = raw[rparen + 2 :].split()
            utime = int(rest[11])
            stime = int(rest[12])
            result[tid] = (comm, utime, stime)
        except (OSError, ValueError, IndexError):
            continue
    return result


def _classify_thread(comm: str) -> str:
    c = comm.lower()
    if "cpu" in c and "qemu" not in c:
        return "vcpu"
    if "_cpu_" in c or c.startswith("cpu"):
        return "vcpu"
    if "gd_" in c or "display" in c or "gtk" in c:
        return "display"
    if "qemu" in c:
        return "main"
    return "other"


def _compute_thread_cpu(
    samples: list[ThreadSample],
) -> dict[str, float]:
    if len(samples) < 2:
        return {}
    class_totals: dict[str, list[float]] = defaultdict(list)
    for i in range(1, len(samples)):
        dt_wall = samples[i].timestamp - samples[i - 1].timestamp
        if dt_wall <= 0:
            continue
        prev = samples[i - 1].threads
        curr = samples[i].threads
        per_class: dict[str, float] = defaultdict(float)
        for tid, (comm, utime, stime) in curr.items():
            if tid in prev:
                _, p_utime, p_stime = prev[tid]
                delta = (utime - p_utime) + (stime - p_stime)
                pct = (delta / (_CLK_TCK * dt_wall)) * 100.0
                cls = _classify_thread(comm)
                per_class[cls] += pct
        for cls, pct in per_class.items():
            class_totals[cls].append(pct)
    result: dict[str, float] = {}
    for cls, pcts in class_totals.items():
        result[cls] = _median(pcts) if pcts else 0.0
    return result


def _parse_probes(serial_text: str) -> dict[str, dict[str, list[tuple[float, int]]]]:
    """Parse probe markers, returning probe -> key -> [(timestamp, value), ...]."""
    result: dict[str, dict[str, list[tuple[float, int]]]] = defaultdict(
        lambda: defaultdict(list)
    )
    for line in serial_text.splitlines():
        m = _PROBE_RE.search(line)
        if not m:
            continue
        probe = m.group(1)
        rest = m.group(2)
        ts = 0.0
        tm = _TS_RE.match(line)
        if tm:
            ts = float(tm.group(1))
        for km in _KV_RE.finditer(rest):
            key, val = km.group(1), km.group(2)
            try:
                result[probe][key].append((ts, int(val)))
            except ValueError:
                pass
    return {k: dict(v) for k, v in result.items()}


def _split_windows_by_time(
    probes: dict[str, dict[str, list[tuple[float, int]]]],
    n_windows: int,
    t_start: float,
    t_end: float,
) -> list[dict[str, dict[str, list[int]]]]:
    """Split probes into n equal-duration time windows.

    Each probe value is a list of (timestamp, value) tuples. We split
    them by timestamp into [t_start, t_start+dur), [t_start+dur, t_start+2*dur), etc.
    Returns plain int lists per window.
    """
    total = t_end - t_start
    if total <= 0:
        return [{} for _ in range(n_windows)]
    window_dur = total / n_windows
    windows: list[dict[str, dict[str, list[int]]]] = []
    for i in range(n_windows):
        ws = t_start + i * window_dur
        we = t_start + (i + 1) * window_dur if i < n_windows - 1 else t_end + 0.01
        wp: dict[str, dict[str, list[int]]] = {}
        for probe, kvs in probes.items():
            wp[probe] = {}
            for key, tvs in kvs.items():
                wp[probe][key] = [v for ts, v in tvs if ws <= ts < we]
        windows.append(wp)
    return windows


def _parse_damage_areas(serial_text: str) -> list[tuple[float, int]]:
    """Parse rect=WxH from BENCH_COMP_BB2FB_BYTES lines, returning (timestamp, area)."""
    result: list[tuple[float, int]] = []
    for line in serial_text.splitlines():
        if "BENCH_COMP_BB2FB_BYTES" not in line:
            continue
        rm = _RECT_RE.search(line)
        if not rm:
            continue
        w, h = int(rm.group(1)), int(rm.group(2))
        ts = 0.0
        tm = _TS_RE.match(line)
        if tm:
            ts = float(tm.group(1))
        result.append((ts, w * h))
    return result


def _find_qemu_pid() -> int | None:
    try:
        for entry in os.scandir("/proc"):
            if not entry.name.isdigit():
                continue
            try:
                comm = (Path(entry.path) / "comm").read_text().strip()
                if "qemu-system-x86" in comm:
                    return int(entry.name)
            except (OSError, FileNotFoundError):
                continue
    except OSError:
        pass
    return None


def run_baseline_state(
    case_name: str,
    display: str = "none",
    n_samples: int = 3,
    sample_s: float = 10.0,
    cfg: HarnessConfig | None = None,
) -> StateResult:
    """Run one baseline state and collect n_samples equal-duration windows."""
    cfg = cfg or HarnessConfig()
    cfg.qemu_display = display
    case = registry.get(case_name)
    total_run = max(case.run_wait_s or 30, int(sample_s * n_samples) + 10)
    cfg.run_wait_s = total_run

    result = StateResult(state=case_name, display=display)
    thread_samples: list[ThreadSample] = []
    stop_evt = threading.Event()

    def poller():
        pid: int | None = None
        while not stop_evt.is_set():
            if pid is None:
                pid = _find_qemu_pid()
            if pid is not None:
                ts = time.monotonic()
                threads = _read_thread_stat(pid)
                if threads:
                    thread_samples.append(ThreadSample(timestamp=ts, threads=threads))
            time.sleep(1.0)

    t = threading.Thread(target=poller, daemon=True)
    t.start()

    case_result = run_case(case, cfg)
    stop_evt.set()
    t.join(timeout=3.0)

    result.passed = case_result.passed
    result.error = case_result.error

    if not case_result.passed:
        result.error = f"case failed: {case_result.error or case_result.missing_markers}"
        return result

    serial_text = Path(case_result.serial_log).read_text(encoding="utf-8", errors="replace")
    probes = _parse_probes(serial_text)

    # Find the time range of probe emissions for window splitting.
    all_ts: list[float] = []
    for kvs in probes.values():
        for tvs in kvs.values():
            if tvs:
                all_ts.extend(ts for ts, _ in tvs)
    if all_ts:
        probe_start = min(all_ts)
        probe_end = max(all_ts)
    else:
        probe_start = 0.0
        probe_end = float(total_run)

    probe_windows = _split_windows_by_time(probes, n_samples, probe_start, probe_end)

    # Split thread samples into windows aligned with probe windows.
    if thread_samples:
        t0 = thread_samples[0].timestamp
        t_end = thread_samples[-1].timestamp
        total = t_end - t0
        window_dur = total / n_samples if total > 0 else sample_s
    else:
        t0 = 0.0
        window_dur = sample_s

    for i in range(n_samples):
        if thread_samples:
            ws = t0 + i * window_dur
            we = t0 + (i + 1) * window_dur if i < n_samples - 1 else t_end + 1
            w_samples = [s for s in thread_samples if ws <= s.timestamp < we]
        else:
            w_samples = []
        cpu_pct = _compute_thread_cpu(w_samples)
        pw = probe_windows[i] if i < len(probe_windows) else {}
        frame_count = len(pw.get("BENCH_DOOM_FRAME", {}).get("dt_cycles", []))
        # Use the probe window duration for fps calculation
        pw_dur = (probe_end - probe_start) / n_samples if probe_end > probe_start else window_dur
        fps = frame_count / pw_dur if pw_dur > 0 else 0.0
        result.windows.append(
            WindowResult(
                index=i,
                start_s=probe_start + i * pw_dur if probe_end > probe_start else ws if thread_samples else i * window_dur,
                duration_s=pw_dur if probe_end > probe_start else window_dur,
                thread_cpu_pct=cpu_pct,
                probe_values=pw,
                frame_count=frame_count,
                fps=fps,
            )
        )

    return result


BASELINE_STATES = [
    ("l2_baseline_idle_tui", "Idle TUI"),
    ("l2_baseline_quiet_shell", "Quiet shell"),
    ("l2_baseline_doom_windowed", "DOOM windowed"),
    ("l2_baseline_doom_fullscreen", "DOOM fullscreen"),
]


def format_report(
    results: list[StateResult],
    host_config: dict[str, str],
) -> str:
    lines: list[str] = []
    lines.append("# T2 — Multimedia Baseline Report\n")
    lines.append("## Pinned host/QEMU configuration\n")
    for k, v in host_config.items():
        lines.append(f"- **{k}**: {v}")
    lines.append("")

    for sr in results:
        label = dict(BASELINE_STATES).get(sr.state, sr.state)
        lines.append(f"## {label} (`{sr.state}`, display={sr.display})\n")
        if not sr.passed:
            lines.append(f"**FAILED**: {sr.error}\n")
            continue

        # QEMU thread CPU
        lines.append("### QEMU per-thread CPU%\n")
        lines.append("| Window | vCPU | display | main | other |")
        lines.append("|--------|------|---------|------|-------|")
        for w in sr.windows:
            lines.append(
                f"| {w.index} | {w.thread_cpu_pct.get('vcpu', 0):.1f} | "
                f"{w.thread_cpu_pct.get('display', 0):.1f} | "
                f"{w.thread_cpu_pct.get('main', 0):.1f} | "
                f"{w.thread_cpu_pct.get('other', 0):.1f} |"
            )
        for cls in ("vcpu", "display", "main", "other"):
            series = sr.thread_cpu_series(cls)
            if series:
                lines.append(
                    f"- {cls}: median={_median(series):.1f}% "
                    f"p95={_percentile_p95(series):.1f}%"
                )
        lines.append("")

        # Guest stage cycles
        lines.append("### Guest stage cycles (TSC)\n")
        for probe in (
            "BENCH_COMP_SHM2BB",
            "BENCH_COMP_GRID2BB",
            "BENCH_COMP_BB2FB_BYTES",
            "BENCH_COMP_FRAME",
            "BENCH_SHIM_UPDATE",
            "BENCH_SHIM_PRESENT",
            "BENCH_DOOM_FRAME",
        ):
            cycles = sr.aggregate(probe, "cycles") or sr.aggregate(probe, "dt_cycles")
            if cycles:
                lines.append(
                    f"- {probe}: n={len(cycles)} "
                    f"median={_median(cycles):.0f} "
                    f"p95={_percentile_p95(cycles):.0f}"
                )
        lines.append("")

        # Bytes per frame
        lines.append("### Bytes/frame\n")
        for probe in ("BENCH_COMP_SHM2BB", "BENCH_COMP_BB2FB_BYTES", "BENCH_SHIM_UPDATE"):
            b = sr.aggregate(probe, "bytes")
            if b:
                lines.append(
                    f"- {probe}: n={len(b)} "
                    f"median={_median(b):.0f} "
                    f"p95={_percentile_p95(b):.0f}"
                )
        lines.append("")

        # Frame cadence
        lines.append("### Frame cadence\n")
        for w in sr.windows:
            lines.append(f"- window {w.index}: {w.frame_count} frames, {w.fps:.1f} fps")
        fps_series = [w.fps for w in sr.windows]
        if fps_series:
            lines.append(
                f"- median fps={_median(fps_series):.1f} "
                f"p95={_percentile_p95(fps_series):.1f}"
            )
        lines.append("")

        # Damage area
        lines.append("### Damage area\n")
        for probe in ("BENCH_COMP_BB2FB_BYTES",):
            bytes_vals = sr.aggregate(probe, "bytes")
            if bytes_vals:
                lines.append(
                    f"- {probe} bytes/frame: n={len(bytes_vals)} "
                    f"median={_median(bytes_vals):.0f} "
                    f"p95={_percentile_p95(bytes_vals):.0f}"
                )
        lines.append("")

    return "\n".join(lines)


__all__ = [
    "BASELINE_STATES",
    "StateResult",
    "WindowResult",
    "run_baseline_state",
    "format_report",
]
