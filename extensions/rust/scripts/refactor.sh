#!/usr/bin/env bash
# Rust refactor script — delegates to Python parser package.
# Receives JSON command on stdin, outputs JSON result on stdout.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PYTHONPATH="${SCRIPT_DIR}" exec python3 -m refactor
