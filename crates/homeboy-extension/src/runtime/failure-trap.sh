#!/usr/bin/env bash

# Shared failure summary trap for extension runner scripts.
#
# Provides a structured error banner on script exit when a step fails.
# Extensions set FAILED_STEP to the name of the failing step, and optionally
# FAILURE_OUTPUT to captured error output and FAILURE_REPLAY_MODE to control
# whether captured output is replayed.
#
# Usage:
#   source "${HOMEBOY_RUNTIME_FAILURE_TRAP:-/path/to/fallback}"
#   homeboy_init_failure_trap
#
# Then in your script:
#   FAILED_STEP="phpunit"
#   FAILURE_OUTPUT="$captured_stderr"
#   FAILURE_REPLAY_MODE="full"  # or "none" to skip replay

FAILED_STEP=""
FAILURE_OUTPUT=""
FAILURE_REPLAY_MODE="full"

homeboy_print_failure_summary() {
    if [ -n "$FAILED_STEP" ]; then
        echo ""
        echo "============================================"
        echo "BUILD FAILED: $FAILED_STEP"
        echo "============================================"
        if [ "$FAILURE_REPLAY_MODE" = "none" ]; then
            echo ""
            echo "See output above (not replayed)."
        elif [ -n "$FAILURE_OUTPUT" ]; then
            echo ""
            echo "Error details:"
            echo "$FAILURE_OUTPUT"
        fi
    fi
}

homeboy_init_failure_trap() {
    trap homeboy_print_failure_summary EXIT
}
