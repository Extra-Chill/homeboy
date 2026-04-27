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
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [bench] Extension path: $EXTENSION_PATH"
    echo "DEBUG: [bench] Component: ${HOMEBOY_COMPONENT_ID:-none}"
    echo "DEBUG: [bench] Component path: ${COMPONENT_PATH:-$(pwd)}"
fi

exec bash "${SCRIPT_DIR}/bench-runner-playground.sh" "$@"
