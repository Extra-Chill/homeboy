#!/bin/bash
set -euo pipefail

echo "Setting up Swift test infrastructure..."

# Verify Swift is available
if ! command -v swift &> /dev/null; then
    echo "Error: Swift not found. Please install Xcode or Swift toolchain."
    exit 1
fi

SWIFT_VERSION=$(swift --version 2>&1 | head -1)
echo "Swift found: $SWIFT_VERSION"

echo "Swift test infrastructure ready"
