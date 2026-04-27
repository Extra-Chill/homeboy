#!/usr/bin/env bash
set -euo pipefail

# Test runner router for WordPress Homeboy extension.
#
# Plugin/theme tests run inside WordPress Playground. Core-dev checkouts
# (wordpress-develop) dispatch to WordPress core's native PHPUnit runner.

# Bash 4.0+ required — lint-runner.sh (called during test runs) uses
# associative arrays which are bash 4+ only. Fail early with a clear
# message rather than producing misleading cascading errors.
if ((BASH_VERSINFO[0] < 4)); then
    echo "============================================" >&2
    echo "ERROR: bash 4.0+ required (found ${BASH_VERSION})" >&2
    echo "============================================" >&2
    case "$(uname -s)" in
        Darwin)
            echo "macOS ships bash 3.2. Fix: brew install bash" >&2
            echo "Then restart your terminal (Homebrew bash takes priority on PATH)." >&2
            ;;
        *)
            echo "Update bash via your package manager." >&2
            ;;
    esac
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Resolve execution context and export env vars that the Playground runner expects.
if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
    # Called through Homeboy extension system — env vars already set by core.
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
else
    # Called directly (e.g., from composer test in component directory).
    EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
    export HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH"

    COMPONENT_PATH="$(pwd)"
    export HOMEBOY_COMPONENT_ID="$(basename "$COMPONENT_PATH")"
    export HOMEBOY_COMPONENT_PATH="$COMPONENT_PATH"

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Direct execution context (component: $(basename "$COMPONENT_PATH"))"
    fi
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Extension path: $EXTENSION_PATH"
    echo "DEBUG: Component: ${HOMEBOY_COMPONENT_ID:-none}"
    echo "DEBUG: Component path: ${HOMEBOY_COMPONENT_PATH:-$(pwd)}"
fi

COMPONENT_SHAPE="${HOMEBOY_COMPONENT_SHAPE:-}"
if [ -z "$COMPONENT_SHAPE" ]; then
    DETECT_COMPONENT_HELPER="${HOMEBOY_RUNTIME_DETECT_COMPONENT:-${SCRIPT_DIR}/../lib/detect-component.sh}"
    # shellcheck source=../lib/detect-component.sh
    source "${DETECT_COMPONENT_HELPER}"
    if homeboy_detect_component "${HOMEBOY_COMPONENT_PATH:-$(pwd)}"; then
        COMPONENT_SHAPE="$HOMEBOY_COMPONENT_TYPE"
    fi
fi

case "$COMPONENT_SHAPE" in
    core-dev)
        exec bash "${SCRIPT_DIR}/test-runner-core-dev.sh" "$@"
        ;;
    *)
        exec bash "${SCRIPT_DIR}/test-runner-playground.sh" "$@"
        ;;
esac
