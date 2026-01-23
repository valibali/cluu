#!/bin/bash
# Setup script for newlib build dependencies
# Run with: sudo ./scripts/setup-newlib-deps.sh

set -e

echo "Installing newlib build dependencies..."

# Install clang, lld, and texinfo
apt-get update
apt-get install -y clang lld texinfo

echo "Dependencies installed successfully!"
echo ""
echo "Installed versions:"
clang --version | head -1
ld.lld --version | head -1
makeinfo --version | head -1

echo ""
echo "Next steps:"
echo "  1. Download newlib: ./scripts/download-newlib.sh"
echo "  2. Build newlib: cargo xtask build-newlib"
