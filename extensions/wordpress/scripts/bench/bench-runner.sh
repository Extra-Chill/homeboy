#!/usr/bin/env bash
set -euo pipefail

# Bench runner router for WordPress Homeboy extension.
#
# Basic in-tree bench workloads run through WP Codebox by default. More complex
# bench features still dispatch to the legacy direct Playground runner until
# their WP Codebox equivalents land under #698.
#
# This entry script resolves execution context and dispatches to the selected
# bench runner. Bash 4.0+ required (matches test-runner.sh's gate so bench
# inherits the same surface).

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
# runner can mount the right paths.
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${SCRIPT_DIR}/../lib/resolve-context.sh}"
# shellcheck source=/dev/null
source "$RESOLVE_CONTEXT_HELPER"
homeboy_resolve_context --component-alias PLUGIN_PATH

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: [bench] Extension path: $EXTENSION_PATH"
    echo "DEBUG: [bench] Component: ${HOMEBOY_COMPONENT_ID:-none}"
    echo "DEBUG: [bench] Component path: ${COMPONENT_PATH:-$(pwd)}"
fi

settings_json="${HOMEBOY_SETTINGS_JSON:-}"
[ -n "$settings_json" ] || settings_json="{}"

requires_legacy_playground=0
if printf '%s' "$settings_json" | jq -e '
    ((.playground_workloads // []) | length > 0)
    or ((.playground_scenario_manifests // .scenario_manifests // []) | length > 0)
    or ((.playground_file_mounts // []) | length > 0)
    or ((.wp_config_defines // {}) | length > 0)
    or ((.bench_env // {}) | length > 0)
    or ((.playground_blueprint // {}) | length > 0)
    or ((.bench_site_mode // "fresh") == "installed")
    or ((.bench_browser_target // {}) | length > 0)
' >/dev/null 2>&1; then
    requires_legacy_playground=1
fi

if [ "$requires_legacy_playground" -eq 1 ]; then
    echo "Notice: dispatching to legacy Playground bench runner for unsupported WP Codebox bench features." >&2
    exec bash "${SCRIPT_DIR}/bench-runner-playground.sh" "$@"
fi

exec bash "${SCRIPT_DIR}/bench-runner-wp-codebox.sh" "$@"
