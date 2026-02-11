#!/bin/bash
set -euo pipefail

LOG_FILE=""
MIN_EXIT_COOKIES="${MIN_EXIT_COOKIES:-0}"
MAX_DELTA_SPACES="${MAX_DELTA_SPACES:-}"
MAX_DELTA_TOKENS="${MAX_DELTA_TOKENS:-}"
MAX_DELTA_ENDPOINTS="${MAX_DELTA_ENDPOINTS:-}"
MAX_DELTA_PMM_USED_FRAMES="${MAX_DELTA_PMM_USED_FRAMES:-}"
MAX_IPC_WAIT_P95_MS="${MAX_IPC_WAIT_P95_MS:-}"
MAX_IPC_WAIT_P99_MS="${MAX_IPC_WAIT_P99_MS:-}"
MAX_IPC_SCAN_AVG_STEPS_X100="${MAX_IPC_SCAN_AVG_STEPS_X100:-}"
MAX_SHELL_READY_S="${MAX_SHELL_READY_S:-15}"

usage() {
    cat <<'EOF'
Usage: scripts/harness_slo_report.sh --log PATH [threshold options]

Threshold options can be provided either as CLI args or env vars:
  --min-exit-cookies N
  --max-delta-spaces N
  --max-delta-tokens N
  --max-delta-endpoints N
  --max-delta-pmm-used-frames N
  --max-ipc-wait-p95-ms N
  --max-ipc-wait-p99-ms N
  --max-ipc-scan-avg-steps-x100 N
  --max-shell-ready-s N
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --log)
            LOG_FILE="${2:-}"
            shift 2
            ;;
        --min-exit-cookies)
            MIN_EXIT_COOKIES="${2:-}"
            shift 2
            ;;
        --max-delta-spaces)
            MAX_DELTA_SPACES="${2:-}"
            shift 2
            ;;
        --max-delta-tokens)
            MAX_DELTA_TOKENS="${2:-}"
            shift 2
            ;;
        --max-delta-endpoints)
            MAX_DELTA_ENDPOINTS="${2:-}"
            shift 2
            ;;
        --max-delta-pmm-used-frames)
            MAX_DELTA_PMM_USED_FRAMES="${2:-}"
            shift 2
            ;;
        --max-ipc-wait-p95-ms)
            MAX_IPC_WAIT_P95_MS="${2:-}"
            shift 2
            ;;
        --max-ipc-wait-p99-ms)
            MAX_IPC_WAIT_P99_MS="${2:-}"
            shift 2
            ;;
        --max-ipc-scan-avg-steps-x100)
            MAX_IPC_SCAN_AVG_STEPS_X100="${2:-}"
            shift 2
            ;;
        --max-shell-ready-s)
            MAX_SHELL_READY_S="${2:-}"
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

if [[ -z "$LOG_FILE" ]]; then
    echo "ERROR: --log is required"
    exit 1
fi
if [[ ! -f "$LOG_FILE" ]]; then
    echo "ERROR: log file not found: $LOG_FILE"
    exit 1
fi

parse_last_numeric_after_marker() {
    local marker="$1"
    awk -v marker="$marker" '
        $0 ~ marker {
            if (getline > 0) {
                v = $0
                gsub(/[^0-9-]/, "", v)
                if (v != "") {
                    last = v
                }
            }
        }
        END {
            if (last != "") {
                print last
            }
        }
    ' "$LOG_FILE"
}

parse_last_inline_numeric_value() {
    local marker="$1"
    awk -v marker="$marker" '
        index($0, marker) > 0 {
            line = $0
            sub(/^.*=/, "", line)
            gsub(/[^0-9-]/, "", line)
            if (line != "") {
                last = line
            }
        }
        END {
            if (last != "") {
                print last
            }
        }
    ' "$LOG_FILE"
}

