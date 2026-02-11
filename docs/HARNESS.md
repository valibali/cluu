# CLUU Harness and SLO Tooling

## Purpose

The harness stack is split into reusable layers so new cases and SLO checks can be added without editing multiple scripts.

## Components

1. `test_hello.sh`
   - Single-run QEMU harness executor.
   - Boots CLUU, injects command(s), validates markers/faults.
   - Shell readiness policy: default `SHELL_READY_WAIT=15` and hard max `SHELL_READY_WAIT_MAX=15`.
   - Shell-ready timeout is measured from QEMU launch (not from command injection phase).
   - Override only for explicit debugging with `ALLOW_SLOW_SHELL_WAIT=1`.
   - Build modes:
     - Default full mode is incremental and runs `cargo xtask build` (plus toolchain prep only if missing).
     - `HARNESS_CLEAN_REBUILD=1` forces clean toolchain/image rebuild (`make clean` + newlib/syscalls/crt0 + full build).
   - Supports overrides for paths and debug:
     - `SERIAL_LOG`, `MONITOR_SOCK`, `IMG`, `USER_DISK`, `OVMF`
     - `QEMU_GDB=1` to start QEMU with `-S -s`
     - `QEMU_EXTRA_ARGS` for additional QEMU flags
   - M6 IPC SLO env gates:
     - `MAX_IPC_WAIT_P95_MS`, `MAX_IPC_WAIT_P99_MS`, `MAX_IPC_SCAN_AVG_STEPS_X100`
     - `MAX_IPC_QUEUE_BYTES_PEAK`, `MAX_IPC_QUEUE_MESSAGES_PEAK`
   - Warm-cache spawn SLO mode (`MARKER_MODE=b_spawn_warm`):
     - Parses `/bin/noop` `procmgr: spawn_trace ... stage=reply_sent ... dt=...` samples.
     - Parses `/bin/noop` `vfs: map_elf_trace ... stage=reply ... dt=...` samples.
     - Emits inline metrics:
       - `HARNESS noop_spawn_reply_samples=...`
       - `HARNESS noop_map_elf_reply_samples=...`
       - `HARNESS noop_spawn_reply_p95_cycles=...`
       - `HARNESS noop_map_elf_reply_p95_cycles=...`
     - Optional SLO gates:
       - `MIN_NOOP_SPAWN_SAMPLES`, `MIN_NOOP_MAP_ELF_SAMPLES`
       - `MAX_NOOP_SPAWN_REPLY_P95_CYCLES`, `MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES`

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
     - IPC queue pressure metrics (`ipc_queue_bytes_peak`, `ipc_queue_messages_peak`) when present in logs
     - shell readiness latency (`shell_ready_s`, default max 15s)
     - warm-cache `/bin/noop` sample counts and p95 cycle metrics
   - `test_hello.sh` also appends:
     - `HARNESS build_s=...`
     - `HARNESS qemu_to_shell_ready_s=...`
     - `HARNESS total_s=...`

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

# Warm-cache spawn/map_elf sweep
SLO_MODE=b_spawn_warm MIN_NOOP_SPAWN_SAMPLES=8 MIN_NOOP_MAP_ELF_SAMPLES=8 \
MAX_NOOP_SPAWN_REPLY_P95_CYCLES=120000000 MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES=30000000 \
cargo xtask harness-slo --no-build --repeats 5

# Parse/enforce SLOs from an existing log
scripts/harness_slo_report.sh --log /tmp/cluu-serial-com2.log --min-exit-cookies 6 --max-ipc-wait-p95-ms 16 --max-shell-ready-s 15
```

## CI Extension Workflow

1. Add a new case line in `scripts/harness_cases.conf`.
2. Add marker checks in `test_hello.sh` for new `MARKER_MODE`.
3. If new numeric SLO appears, add parsing/check in `scripts/harness_slo_report.sh`.
4. Gate it in CI via `cargo xtask harness-matrix` and/or `cargo xtask harness-slo`.
