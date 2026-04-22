#!/bin/bash
set -uo pipefail

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

# shellcheck source=scripts/harness_case_defaults.sh
. "$ROOT_DIR/scripts/harness_case_defaults.sh"

# Extract the effective values of the build-affecting env vars from an
# assignments string like 'MARKER_MODE=foo CLUU_SHELL_AUTOSTART_CMD="spawn x"'.
# The two things that actually flow into build artifacts are:
#   - CLUU_SHELL_AUTOSTART_CMD: option_env! in procmgr (rerun-if-env-changed)
#     — note that harness_run.sh also derives a default from MARKER_MODE when
#     the caller didn't set one explicitly, so we run the same derivation here
#   - CLUU_BOOTBOOT_ENV:        baked into target/bootboot_config at build time
# Prints two lines: the effective value of each var (empty if unset).
build_signature() {
    local assignments="$1"
    (
        set +u
        unset CLUU_SHELL_AUTOSTART_CMD CLUU_BOOTBOOT_ENV
        eval "$assignments"
        # Run the same MARKER_MODE default-derivation that harness_run.sh does
        # so we compute the *effective* CLUU_SHELL_AUTOSTART_CMD.
        TEST_COMMAND="__AUTO__"
        harness_derive_marker_defaults
        local effective_autostart="${CLUU_SHELL_AUTOSTART_CMD:-$SHELL_AUTOSTART_CMD_DEFAULT}"
        printf '%s\n%s\n' \
            "$effective_autostart" \
            "${CLUU_BOOTBOOT_ENV-}"
    )
}

# Tracks the build signature of the most recent case that actually built.
# Empty while no build has happened yet.
LAST_BUILD_SIG=""
BUILT_ONCE=0

run_case() {
    local name="$1"
    local build_mode="$2"
    local env_assignments="$3"
    local effective_mode="$build_mode"

    # Clear stale compile-time autostart from previous cases
    unset CLUU_SHELL_AUTOSTART_CMD

    if [[ "$FORCE_NO_BUILD" -eq 1 ]]; then
        effective_mode="no_build"
    elif [[ "$effective_mode" == "full" && "$BUILT_ONCE" -eq 1 ]]; then
        local sig
        sig="$(build_signature "$env_assignments")"
        if [[ "$sig" == "$LAST_BUILD_SIG" ]]; then
            effective_mode="no_build"
            echo "    (reusing last build — CLUU_SHELL_AUTOSTART_CMD / CLUU_BOOTBOOT_ENV unchanged)"
        fi
    fi

    echo "=== Harness case: ${name} ==="
    if [[ "$effective_mode" == "no_build" ]]; then
        eval "$env_assignments ./scripts/harness_run.sh --no-build"
    else
        eval "$env_assignments ./scripts/harness_run.sh"
    fi
    local rc=$?
    if [[ "$effective_mode" == "full" && $rc -eq 0 ]]; then
        LAST_BUILD_SIG="$(build_signature "$env_assignments")"
        BUILT_ONCE=1
    fi
    if [[ $rc -eq 0 ]]; then
        echo "=== Harness case PASS: ${name} ==="
    else
        echo "=== Harness case FAIL: ${name} ==="
    fi
    return $rc
}

ran_any=0
passed=0
failed=0
failed_cases=()

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

    if run_case "$name" "$build_mode" "$env_assignments"; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        failed_cases+=("$name")
    fi
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

echo ""
echo "=== SUITE SUMMARY: ${passed} passed, ${failed} failed ==="

if [[ "$failed" -gt 0 ]]; then
    echo "Failed cases:"
    for c in "${failed_cases[@]}"; do
        echo "  - $c"
    done
    exit 1
fi

echo "All harness suite cases passed."