check_upper_bound() {
    local value="$1"
    local limit="$2"
    local label="$3"
    if [[ -z "$limit" ]]; then
        return 0
    fi
    if ! [[ "$limit" =~ ^-?[0-9]+$ ]]; then
        echo "ERROR: $label limit must be an integer, got '$limit'"
        exit 1
    fi
    if [[ -z "$value" ]]; then
        echo "SLO FAIL: missing metric '$label'"
        return 1
    fi
    if (( value > limit )); then
        echo "SLO FAIL: $label value=$value limit=$limit"
        return 1
    fi
    return 0
}

exit_cookie_count="$(grep -c "procmgr: exit cookie" "$LOG_FILE" || true)"
last_delta_spaces="$(parse_last_numeric_after_marker "delta_spaces=")"
last_delta_tokens="$(parse_last_numeric_after_marker "delta_tokens=")"
last_delta_endpoints="$(parse_last_numeric_after_marker "delta_endpoints=")"
last_delta_pmm_frames="$(parse_last_numeric_after_marker "delta_pmm_used_frames=")"
last_ipc_wait_p95_ms="$(parse_last_numeric_after_marker "ipc_wait_p95_ms=")"
last_ipc_wait_p99_ms="$(parse_last_numeric_after_marker "ipc_wait_p99_ms=")"
last_ipc_scan_avg_steps_x100="$(parse_last_numeric_after_marker "ipc_scan_avg_steps_x100=")"
last_shell_ready_s="$(parse_last_inline_numeric_value "HARNESS shell_ready_s=")"

echo "=== Harness SLO Summary ==="
echo "log=$LOG_FILE"
echo "exit_cookie_count=$exit_cookie_count"
echo "delta_spaces=${last_delta_spaces:-na}"
echo "delta_tokens=${last_delta_tokens:-na}"
echo "delta_endpoints=${last_delta_endpoints:-na}"
echo "delta_pmm_used_frames=${last_delta_pmm_frames:-na}"
echo "ipc_wait_p95_ms=${last_ipc_wait_p95_ms:-na}"
echo "ipc_wait_p99_ms=${last_ipc_wait_p99_ms:-na}"
echo "ipc_scan_avg_steps_x100=${last_ipc_scan_avg_steps_x100:-na}"
echo "shell_ready_s=${last_shell_ready_s:-na}"

failed=0
if ! [[ "$MIN_EXIT_COOKIES" =~ ^[0-9]+$ ]]; then
    echo "ERROR: MIN_EXIT_COOKIES must be a non-negative integer, got '$MIN_EXIT_COOKIES'"
    exit 1
fi
if (( exit_cookie_count < MIN_EXIT_COOKIES )); then
    echo "SLO FAIL: exit_cookie_count value=$exit_cookie_count minimum=$MIN_EXIT_COOKIES"
    failed=1
fi

check_upper_bound "${last_delta_spaces:-}" "$MAX_DELTA_SPACES" "delta_spaces" || failed=1
check_upper_bound "${last_delta_tokens:-}" "$MAX_DELTA_TOKENS" "delta_tokens" || failed=1
check_upper_bound "${last_delta_endpoints:-}" "$MAX_DELTA_ENDPOINTS" "delta_endpoints" || failed=1
check_upper_bound "${last_delta_pmm_frames:-}" "$MAX_DELTA_PMM_USED_FRAMES" "delta_pmm_used_frames" || failed=1
check_upper_bound "${last_ipc_wait_p95_ms:-}" "$MAX_IPC_WAIT_P95_MS" "ipc_wait_p95_ms" || failed=1
check_upper_bound "${last_ipc_wait_p99_ms:-}" "$MAX_IPC_WAIT_P99_MS" "ipc_wait_p99_ms" || failed=1
check_upper_bound "${last_ipc_scan_avg_steps_x100:-}" "$MAX_IPC_SCAN_AVG_STEPS_X100" "ipc_scan_avg_steps_x100" || failed=1
check_upper_bound "${last_shell_ready_s:-}" "$MAX_SHELL_READY_S" "shell_ready_s" || failed=1

if [[ "$failed" -ne 0 ]]; then
    exit 1
fi

echo "SLO PASS"
