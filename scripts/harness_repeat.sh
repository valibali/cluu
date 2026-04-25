#!/bin/bash
# Run a single harness case N times, report the pass/fail tally.
#
# Usage: bash scripts/harness_repeat.sh <CASE> <N> [extra-env=val ...]
#
# Example:
#   bash scripts/harness_repeat.sh l2_rm 10 RUN_WAIT=45
#
# Exit code: 0 if all N runs passed, 1 otherwise.

set -u

CASE=${1:?usage: harness_repeat.sh <case> <n> [extra-env...]}
N=${2:?usage: harness_repeat.sh <case> <n> [extra-env...]}
shift 2

PASS=0
FAIL=0
FAILED_RUNS=()

for i in $(seq 1 "$N"); do
    output=$(env "$@" MARKER_MODE="$CASE" bash scripts/harness_run.sh 2>&1)
    if echo "$output" | grep -q "No faults detected and all required markers found"; then
        PASS=$((PASS + 1))
        echo "Run $i: PASS"
    else
        FAIL=$((FAIL + 1))
        FAILED_RUNS+=("$i")
        echo "Run $i: FAIL"
    fi
done

echo "==================================="
echo "$CASE: $PASS/$N passed"
if [ $FAIL -gt 0 ]; then
    echo "Failed runs: ${FAILED_RUNS[*]}"
    exit 1
fi
exit 0
