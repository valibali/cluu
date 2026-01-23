#!/bin/bash
# Build newlib for CLUU
# Run from repository root after downloading newlib

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
NEWLIB_VERSION="4.4.0.20231231"
NEWLIB_SRC="${PROJECT_ROOT}/external/newlib-${NEWLIB_VERSION}"
BUILD_DIR="${PROJECT_ROOT}/target/newlib-build"
SYSROOT="${PROJECT_ROOT}/target/sysroot"

# Check for newlib source
if [ ! -d "$NEWLIB_SRC" ]; then
    echo "Error: Newlib source not found at ${NEWLIB_SRC}"
    echo "Run: ./scripts/download-newlib.sh"
    exit 1
fi

# Check for clang
if ! command -v clang &> /dev/null; then
    echo "Error: clang not found"
    echo "Run: sudo ./scripts/setup-newlib-deps.sh"
    exit 1
fi

echo "Building newlib for CLUU..."
echo "  Source: ${NEWLIB_SRC}"
echo "  Build:  ${BUILD_DIR}"
echo "  Sysroot: ${SYSROOT}"

# Create directories
mkdir -p "${BUILD_DIR}"
mkdir -p "${SYSROOT}/lib"
mkdir -p "${SYSROOT}/include"

cd "${BUILD_DIR}"

# Configure newlib
if [ ! -f "Makefile" ]; then
    echo ""
    echo "Configuring newlib..."
    
    "${NEWLIB_SRC}/configure" \
        --target=x86_64-cluu \
        --prefix="${SYSROOT}" \
        --disable-newlib-supplied-syscalls \
        --enable-newlib-reent-small \
        --enable-lite-exit \
        --disable-multilib \
        --disable-newlib-fvwrite-in-streamio \
        --disable-newlib-fseek-optimization \
        --disable-newlib-wide-orient \
        --disable-newlib-unbuf-stream-opt \
        --enable-newlib-global-atexit \
        --enable-newlib-nano-malloc \
        CC_FOR_TARGET="clang --target=x86_64-unknown-none-elf -ffreestanding -fno-stack-protector" \
        AR_FOR_TARGET="llvm-ar" \
        RANLIB_FOR_TARGET="llvm-ranlib" \
        CFLAGS_FOR_TARGET="-O2 -g -nostdlib -fno-builtin"
fi

# Build newlib
echo ""
echo "Building newlib (this may take a while)..."
make -j$(nproc) all

# Install to sysroot
echo ""
echo "Installing to sysroot..."
make install

echo ""
echo "Newlib built and installed to ${SYSROOT}"
echo ""
echo "Contents:"
ls -la "${SYSROOT}/lib/"*.a 2>/dev/null | head -10 || echo "  (libraries in x86_64-cluu/lib/)"
ls -la "${SYSROOT}/x86_64-cluu/lib/"*.a 2>/dev/null | head -10 || true

echo ""
echo "Next steps:"
echo "  1. Build libcluu_syscalls: cargo xtask build-syscalls"
echo "  2. Assemble crt0.o: cargo xtask build-crt0"
echo "  3. Build a C program: cargo xtask build-c <name> <source.c>"
