#!/usr/bin/env bash
# Agent Hooks dispatcher
# Routes to appropriate script based on command argument

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

case "${1:-}" in
    uninstall)
        exec bash "$SCRIPT_DIR/uninstall.sh"
        ;;
    *)
        exec bash "$SCRIPT_DIR/setup.sh"
        ;;
esac
