#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_FILE="${SCRIPT_DIR}/runtime-agent-driver.php"

if [ ! -f "$PLUGIN_FILE" ]; then
    echo "ERROR: plugin fixture not found at $PLUGIN_FILE" >&2
    exit 1
fi

php -l "$PLUGIN_FILE" >/dev/null

for header in "Plugin Name:" "Plugin URI:"; do
    if ! grep -q "$header" "$PLUGIN_FILE"; then
        echo "ERROR: missing plugin header: $header" >&2
        exit 1
    fi
done

for runtime_pattern in \
    "add_action" \
    "add_filter" \
    "register_activation_hook" \
    "register_deactivation_hook" \
    "^[[:space:]]*class[[:space:]]"; do
    if grep -qE "$runtime_pattern" "$PLUGIN_FILE"; then
        echo "ERROR: unexpected runtime behavior matched: $runtime_pattern" >&2
        exit 1
    fi
done

echo "Runtime agent driver fixture smoke test PASSED"
