#!/bin/bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

REPEATS="${REPEATS:-5}"
SLO_MODE="${SLO_MODE:-m5_fairness}"
RUN_WAIT="${RUN_WAIT:-16}"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/tmp/harness_slo}"
NO_BUILD=0
QEMU_GDB="${QEMU_GDB:-0}"
if [[ -z "${TEST_COMMAND_REPEAT+x}" ]]; then
    if [[ "$SLO_MODE" == "b_spawn_warm" ]]; then
        TEST_COMMAND_REPEAT=2
    else
        TEST_COMMAND_REPEAT=1
    fi
fi

usage() {
    cat <<'EOF'
Usage: scripts/harness_slo_sweep.sh [--no-build] [--repeats N] [--out-dir PATH]

Runs selected SLO mode repeatedly and emits per-run SLO summaries.
Set mode via env `SLO_MODE` (default: `m5_fairness`).
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build)
            NO_BUILD=1
            shift
            ;;
        --repeats)
            REPEATS="${2:-}"
            shift 2
            ;;
        --out-dir)
            OUT_DIR="${2:-}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument '$1'"
            usage
            exit 1
            ;;
    esac
done

if ! [[ "$REPEATS" =~ ^[0-9]+$ ]] || [[ "$REPEATS" -lt 1 ]]; then
    echo "ERROR: REPEATS must be a positive integer"
    exit 1
fi

mkdir -p "$OUT_DIR"
summary_csv="$OUT_DIR/summary.csv"
echo "run,mode,exit_cookie_count,delta_spaces,delta_tokens,delta_endpoints,delta_pmm_used_frames,ipc_wait_p95_ms,ipc_wait_p99_ms,ipc_scan_avg_steps_x100,shell_ready_s,noop_spawn_reply_samples,noop_map_elf_reply_samples,noop_spawn_reply_p95_cycles,noop_map_elf_reply_p95_cycles" > "$summary_csv"

extract_kv() {
    local key="$1"
    local file="$2"
    awk -F'=' -v key="$key" '$1 == key { print $2; exit 0 }' "$file"
}

for ((run = 1; run <= REPEATS; run++)); do
    serial_log="$OUT_DIR/serial_run_${run}.log"
    report_log="$OUT_DIR/slo_run_${run}.txt"

    echo "=== SLO sweep run ${run}/${REPEATS} mode=${SLO_MODE} ==="
    env \
        SERIAL_LOG="$serial_log" \
        MARKER_MODE="$SLO_MODE" \
        TEST_COMMAND_REPEAT="$TEST_COMMAND_REPEAT" \
        RUN_WAIT="$RUN_WAIT" \
        MIN_EXIT_COOKIES=6 \
        QEMU_GDB="$QEMU_GDB" \
        MAX_IPC_WAIT_P95_MS="${MAX_IPC_WAIT_P95_MS:-}" \
        MAX_IPC_WAIT_P99_MS="${MAX_IPC_WAIT_P99_MS:-}" \
        MAX_IPC_SCAN_AVG_STEPS_X100="${MAX_IPC_SCAN_AVG_STEPS_X100:-}" \
        MIN_NOOP_SPAWN_SAMPLES="${MIN_NOOP_SPAWN_SAMPLES:-}" \
        MIN_NOOP_MAP_ELF_SAMPLES="${MIN_NOOP_MAP_ELF_SAMPLES:-}" \
        MAX_NOOP_SPAWN_REPLY_P95_CYCLES="${MAX_NOOP_SPAWN_REPLY_P95_CYCLES:-}" \
        MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES="${MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES:-}" \
        ./scripts/harness_run.sh $([[ "$NO_BUILD" -eq 1 ]] && echo "--no-build")

    ./scripts/harness_slo_report.sh \
        --log "$serial_log" \
        --min-exit-cookies "${MIN_EXIT_COOKIES:-6}" \
        ${MAX_DELTA_SPACES:+--max-delta-spaces "$MAX_DELTA_SPACES"} \
        ${MAX_DELTA_TOKENS:+--max-delta-tokens "$MAX_DELTA_TOKENS"} \
        ${MAX_DELTA_ENDPOINTS:+--max-delta-endpoints "$MAX_DELTA_ENDPOINTS"} \
        ${MAX_DELTA_PMM_USED_FRAMES:+--max-delta-pmm-used-frames "$MAX_DELTA_PMM_USED_FRAMES"} \
        ${MAX_IPC_WAIT_P95_MS:+--max-ipc-wait-p95-ms "$MAX_IPC_WAIT_P95_MS"} \
        ${MAX_IPC_WAIT_P99_MS:+--max-ipc-wait-p99-ms "$MAX_IPC_WAIT_P99_MS"} \
        ${MAX_IPC_SCAN_AVG_STEPS_X100:+--max-ipc-scan-avg-steps-x100 "$MAX_IPC_SCAN_AVG_STEPS_X100"} \
        ${MIN_NOOP_SPAWN_SAMPLES:+--min-noop-spawn-samples "$MIN_NOOP_SPAWN_SAMPLES"} \
        ${MIN_NOOP_MAP_ELF_SAMPLES:+--min-noop-map-elf-samples "$MIN_NOOP_MAP_ELF_SAMPLES"} \
        ${MAX_NOOP_SPAWN_REPLY_P95_CYCLES:+--max-noop-spawn-reply-p95-cycles "$MAX_NOOP_SPAWN_REPLY_P95_CYCLES"} \
        ${MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES:+--max-noop-map-elf-reply-p95-cycles "$MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES"} \
        ${MAX_SHELL_READY_S:+--max-shell-ready-s "$MAX_SHELL_READY_S"} \
        | tee "$report_log"

    exit_cookie_count="$(extract_kv "exit_cookie_count" "$report_log")"
    delta_spaces="$(extract_kv "delta_spaces" "$report_log")"
    delta_tokens="$(extract_kv "delta_tokens" "$report_log")"
    delta_endpoints="$(extract_kv "delta_endpoints" "$report_log")"
    delta_pmm_used_frames="$(extract_kv "delta_pmm_used_frames" "$report_log")"
    ipc_wait_p95_ms="$(extract_kv "ipc_wait_p95_ms" "$report_log")"
    ipc_wait_p99_ms="$(extract_kv "ipc_wait_p99_ms" "$report_log")"
    ipc_scan_avg_steps_x100="$(extract_kv "ipc_scan_avg_steps_x100" "$report_log")"
    shell_ready_s="$(extract_kv "shell_ready_s" "$report_log")"
    noop_spawn_reply_samples="$(extract_kv "noop_spawn_reply_samples" "$report_log")"
    noop_map_elf_reply_samples="$(extract_kv "noop_map_elf_reply_samples" "$report_log")"
    noop_spawn_reply_p95_cycles="$(extract_kv "noop_spawn_reply_p95_cycles" "$report_log")"
    noop_map_elf_reply_p95_cycles="$(extract_kv "noop_map_elf_reply_p95_cycles" "$report_log")"

    echo "${run},${SLO_MODE},${exit_cookie_count:-na},${delta_spaces:-na},${delta_tokens:-na},${delta_endpoints:-na},${delta_pmm_used_frames:-na},${ipc_wait_p95_ms:-na},${ipc_wait_p99_ms:-na},${ipc_scan_avg_steps_x100:-na},${shell_ready_s:-na},${noop_spawn_reply_samples:-na},${noop_map_elf_reply_samples:-na},${noop_spawn_reply_p95_cycles:-na},${noop_map_elf_reply_p95_cycles:-na}" >> "$summary_csv"
done

echo "=== SLO sweep complete ==="
echo "summary_csv=$summary_csv"
