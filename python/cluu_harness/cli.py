"""Command-line interface for the CLUU harness."""

from __future__ import annotations

import argparse
import logging
import sys
from pathlib import Path

from cluu_harness.case_defaults import get_defaults
from cluu_harness.cases import registry
from cluu_harness.config import HarnessConfig
from cluu_harness.markers import list_modes
from cluu_harness.suite import run_case, run_suite


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="cluu-harness",
        description="CLUU gen2 test harness (event-driven QEMU integration tests)",
    )
    p.add_argument(
        "--list", action="store_true",
        help="list registered cases and exit",
    )
    p.add_argument(
        "--list-modes", action="store_true",
        help="list known MARKER_MODEs and exit",
    )
    p.add_argument(
        "--case", action="append", default=[],
        help="run only the named case (repeatable)",
    )
    p.add_argument(
        "--no-build", action="store_true",
        help="reuse existing build artifacts",
    )
    p.add_argument(
        "--marker-mode",
        help="override MARKER_MODE for an ad-hoc run",
    )
    p.add_argument(
        "--serial-log", type=Path,
        help="serial log path (default: /tmp/cluu-serial-com2.log)",
    )
    p.add_argument(
        "--verbose", "-v", action="store_true",
        help="debug-level logging",
    )
    p.add_argument(
        "--stop-on-fail", action="store_true",
        help="stop the suite after the first failure",
    )
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        datefmt="%H:%M:%S",
    )

    if args.list:
        for name in registry.names():
            case = registry.get(name)
            desc = case.description or get_defaults(case.marker_mode).test_command or ""
            print(f"{name:30s}  marker_mode={case.marker_mode:20s}  {desc}")
        return 0

    if args.list_modes:
        for m in list_modes():
            print(m)
        return 0

    # Build config from env, applying CLI overrides.
    cfg = HarnessConfig()
    if args.no_build:
        cfg.no_build = True
    if args.marker_mode:
        cfg.marker_mode = args.marker_mode
    if args.serial_log:
        cfg.serial_log = args.serial_log

    # Select cases.
    if args.case:
        cases = []
        for name in args.case:
            try:
                cases.append(registry.get(name))
            except KeyError:
                print(f"ERROR: case not found: {name}", file=sys.stderr)
                return 2
    else:
        cases = registry.all_cases()
        if not cases:
            print("ERROR: no cases registered", file=sys.stderr)
            return 2

    if len(cases) == 1:
        result = run_case(cases[0], cfg)
        print(f"=== {result.name}: {result.status} ({result.elapsed_s:.1f}s) ===")
        if not result.passed:
            if result.error:
                print(f"  error: {result.error}", file=sys.stderr)
            if result.missing_markers:
                print(f"  missing: {result.missing_markers}", file=sys.stderr)
            if result.fail_line:
                print(f"  fail: {result.fail_line}", file=sys.stderr)
            if result.post_check_message:
                print(f"  SLO: {result.post_check_message}", file=sys.stderr)
        return 0 if result.passed else 1

    suite = run_suite(cases, cfg, stop_on_fail=args.stop_on_fail)
    print()
    print(f"=== SUITE SUMMARY: {suite.passed} passed, {suite.failed} failed ===")
    if suite.failed:
        print("Failed cases:")
        for name in suite.failed_names:
            print(f"  - {name}")
        return 1
    print("All harness suite cases passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
