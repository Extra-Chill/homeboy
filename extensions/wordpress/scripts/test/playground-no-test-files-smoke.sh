#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner-playground.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_PATH="${TMPDIR}/extension"
PLUGIN_PATH="${TMPDIR}/component"
mkdir -p "${EXTENSION_PATH}/node_modules/.bin" "${PLUGIN_PATH}/tests"

cat > "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
cat > "${HOMEBOY_PLUGIN_PATH}/.pg-test-result.txt" <<'LOG'
STAGE_BEGIN:discover_tests
DISCOVERY: dirs=/wordpress/wp-content/plugins/example/tests suffixes=Test.php prefixes=test- excludes=0 found=0
NO_TEST_FILES
STAGE_OK:discover_tests
LOG
exit 1
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"

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

set +e
skip_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" HOMEBOY_COMPONENT_ID="example" bash "$RUNNER" 2>&1)
skip_status=$?
set -e

if [ "$skip_status" -ne 0 ]; then
    echo "Expected no-files run without component PHPUnit config to skip with exit 0; got $skip_status" >&2
    echo "$skip_output" >&2
    exit 1
fi
assert_contains "$skip_output" "NO PHPUNIT TEST FILES DISCOVERED"
assert_contains "$skip_output" "Skipping PHPUnit tests: no files matched the WordPress runner discovery contract."
assert_contains "$skip_output" "ending in Test.php or starting with test-."
assert_not_contains "$skip_output" "UNCLASSIFIED PLAYGROUND FAILURE"

touch "${PLUGIN_PATH}/phpunit.xml.dist"
set +e
failure_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" HOMEBOY_COMPONENT_ID="example" bash "$RUNNER" 2>&1)
failure_status=$?
set -e

if [ "$failure_status" -eq 0 ]; then
    echo "Expected no-files run with component PHPUnit config to fail" >&2
    echo "$failure_output" >&2
    exit 1
fi
assert_contains "$failure_output" "NO PHPUNIT TEST FILES DISCOVERED"
assert_contains "$failure_output" "PHPUnit config exists, but no files matched the WordPress runner discovery contract."
assert_not_contains "$failure_output" "UNCLASSIFIED PLAYGROUND FAILURE"
assert_not_contains "$failure_output" "Skipping PHPUnit tests: no files matched"

echo "Playground no-test-files smoke passed (9 assertions)"
