#!/bin/bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

REPEATS="${REPEATS:-5}"
RUN_WAIT="${RUN_WAIT:-16}"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/tmp/harness_slo}"
NO_BUILD=0
QEMU_GDB="${QEMU_GDB:-0}"

usage() {
    cat <<'EOF'
Usage: scripts/harness_slo_sweep.sh [--no-build] [--repeats N] [--out-dir PATH]

Runs fairness mode repeatedly and emits per-run SLO summaries.
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
echo "run,exit_cookie_count,delta_spaces,delta_tokens,delta_endpoints,delta_pmm_used_frames,ipc_wait_p95_ms,ipc_wait_p99_ms,ipc_scan_avg_steps_x100" > "$summary_csv"

extract_kv() {
    local key="$1"
    local file="$2"
    awk -F'=' -v key="$key" '$1 == key { print $2; exit 0 }' "$file"
}

for ((run = 1; run <= REPEATS; run++)); do
    serial_log="$OUT_DIR/serial_run_${run}.log"
    report_log="$OUT_DIR/slo_run_${run}.txt"

    echo "=== SLO sweep run ${run}/${REPEATS} ==="
    env \
        SERIAL_LOG="$serial_log" \
        MARKER_MODE=m5_fairness \
        TEST_COMMAND_REPEAT=1 \
        RUN_WAIT="$RUN_WAIT" \
        MIN_EXIT_COOKIES=6 \
        QEMU_GDB="$QEMU_GDB" \
        MAX_IPC_WAIT_P95_MS="${MAX_IPC_WAIT_P95_MS:-}" \
        MAX_IPC_WAIT_P99_MS="${MAX_IPC_WAIT_P99_MS:-}" \
        MAX_IPC_SCAN_AVG_STEPS_X100="${MAX_IPC_SCAN_AVG_STEPS_X100:-}" \
        ./test_hello.sh $([[ "$NO_BUILD" -eq 1 ]] && echo "--no-build")

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
        | tee "$report_log"

    exit_cookie_count="$(extract_kv "exit_cookie_count" "$report_log")"
    delta_spaces="$(extract_kv "delta_spaces" "$report_log")"
    delta_tokens="$(extract_kv "delta_tokens" "$report_log")"
    delta_endpoints="$(extract_kv "delta_endpoints" "$report_log")"
    delta_pmm_used_frames="$(extract_kv "delta_pmm_used_frames" "$report_log")"
    ipc_wait_p95_ms="$(extract_kv "ipc_wait_p95_ms" "$report_log")"
    ipc_wait_p99_ms="$(extract_kv "ipc_wait_p99_ms" "$report_log")"
    ipc_scan_avg_steps_x100="$(extract_kv "ipc_scan_avg_steps_x100" "$report_log")"

    echo "${run},${exit_cookie_count:-na},${delta_spaces:-na},${delta_tokens:-na},${delta_endpoints:-na},${delta_pmm_used_frames:-na},${ipc_wait_p95_ms:-na},${ipc_wait_p99_ms:-na},${ipc_scan_avg_steps_x100:-na}" >> "$summary_csv"
done

echo "=== SLO sweep complete ==="
echo "summary_csv=$summary_csv"
