#!/usr/bin/env python3
"""Collect multimedia baseline data for all 4 states.

Runs each baseline state, polls QEMU thread CPU, parses serial probes,
and saves raw logs + structured data to .omo/evidence/task-2-raw-logs/.
"""
from __future__ import annotations

import json
import logging
import os
import shutil
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "python"))

from cluu_harness.baseline import (
    BASELINE_STATES,
    StateResult,
    format_report,
    run_baseline_state,
)
from cluu_harness.config import HarnessConfig

EVIDENCE_DIR = REPO_ROOT / ".omo/evidence/task-2-raw-logs"
REPORT_PATH = REPO_ROOT / ".omo/evidence/task-2-cluu-multimedia-stack.md"

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    datefmt="%H:%M:%S",
)
log = logging.getLogger("baseline-collect")


def collect_state(case_name: str, label: str, n_samples: int = 3, sample_s: float = 10.0) -> StateResult:
    log.info("=== Collecting: %s ===", label)
    cfg = HarnessConfig()
    cfg.no_build = True
    result = run_baseline_state(case_name, display="none", n_samples=n_samples, sample_s=sample_s, cfg=cfg)

    serial_log = Path(cfg.serial_log)
    raw_path = EVIDENCE_DIR / f"{case_name}.serial.log"
    if serial_log.exists():
        shutil.copy2(serial_log, raw_path)
        log.info("  serial log saved: %s (%d bytes)", raw_path.name, raw_path.stat().st_size)

    data = {
        "state": case_name,
        "label": label,
        "display": "none",
        "passed": result.passed,
        "error": result.error,
        "n_windows": len(result.windows),
        "windows": [
            {
                "index": w.index,
                "duration_s": w.duration_s,
                "thread_cpu_pct": w.thread_cpu_pct,
                "frame_count": w.frame_count,
                "fps": w.fps,
                "probe_counts": {k: {kk: len(vv) for kk, vv in v.items()} for k, v in w.probe_values.items()},
            }
            for w in result.windows
        ],
    }
    json_path = EVIDENCE_DIR / f"{case_name}.json"
    json_path.write_text(json.dumps(data, indent=2))
    log.info("  json saved: %s", json_path.name)

    return result


def main() -> int:
    EVIDENCE_DIR.mkdir(parents=True, exist_ok=True)

    results: list[StateResult] = []
    for case_name, label in BASELINE_STATES:
        sr = collect_state(case_name, label)
        results.append(sr)
        if not sr.passed:
            log.warning("  STATE FAILED: %s — %s", label, sr.error)

    host_config = {
        "QEMU": "qemu-system-x86_64 11.0.2",
        "Host CPU": "11th Gen Intel(R) Core(TM) i7-1185G7 @ 3.00GHz (4 cores, 8 threads)",
        "KVM": "enabled (-accel kvm -cpu host)",
        "Memory": "1G guest",
        "Display": "none (headless)",
        "Kernel": "Linux 6.8.0-107-generic",
        "Cargo profile": "release (promote_to_release in container-build)",
        "Bench feature": "enabled (CLUU_BENCH=1)",
        "Sample windows": "3 x 10s per state",
    }

    report = format_report(results, host_config)
    REPORT_PATH.write_text(report)
    log.info("Report written: %s", REPORT_PATH)

    all_passed = all(r.passed for r in results)
    log.info("=== ALL STATES %s ===", "PASSED" if all_passed else "FAILED")
    return 0 if all_passed else 1


if __name__ == "__main__":
    sys.exit(main())
