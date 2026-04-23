#!/usr/bin/env bash
#
# Playground backend db.php drop-in coexistence smoke test.
#
# Runs the fixture at wordpress/tests/fixtures/dropin-coexistence/ through the
# Playground backend and asserts that a custom db.php drop-in can coexist
# with Playground's built-in SQLite integration.
#
# This is a manual integration test, not part of the normal CI matrix. Run it
# after changes to:
#   - test-runner-playground.sh (the bash dispatcher)
#   - playground-runner.php (the template)
#   - anything touching the db.php mount logic
#
# Usage: bash wordpress/scripts/test/playground-dropin-smoke.sh
# Exit:  0 = coexistence works, non-zero = regression
#
# Background: see wordpress/docs/PLAYGROUND_DROPIN.md

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/dropin-coexistence"

if [ ! -d "$FIXTURE_DIR" ]; then
    echo "ERROR: fixture not found at $FIXTURE_DIR" >&2
    exit 1
fi

if [ ! -f "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" ]; then
    echo "ERROR: @wp-playground/cli not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && npm install" >&2
    exit 1
fi

echo "============================================"
echo "Playground drop-in coexistence smoke test"
echo "============================================"
echo "Fixture: $FIXTURE_DIR"
echo ""

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$FIXTURE_DIR" \
    HOMEBOY_COMPONENT_ID="dropin-coexistence-fixture" \
    bash "${SCRIPT_DIR}/test-runner-playground.sh"

exit_code=$?
if [ $exit_code -eq 0 ]; then
    echo ""
    echo "============================================"
    echo "✓ Drop-in coexistence smoke test PASSED"
    echo "============================================"
else
    echo ""
    echo "============================================"
    echo "✗ Drop-in coexistence smoke test FAILED (exit $exit_code)"
    echo "============================================"
    echo "See docs/PLAYGROUND_DROPIN.md for the expected mechanism."
fi

exit $exit_code
