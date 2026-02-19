#!/bin/bash
# Download/update MicroPython source according to external/sources.env

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCES_CONFIG="${PROJECT_ROOT}/external/sources.env"
if [ ! -f "$SOURCES_CONFIG" ]; then
    echo "Error: external source config not found at ${SOURCES_CONFIG}"
    echo "Create/restore it (see external/sources.env in repo)."
    exit 1
fi
# shellcheck disable=SC1090
source "$SOURCES_CONFIG"

MICROPYTHON_VERSION="${CLUU_MICROPYTHON_VERSION:-v1.22.0}"
MICROPYTHON_REPO="${CLUU_MICROPYTHON_REPO:-https://github.com/micropython/micropython.git}"
MICROPYTHON_REF="${CLUU_MICROPYTHON_REF:-$MICROPYTHON_VERSION}"
MICROPYTHON_DIR_REL="${CLUU_MICROPYTHON_DIR:-external/micropython}"
MICROPYTHON_DIR="${PROJECT_ROOT}/${MICROPYTHON_DIR_REL}"

if ! command -v git >/dev/null 2>&1; then
    echo "Error: git is required to fetch MicroPython"
    exit 1
fi

echo "Ensuring MicroPython source..."
echo "  repo: ${MICROPYTHON_REPO}"
echo "  ref:  ${MICROPYTHON_REF}"
echo "  dir:  ${MICROPYTHON_DIR}"

mkdir -p "$(dirname "$MICROPYTHON_DIR")"

if [ -d "$MICROPYTHON_DIR/.git" ]; then
    git -C "$MICROPYTHON_DIR" fetch --tags origin

    if [ -n "$(git -C "$MICROPYTHON_DIR" status --porcelain --untracked-files=no)" ]; then
        echo "Warning: MicroPython repo has local tracked modifications; skipping checkout"
    else
        git -C "$MICROPYTHON_DIR" checkout --detach "$MICROPYTHON_REF"
    fi
elif [ -d "$MICROPYTHON_DIR" ]; then
    echo "Error: ${MICROPYTHON_DIR} exists but is not a git repo"
    echo "Remove it or convert it to a clone of ${MICROPYTHON_REPO}"
    exit 1
else
    git clone "$MICROPYTHON_REPO" "$MICROPYTHON_DIR"
    git -C "$MICROPYTHON_DIR" checkout --detach "$MICROPYTHON_REF"
fi

echo "MicroPython ready at:"
git -C "$MICROPYTHON_DIR" rev-parse --short HEAD
