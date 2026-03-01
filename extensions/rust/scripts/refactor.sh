#!/usr/bin/env bash
# Rust refactor script — delegates to Python parser.
# Receives JSON command on stdin, outputs JSON result on stdout.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
exec python3 "${SCRIPT_DIR}/refactor.py"
