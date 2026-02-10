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
    run_case "m2_leakdiag" no_build env MARKER_MODE=m2_leakdiag TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh
    run_case "m3_mapfail" no_build env MARKER_MODE=m3_mapfail TEST_COMMAND_REPEAT=1 ./test_hello.sh
else
    run_case "m1_recv" no_build env MARKER_MODE=m1_recv TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh
    run_case "m2_token_audit" no_build env MARKER_MODE=m2_token_audit TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh
    run_case "m2_leakdiag" no_build env MARKER_MODE=m2_leakdiag TEST_COMMAND_REPEAT=3 MIN_EXIT_COOKIES=3 ./test_hello.sh
    run_case "m3_mapfail" no_build env MARKER_MODE=m3_mapfail TEST_COMMAND_REPEAT=1 ./test_hello.sh
fi

echo "All harness matrix cases passed."
