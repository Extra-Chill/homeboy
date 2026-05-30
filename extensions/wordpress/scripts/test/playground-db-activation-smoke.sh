#!/usr/bin/env bash
#
# Test runner DB-touching activation smoke (homeboy-extensions#431).
#
# Pins the test runner's post-install activation behavior so a future
# refactor cannot silently regress to the pre-#431 shape that fired
# `activate_<plugin>` inline during muplugins_loaded — before wp-phpunit's
# install.php had created the wptests_* tables. Mirrors the bench-side
# smoke with PHPUnit assertions instead of workload metrics.
#
# Asserts (via PHPUnit inside the runner):
#   1. The dep's add_option() activation write succeeded (wptests_options
#      existed when activation fired).
#   2. The dep's get_users() activation read succeeded (wptests_users
#      existed when activation fired).
#   3. The dep's plugins_loaded callback still fires (don't lose
#      homeboy-extensions#426/#427).
#
# Pre-#431 the dep's activation hook fataled with "no such table:
# wptests_options" / "no such table: wptests_users" and the entire test run
# crashed at the install boundary.
#
# Usage: bash wordpress/scripts/test/playground-db-activation-smoke.sh
# Exit:  0 = test runner activates plugins post-install, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOST_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/test-db-activation-host"
DEP_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-db-activation-dep"

if [ ! -d "$HOST_FIXTURE_DIR" ]; then
    echo "ERROR: host fixture not found at $HOST_FIXTURE_DIR" >&2
    exit 1
fi

if [ ! -d "$DEP_FIXTURE_DIR" ]; then
    echo "ERROR: dep fixture not found at $DEP_FIXTURE_DIR" >&2
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
echo "Test runner DB-touching activation smoke (#431)"
echo "============================================"
echo "Host fixture: $HOST_FIXTURE_DIR"
echo "Dep fixture:  $DEP_FIXTURE_DIR"
echo ""

HOMEBOY_COMPONENT_ID=test-db-activation-host \
HOMEBOY_COMPONENT_PATH="$HOST_FIXTURE_DIR" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    bash "${SCRIPT_DIR}/test-runner.sh"

echo ""
echo "============================================"
echo "✓ Test runner DB-touching activation smoke PASSED"
echo "============================================"
