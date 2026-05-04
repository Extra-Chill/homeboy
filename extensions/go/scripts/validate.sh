#!/usr/bin/env bash
set -euo pipefail

# Go validator for homeboy's post-write validation gate.

if [ ! -f ./go.mod ]; then
    echo "No go.mod found — skipping validation"
    exit 0
fi

go vet ./... 2>&1
