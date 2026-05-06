#!/usr/bin/env bash
#
# Test runner validation-dependency `plugins_loaded` smoke (homeboy-extensions#426).
#
# Pins the test runner's existing pre-install dep load behavior so a future
# refactor of the test runner can't silently regress Abilities API / hook-
# gated plugin support. Mirrors the bench-side smoke with a PHPUnit assertion
# instead of a workload metric.
#
# Asserts:
#   1. The dep fixture's plugins_loaded callback fired (sets a global flag).
#   2. The dep's Abilities API ability is in the registry.
#
# A pre-#426 regression in the test runner — i.e. moving load_deps after
# install — would make both assertions fail because the dep file would only
# be required after wp-settings.php had already fired plugins_loaded /
# wp_abilities_api_init.
#
# Usage: bash wordpress/scripts/test/playground-dep-plugins-loaded-smoke.sh
# Exit:  0 = test runner loads deps before plugins_loaded, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOST_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/test-plugins-loaded-host"
DEP_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-plugins-loaded-dep"

if [ ! -d "$HOST_FIXTURE_DIR" ]; then
    echo "ERROR: host fixture not found at $HOST_FIXTURE_DIR" >&2
    exit 1
fi

if [ ! -d "$DEP_FIXTURE_DIR" ]; then
    echo "ERROR: dep fixture not found at $DEP_FIXTURE_DIR" >&2
    exit 1
fi

if [ ! -f "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" ]; then
    echo "ERROR: @wp-playground/cli not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && npm install" >&2
    exit 1
fi

if [ ! -d "${EXTENSION_PATH}/vendor/wp-phpunit" ]; then
    echo "ERROR: wp-phpunit not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && composer install" >&2
    exit 1
fi

# Pass the dep fixture as an absolute-path validation dependency. The
# resolver hits the direct-path branch first so we avoid homeboy / git fallbacks.
SETTINGS_JSON=$(cat <<EOF
{"validation_dependencies": ["${DEP_FIXTURE_DIR}"]}
EOF
)

echo "============================================"
echo "Test runner dep plugins_loaded smoke (#426)"
echo "============================================"
echo "Host fixture: $HOST_FIXTURE_DIR"
echo "Dep fixture:  $DEP_FIXTURE_DIR"
echo ""

HOMEBOY_COMPONENT_ID=test-plugins-loaded-host \
HOMEBOY_COMPONENT_PATH="$HOST_FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "${SCRIPT_DIR}/test-runner.sh"

echo ""
echo "============================================"
echo "✓ Test runner dep plugins_loaded smoke PASSED"
echo "============================================"
