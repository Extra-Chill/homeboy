#!/usr/bin/env bash
set -euo pipefail

# Go formatter for homeboy's post-write formatting gate.

if [ ! -f ./go.mod ]; then
    echo "No go.mod found — skipping format"
    exit 0
fi

go fmt ./... 2>&1
