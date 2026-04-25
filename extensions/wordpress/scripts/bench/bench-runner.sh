#!/usr/bin/env bash
set -euo pipefail

# Bench runner router for WordPress Homeboy extension.
#
# All bench execution runs inside WordPress Playground (PHP-WASM + embedded
# SQLite), reusing the same shared bootstrap stages as the test runner
# (`scripts/lib/playground-bootstrap.php`). That shared boot path is the
# whole point: bench numbers from one runner kind only mean something
# relative to numbers from another if both were measured against the same
# `boot` and `install` code.
#
# This entry script resolves execution context and dispatches to the
# Playground bench runner. Bash 4.0+ required (matches test-runner.sh's
# gate so bench inherits the same surface).

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

# Resolve execution context. When invoked through homeboy core the env
# vars are pre-set; when invoked directly (e.g. via composer or
# `bash scripts/bench/bench-runner.sh`) we synthesize them from CWD so the
# Playground runner can mount the right paths.
if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
else
    EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
    export HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH"

    COMPONENT_PATH="$(pwd)"
    export HOMEBOY_COMPONENT_ID="$(basename "$COMPONENT_PATH")"
    export HOMEBOY_COMPONENT_PATH="$COMPONENT_PATH"

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: [bench] Direct execution context (component: $(basename "$COMPONENT_PATH"))"
    fi
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [bench] Extension path: $EXTENSION_PATH"
    echo "DEBUG: [bench] Component: ${HOMEBOY_COMPONENT_ID:-none}"
    echo "DEBUG: [bench] Component path: ${HOMEBOY_COMPONENT_PATH:-$(pwd)}"
fi

exec bash "${SCRIPT_DIR}/bench-runner-playground.sh" "$@"
