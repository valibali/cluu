# CLUU Harness and SLO Tooling

## Purpose

The harness stack is split into reusable layers so new cases and SLO checks can be added without editing multiple scripts.

## Components

1. `test_hello.sh`
   - Single-run QEMU harness executor.
   - Boots CLUU, injects command(s), validates markers/faults.
   - Supports overrides for paths and debug:
     - `SERIAL_LOG`, `MONITOR_SOCK`, `IMG`, `USER_DISK`, `OVMF`
     - `QEMU_GDB=1` to start QEMU with `-S -s`
     - `QEMU_EXTRA_ARGS` for additional QEMU flags

2. `scripts/harness_cases.conf`
   - Central case catalog (`name|build_mode|env_assignments`).
   - Add new CI scenarios here.

3. `scripts/harness_suite.sh`
   - Generic case runner consuming `harness_cases.conf`.
   - Supports:
     - `--no-build` (force artifact reuse)
     - `--case NAME` (run one case)
     - `--list` (enumerate known cases)

4. `scripts/harness_matrix.sh`
   - Compatibility wrapper to keep existing CI/xtask entrypoints stable.
   - Delegates to `harness_suite.sh`.

5. `scripts/harness_slo_report.sh`
   - Standalone SLO parser/enforcer for a serial log.
   - Extracts and checks:
     - exit cookie count
     - delta resource metrics
     - IPC fairness metrics (`p95`, `p99`, scan average)

6. `scripts/harness_slo_sweep.sh`
   - Repeated fairness runs + per-run SLO report.
   - Emits CSV at `tmp/harness_slo/summary.csv`.

## Common Commands

```bash
# Full matrix
cargo xtask harness-matrix

# Matrix reusing build artifacts
cargo xtask harness-matrix --no-build

# One case only
scripts/harness_suite.sh --case m5_fairness --no-build

# Fairness SLO sweep
cargo xtask harness-slo --no-build --repeats 5

# Parse/enforce SLOs from an existing log
scripts/harness_slo_report.sh --log /tmp/cluu-serial-com2.log --min-exit-cookies 6 --max-ipc-wait-p95-ms 16
```

## CI Extension Workflow

1. Add a new case line in `scripts/harness_cases.conf`.
2. Add marker checks in `test_hello.sh` for new `MARKER_MODE`.
3. If new numeric SLO appears, add parsing/check in `scripts/harness_slo_report.sh`.
4. Gate it in CI via `cargo xtask harness-matrix` and/or `cargo xtask harness-slo`.
