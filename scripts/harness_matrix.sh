#!/bin/bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

RUN_BUILD_ONCE=1
if [[ "${1:-}" == "--no-build" ]]; then
    RUN_BUILD_ONCE=0
fi

run_case() {
    local name="$1"
    shift
    local build_flag="$1"
    shift

    echo "=== Harness case: ${name} ==="
    if [[ "$build_flag" == "full" ]]; then
        "$@"
    else
        "$@" --no-build
    fi
    echo "=== Harness case PASS: ${name} ==="
}

if [[ "$RUN_BUILD_ONCE" -eq 1 ]]; then
    run_case "m1_recv" full env MARKER_MODE=m1_recv TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh
    run_case "m2_token_audit" no_build env MARKER_MODE=m2_token_audit TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh
    run_case "m2_leakdiag" no_build env MARKER_MODE=m2_leakdiag TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 MAX_DELTA_SPACES=14 MAX_DELTA_TOKENS=180 MAX_DELTA_ENDPOINTS=80 MAX_DELTA_PMM_USED_FRAMES=46000 ./test_hello.sh
    run_case "m3_mapfail" no_build env MARKER_MODE=m3_mapfail TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m3_mapcopyfail" no_build env MARKER_MODE=m3_mapcopyfail TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m3_maperror" no_build env MARKER_MODE=m3_maperror TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m4_sender_auth" no_build env MARKER_MODE=m4_sender_auth TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m4_registry_sender_auth" no_build env MARKER_MODE=m4_registry_sender_auth TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m4_notify_lifecycle" no_build env MARKER_MODE=m4_notify_lifecycle TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m4_deny_paths" full env MARKER_MODE=m4_deny_paths TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "m4_registry_deny_paths" full env MARKER_MODE=m4_registry_deny_paths TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_owner_deny" full env MARKER_MODE=l2_owner_deny TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_sigint" full env MARKER_MODE=l2_sigint TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_jobs" full env MARKER_MODE=l2_jobs TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_fg" full env MARKER_MODE=l2_fg TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_stop" full env MARKER_MODE=l2_stop TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_jobchurn" full env MARKER_MODE=l2_jobchurn TEST_COMMAND_REPEAT=1 RUN_WAIT=16 ./test_hello.sh
    run_case "l2_jobchurn_heavy" full env MARKER_MODE=l2_jobchurn_heavy TEST_COMMAND_REPEAT=1 RUN_WAIT=36 ./test_hello.sh
    run_case "l2_jobmix" full env MARKER_MODE=l2_jobmix TEST_COMMAND_REPEAT=1 RUN_WAIT=24 ./test_hello.sh
    run_case "l2_waitpid" full env MARKER_MODE=l2_waitpid TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_mmap" full env MARKER_MODE=l2_mmap TEST_COMMAND_REPEAT=1 RUN_WAIT=16 ./test_hello.sh
    run_case "m5_fairness" full env MARKER_MODE=m5_fairness TEST_COMMAND_REPEAT=1 RUN_WAIT=16 MIN_EXIT_COOKIES=6 MAX_IPC_WAIT_P95_MS=16 MAX_IPC_WAIT_P99_MS=16 MAX_IPC_SCAN_AVG_STEPS_X100=250 ./test_hello.sh
else
    run_case "m1_recv" no_build env MARKER_MODE=m1_recv TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh
    run_case "m2_token_audit" no_build env MARKER_MODE=m2_token_audit TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh
    run_case "m2_leakdiag" no_build env MARKER_MODE=m2_leakdiag TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 MAX_DELTA_SPACES=14 MAX_DELTA_TOKENS=180 MAX_DELTA_ENDPOINTS=80 MAX_DELTA_PMM_USED_FRAMES=46000 ./test_hello.sh
    run_case "m3_mapfail" no_build env MARKER_MODE=m3_mapfail TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m3_mapcopyfail" no_build env MARKER_MODE=m3_mapcopyfail TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m3_maperror" no_build env MARKER_MODE=m3_maperror TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m4_sender_auth" no_build env MARKER_MODE=m4_sender_auth TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m4_registry_sender_auth" no_build env MARKER_MODE=m4_registry_sender_auth TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m4_notify_lifecycle" no_build env MARKER_MODE=m4_notify_lifecycle TEST_COMMAND_REPEAT=1 ./test_hello.sh
    run_case "m4_deny_paths" full env MARKER_MODE=m4_deny_paths TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "m4_registry_deny_paths" full env MARKER_MODE=m4_registry_deny_paths TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_owner_deny" full env MARKER_MODE=l2_owner_deny TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_sigint" full env MARKER_MODE=l2_sigint TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_jobs" full env MARKER_MODE=l2_jobs TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_fg" full env MARKER_MODE=l2_fg TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_stop" full env MARKER_MODE=l2_stop TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_jobchurn" full env MARKER_MODE=l2_jobchurn TEST_COMMAND_REPEAT=1 RUN_WAIT=16 ./test_hello.sh
    run_case "l2_jobchurn_heavy" full env MARKER_MODE=l2_jobchurn_heavy TEST_COMMAND_REPEAT=1 RUN_WAIT=36 ./test_hello.sh
    run_case "l2_jobmix" full env MARKER_MODE=l2_jobmix TEST_COMMAND_REPEAT=1 RUN_WAIT=24 ./test_hello.sh
    run_case "l2_waitpid" full env MARKER_MODE=l2_waitpid TEST_COMMAND_REPEAT=1 RUN_WAIT=12 ./test_hello.sh
    run_case "l2_mmap" full env MARKER_MODE=l2_mmap TEST_COMMAND_REPEAT=1 RUN_WAIT=16 ./test_hello.sh
    run_case "m5_fairness" full env MARKER_MODE=m5_fairness TEST_COMMAND_REPEAT=1 RUN_WAIT=16 MIN_EXIT_COOKIES=6 MAX_IPC_WAIT_P95_MS=16 MAX_IPC_WAIT_P99_MS=16 MAX_IPC_SCAN_AVG_STEPS_X100=250 ./test_hello.sh
fi

echo "All harness matrix cases passed."
