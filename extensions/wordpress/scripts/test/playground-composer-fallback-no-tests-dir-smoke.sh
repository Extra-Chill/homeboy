#!/usr/bin/env bash
# Regression smoke: when a component has composer.json declaring scripts.test
# but NO tests/ directory, the runner must fall through to composer-fallback
# WITHOUT tripping `set -u` on PLUGIN_SLUG.
#
# Bug: homeboy-extensions#619. Before the fix, PLUGIN_SLUG was defined further
# down the script (past the composer-fallback exit), so the fallback's
# `echo "  Plugin: ${PLUGIN_SLUG} (...)"` died with `unbound variable`.
#
# This complements playground-no-test-files-smoke.sh, which always pre-creates
# tests/ — it exercises the empty-tests-dir composer fallback path, not the
# no-tests-dir-at-all path that triggered #619.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner-playground.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_PATH="${TMPDIR}/extension"
PLUGIN_PATH="${TMPDIR}/component"
BIN_PATH="${TMPDIR}/bin"
mkdir -p "${BIN_PATH}"
export PATH="${BIN_PATH}:${PATH}"

# Critical: do NOT create ${PLUGIN_PATH}/tests — that's what makes this smoke
# different from playground-no-test-files-smoke.sh and what trips #619.
mkdir -p "${EXTENSION_PATH}/node_modules/.bin" "${PLUGIN_PATH}"

# Stub playground-cli — should never be reached in the composer-fallback path,
# but provide it so the runner's discovery checks don't crash earlier.
cat > "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" <<'SH'
#!/usr/bin/env bash
echo "wp-playground-cli should not be invoked in composer-fallback path" >&2
exit 99
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"

# Stub composer so we can verify the fallback actually invokes `composer test`.
cat > "${BIN_PATH}/composer" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "composer:$PWD:$*" >> "${HOMEBOY_COMPOSER_CALLS_FILE}"
echo "stub-composer ran composer $*"
SH
chmod +x "${BIN_PATH}/composer"

cat > "${PLUGIN_PATH}/composer.json" <<'JSON'
{
    "scripts": {
        "test": "phpunit"
    }
}
JSON

assert_contains() {
    local haystack="$1"
    local needle="$2"
    if [[ "$haystack" != *"$needle"* ]]; then
        echo "Expected output to contain: $needle" >&2
        echo "Actual output:" >&2
        echo "$haystack" >&2
        exit 1
    fi
}

assert_not_contains() {
    local haystack="$1"
    local needle="$2"
    if [[ "$haystack" == *"$needle"* ]]; then
        echo "Expected output not to contain: $needle" >&2
        echo "Actual output:" >&2
        echo "$haystack" >&2
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Branch 1: COMPONENT_ID set → PLUGIN_SLUG resolves from $COMPONENT_ID
# ---------------------------------------------------------------------------

COMPOSER_CALLS_FILE="${TMPDIR}/composer-calls-with-id.log"
set +e
output_with_id=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="example" \
    HOMEBOY_COMPOSER_CALLS_FILE="$COMPOSER_CALLS_FILE" \
    bash "$RUNNER" 2>&1)
status_with_id=$?
set -e

# The whole point of the regression: the runner must NOT die with an unbound
# variable error before reaching the composer call.
assert_not_contains "$output_with_id" "PLUGIN_SLUG: unbound variable"
assert_not_contains "$output_with_id" "unbound variable"

# Composer fallback path must be reached and announce itself with the
# canonical component slug, not basename.
assert_contains "$output_with_id" "Running Composer test script..."
assert_contains "$output_with_id" "Backend: composer-script"
assert_contains "$output_with_id" "Plugin: example (${PLUGIN_PATH})"

# And composer test must actually be invoked.
assert_contains "$(cat "$COMPOSER_CALLS_FILE")" "composer:${PLUGIN_PATH}:test"

if [ "$status_with_id" -ne 0 ]; then
    echo "Expected composer-fallback with COMPONENT_ID to exit 0; got $status_with_id" >&2
    echo "$output_with_id" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Branch 2: COMPONENT_ID unset → PLUGIN_SLUG falls back to basename
# ---------------------------------------------------------------------------

COMPOSER_CALLS_FILE_NO_ID="${TMPDIR}/composer-calls-no-id.log"
set +e
output_no_id=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPOSER_CALLS_FILE="$COMPOSER_CALLS_FILE_NO_ID" \
    bash "$RUNNER" 2>&1)
status_no_id=$?
set -e

assert_not_contains "$output_no_id" "PLUGIN_SLUG: unbound variable"
assert_not_contains "$output_no_id" "unbound variable"
assert_contains "$output_no_id" "Running Composer test script..."

# basename of /tmp/<random>/component → "component"
assert_contains "$output_no_id" "Plugin: component (${PLUGIN_PATH})"
assert_contains "$(cat "$COMPOSER_CALLS_FILE_NO_ID")" "composer:${PLUGIN_PATH}:test"

if [ "$status_no_id" -ne 0 ]; then
    echo "Expected composer-fallback without COMPONENT_ID to exit 0; got $status_no_id" >&2
    echo "$output_no_id" >&2
    exit 1
fi

echo "Playground composer-fallback no-tests-dir smoke passed (#619 regression guard)"
