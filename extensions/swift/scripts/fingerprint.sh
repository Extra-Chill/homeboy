#!/usr/bin/env bash
# Swift fingerprint script for homeboy audit.
#
# Input (JSON on stdin):
#   {"file_path": "App/Model.swift", "content": "..."}
#
# Output (JSON on stdout): structural symbols used by homeboy audit.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 "$SCRIPT_DIR/fingerprint.py"
