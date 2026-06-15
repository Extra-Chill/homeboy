#!/bin/bash
set -euo pipefail

echo "Setting up Swift test infrastructure..."

if ! command -v swift &> /dev/null; then
    echo "Swift unavailable; Swift extension installed but not ready on this runner. Install Xcode or Swift toolchain to enable Swift workflows."
    exit 0
fi

SWIFT_VERSION=$(swift --version 2>&1 | head -1)
echo "Swift found: $SWIFT_VERSION"

echo "Swift test infrastructure ready"
