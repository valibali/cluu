#!/usr/bin/env python3
"""Re-parse existing serial logs into baseline report (no QEMU re-run needed)."""
from __future__ import annotations

import json
import logging
import sys
from collections import defaultdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "python"))

from cluu_harness.baseline import (
    BASELINE_STATES,
    StateResult,
    WindowResult,
    _compute_thread_cpu,
    _parse_probes,
    _split_windows_by_time,
    format_report,
)

EVIDENCE_DIR = REPO_ROOT / ".omo/evidence/task-2-raw-logs"
REPORT_PATH = REPO_ROOT / ".omo/evidence/task-2-cluu-multimedia-stack.md"

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s: %(message)s", datefmt="%H:%M:%S")
log = logging.getLogger("reparse")


def reparse(case_name: str, label: str, n_samples: int = 3) -> StateResult:
    serial_path = EVIDENCE_DIR / f"{case_name}.serial.log"
    if not serial_path.exists():
        log.error("missing %s", serial_path)
        return StateResult(state=case_name, display="none", passed=False, error="serial log missing")

    text = serial_path.read_text(encoding="utf-8", errors="replace")
    probes = _parse_probes(text)

    all_ts: list[float] = []
    for kvs in probes.values():
        for tvs in kvs.values():
            all_ts.extend(ts for ts, _ in tvs if tvs)
    probe_start = min(all_ts) if all_ts else 0.0
    probe_end = max(all_ts) if all_ts else 60.0

    probe_windows = _split_windows_by_time(probes, n_samples, probe_start, probe_end)
    pw_dur = (probe_end - probe_start) / n_samples if probe_end > probe_start else 20.0

    sr = StateResult(state=case_name, display="none", passed=True)
    for i in range(n_samples):
        pw = probe_windows[i] if i < len(probe_windows) else {}
        frame_count = len(pw.get("BENCH_DOOM_FRAME", {}).get("dt_cycles", []))
        fps = frame_count / pw_dur if pw_dur > 0 else 0.0
        sr.windows.append(WindowResult(
            index=i, start_s=probe_start + i * pw_dur, duration_s=pw_dur,
            thread_cpu_pct={}, probe_values=pw, frame_count=frame_count, fps=fps,
        ))

    data = {
        "state": case_name, "label": label, "display": "none",
        "passed": True, "error": None, "n_windows": len(sr.windows),
        "windows": [
            {
                "index": w.index, "duration_s": w.duration_s,
                "thread_cpu_pct": w.thread_cpu_pct,
                "frame_count": w.frame_count, "fps": w.fps,
                "probe_counts": {k: {kk: len(vv) for kk, vv in v.items()} for k, v in w.probe_values.items()},
            }
            for w in sr.windows
        ],
    }
    json_path = EVIDENCE_DIR / f"{case_name}.json"
    json_path.write_text(json.dumps(data, indent=2))
    log.info("  %s: %d windows, %d probes total", case_name,
             len(sr.windows), sum(len(v) for w in sr.windows for v in w.probe_values.values()))
    return sr


def main() -> int:
    results = []
    for case_name, label in BASELINE_STATES:
        log.info("=== Reparsing: %s ===", label)
        results.append(reparse(case_name, label))

    host_config = {
        "QEMU": "qemu-system-x86_64 11.0.2",
        "Host CPU": "11th Gen Intel(R) Core(TM) i7-1185G7 @ 3.00GHz (4 cores, 8 threads)",
        "KVM": "enabled (-accel kvm -cpu host)",
        "Memory": "1G guest",
        "Display": "none (headless) — no GTK display thread",
        "Kernel": "Linux 6.8.0-107-generic",
        "Cargo profile": "release (promote_to_release in container-build)",
        "Bench feature": "enabled (CLUU_BENCH=1, cfg(feature=bench) gating)",
        "Sample windows": "3 equal-duration per state, split by probe timestamp range",
    }

    report = format_report(results, host_config)
    REPORT_PATH.write_text(report)
    log.info("Report: %s", REPORT_PATH)
    return 0


if __name__ == "__main__":
    sys.exit(main())
