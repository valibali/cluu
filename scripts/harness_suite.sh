#!/bin/bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

CASES_FILE="${CASES_FILE:-$ROOT_DIR/scripts/harness_cases.conf}"
FORCE_NO_BUILD=0
ONLY_CASE="${ONLY_CASE:-}"

usage() {
    cat <<'EOF'
Usage: scripts/harness_suite.sh [--no-build] [--case NAME] [--list]

Options:
  --no-build    Force all cases to reuse existing build artifacts
  --case NAME   Run only one named case
  --list        List configured case names
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build)
            FORCE_NO_BUILD=1
            shift
            ;;
        --case)
            ONLY_CASE="${2:-}"
            if [[ -z "$ONLY_CASE" ]]; then
                echo "ERROR: --case requires a value"
                exit 1
            fi
            shift 2
            ;;
        --list)
            awk -F'|' '
                /^[[:space:]]*#/ { next }
                /^[[:space:]]*$/ { next }
                { print $1 }
            ' "$CASES_FILE"
            exit 0
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

if [[ ! -f "$CASES_FILE" ]]; then
    echo "ERROR: cases file not found: $CASES_FILE"
    exit 1
fi

run_case() {
    local name="$1"
    local build_mode="$2"
    local env_assignments="$3"
    local effective_mode="$build_mode"

    if [[ "$FORCE_NO_BUILD" -eq 1 ]]; then
        effective_mode="no_build"
    fi

    echo "=== Harness case: ${name} ==="
    if [[ "$effective_mode" == "no_build" ]]; then
        # shellcheck disable=SC2086
        env $env_assignments ./scripts/harness_run.sh --no-build
    else
        # shellcheck disable=SC2086
        env $env_assignments ./scripts/harness_run.sh
    fi
    echo "=== Harness case PASS: ${name} ==="
}

ran_any=0
while IFS='|' read -r name build_mode env_assignments; do
    [[ -z "${name// }" ]] && continue
    [[ "${name:0:1}" == "#" ]] && continue

    if [[ -n "$ONLY_CASE" && "$name" != "$ONLY_CASE" ]]; then
        continue
    fi

    if [[ "$build_mode" != "full" && "$build_mode" != "no_build" ]]; then
        echo "ERROR: invalid build mode '$build_mode' for case '$name'"
        exit 1
    fi

    run_case "$name" "$build_mode" "$env_assignments"
    ran_any=1
done < "$CASES_FILE"

if [[ "$ran_any" -eq 0 ]]; then
    if [[ -n "$ONLY_CASE" ]]; then
        echo "ERROR: case not found: $ONLY_CASE"
    else
        echo "ERROR: no runnable harness cases found in $CASES_FILE"
    fi
    exit 1
fi

echo "All harness suite cases passed."
