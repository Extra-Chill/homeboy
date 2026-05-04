#!/usr/bin/env bash
set -euo pipefail

# WordPress/PHP formatter for homeboy's post-write formatting gate.
# Prefer the component's PHPCBF when present; otherwise use the extension's bundled PHPCBF.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/.." && pwd)"
COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-$PWD}"

if [ -x "${COMPONENT_PATH}/vendor/bin/phpcbf" ]; then
    cd "$COMPONENT_PATH"
    "${COMPONENT_PATH}/vendor/bin/phpcbf" "$COMPONENT_PATH" 2>&1
    exit $?
fi

if [ -x "${EXTENSION_PATH}/vendor/bin/phpcbf" ]; then
    cd "$COMPONENT_PATH"
    "${EXTENSION_PATH}/vendor/bin/phpcbf" "$COMPONENT_PATH" 2>&1
    exit $?
fi

echo "No PHPCBF found — skipping format"
exit 0
