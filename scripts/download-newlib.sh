#!/bin/bash
# Download and extract newlib source
# Run from repository root

set -e

NEWLIB_VERSION="4.4.0.20231231"
NEWLIB_URL="ftp://sourceware.org/pub/newlib/newlib-${NEWLIB_VERSION}.tar.gz"
NEWLIB_DIR="newlib-${NEWLIB_VERSION}"
DOWNLOAD_DIR="external"

cd "$(dirname "$0")/.."

echo "Downloading newlib ${NEWLIB_VERSION}..."

mkdir -p "${DOWNLOAD_DIR}"
cd "${DOWNLOAD_DIR}"

if [ -d "${NEWLIB_DIR}" ]; then
    echo "Newlib already downloaded at ${DOWNLOAD_DIR}/${NEWLIB_DIR}"
    exit 0
fi

if [ ! -f "newlib-${NEWLIB_VERSION}.tar.gz" ]; then
    wget "${NEWLIB_URL}" || curl -O "${NEWLIB_URL}"
fi

echo "Extracting..."
tar xzf "newlib-${NEWLIB_VERSION}.tar.gz"

echo ""
echo "Newlib ${NEWLIB_VERSION} extracted to ${DOWNLOAD_DIR}/${NEWLIB_DIR}"
echo ""
echo "Next step: cargo xtask build-newlib"
