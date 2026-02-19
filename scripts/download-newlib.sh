#!/bin/bash
# Download and extract newlib source
# Run from repository root

set -euo pipefail

DOWNLOAD_DIR="external"

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SOURCES_CONFIG="${PROJECT_ROOT}/external/sources.env"
if [ ! -f "$SOURCES_CONFIG" ]; then
    echo "Error: external source config not found at ${SOURCES_CONFIG}"
    echo "Create/restore it (see external/sources.env in repo)."
    exit 1
fi
# shellcheck disable=SC1090
source "$SOURCES_CONFIG"

NEWLIB_VERSION="${CLUU_NEWLIB_VERSION:-4.4.0.20231231}"
NEWLIB_URL="${CLUU_NEWLIB_URL:-ftp://sourceware.org/pub/newlib/newlib-${NEWLIB_VERSION}.tar.gz}"
NEWLIB_DIR="${CLUU_NEWLIB_DIR:-external/newlib-${NEWLIB_VERSION}}"
NEWLIB_DIR_NAME="$(basename "$NEWLIB_DIR")"
NEWLIB_TARBALL_NAME="$(basename "$NEWLIB_URL")"
NEWLIB_CONFIG_SUB_PATH="${PROJECT_ROOT}/${DOWNLOAD_DIR}/${NEWLIB_DIR_NAME}/config.sub"

cd "$PROJECT_ROOT"

echo "Downloading newlib ${NEWLIB_VERSION}..."

mkdir -p "${DOWNLOAD_DIR}"
cd "${DOWNLOAD_DIR}"

apply_cluu_newlib_patch() {
    if [ ! -f "$NEWLIB_CONFIG_SUB_PATH" ]; then
        echo "Error: config.sub not found at ${NEWLIB_CONFIG_SUB_PATH}"
        exit 1
    fi

    if grep -q 'cluu\*)' "$NEWLIB_CONFIG_SUB_PATH"; then
        echo "Newlib patch already present: config.sub recognizes cluu target"
        return
    fi

    sed -i '0,/| emx\*)/s//| emx* | cluu*)/' "$NEWLIB_CONFIG_SUB_PATH"

    if ! grep -q 'cluu\*)' "$NEWLIB_CONFIG_SUB_PATH"; then
        echo "Error: failed to apply CLUU patch to newlib config.sub"
        exit 1
    fi

    echo "Applied CLUU patch: config.sub recognizes cluu target"
}

if [ -d "${NEWLIB_DIR_NAME}" ]; then
    echo "Newlib already downloaded at ${DOWNLOAD_DIR}/${NEWLIB_DIR_NAME}"
    apply_cluu_newlib_patch
    exit 0
fi

if [ ! -f "${NEWLIB_TARBALL_NAME}" ]; then
    wget "${NEWLIB_URL}" || curl -O "${NEWLIB_URL}"
fi

echo "Extracting..."
tar xzf "${NEWLIB_TARBALL_NAME}"
apply_cluu_newlib_patch

echo ""
echo "Newlib ${NEWLIB_VERSION} extracted to ${DOWNLOAD_DIR}/${NEWLIB_DIR_NAME}"
echo ""
echo "Next step: cargo xtask build-newlib"
