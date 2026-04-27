#!/usr/bin/env bash
#
# Playground extension mount smoke test.
#
# Verifies a symlinked Homeboy WordPress extension path is resolved before it is
# mounted, so files under /homeboy-extension are visible inside PHP-WASM.
#
# Usage: bash wordpress/scripts/test/playground-extension-mount-smoke.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
PLAYGROUND_CLI="${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"
PLAYGROUND_PATHS_HELPER="${SCRIPT_DIR}/../lib/playground-paths.sh"

# shellcheck source=../lib/playground-paths.sh
source "$PLAYGROUND_PATHS_HELPER"

if [ ! -f "$PLAYGROUND_CLI" ]; then
    echo "ERROR: @wp-playground/cli not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && npm install" >&2
    exit 1
fi

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-extension-link.XXXXXX")"
RUNNER_TMPFILE=""
cleanup() {
    rm -rf "$TMP_ROOT"
    if [ -n "$RUNNER_TMPFILE" ]; then
        rm -f "$RUNNER_TMPFILE"
    fi
}
trap cleanup EXIT

LINK_PATH="${TMP_ROOT}/wordpress-extension"
ln -s "$EXTENSION_PATH" "$LINK_PATH"

RESOLVED_PATH="$(homeboy_playground_resolve_mount_path "$LINK_PATH")"
if [ "$RESOLVED_PATH" = "$LINK_PATH" ]; then
    echo "ERROR: expected symlink path to resolve before mounting" >&2
    exit 1
fi

if [ ! -f "${RESOLVED_PATH}/scripts/lib/playground-bootstrap.php" ]; then
    echo "ERROR: resolved extension path does not contain playground-bootstrap.php: ${RESOLVED_PATH}" >&2
    exit 1
fi

RUNNER_TMPFILE="$(mktemp "${TMPDIR:-/tmp}/pg-extension-mount.XXXXXX.php")"
cat > "$RUNNER_TMPFILE" <<'PHP'
<?php
$path = '/homeboy-extension/scripts/lib/playground-bootstrap.php';
echo 'exists=' . ( file_exists( $path ) ? 'yes' : 'no' ) . PHP_EOL;
PHP

echo "============================================"
echo "Playground extension mount smoke test"
echo "============================================"
echo "Symlink:  $LINK_PATH"
echo "Resolved: $RESOLVED_PATH"
echo ""

OUTPUT=$("$PLAYGROUND_CLI" php \
    --mount "${RESOLVED_PATH}:/homeboy-extension" \
    --mount "${RUNNER_TMPFILE}:/runner.php" \
    --wp=6.9 \
    --verbosity=quiet \
    -- /runner.php)

printf '%s\n' "$OUTPUT"

if ! printf '%s\n' "$OUTPUT" | grep -q 'exists=yes'; then
    echo "ERROR: expected playground-bootstrap.php to be visible in Playground VFS" >&2
    exit 1
fi

echo "============================================"
echo "✓ Playground extension mount smoke test PASSED"
echo "============================================"
