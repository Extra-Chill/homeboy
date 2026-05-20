#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner-playground.sh"
TEMPLATE="${SCRIPT_DIR}/playground-runner.php"
BOOTSTRAP="${SCRIPT_DIR}/../lib/playground-bootstrap.php"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_contains "$TEMPLATE" "{{CHANGED_TEST_FILES_JSON_B64}}"
assert_contains "$TEMPLATE" "pg_filter_changed_test_files"
assert_contains "$BOOTSTRAP" "function pg_filter_changed_test_files"
assert_contains "$BOOTSTRAP" "SCOPED_TEST_FILES requested="
assert_contains "$RUNNER" "CHANGED_TEST_FILES_JSON"
assert_contains "$RUNNER" "{{CHANGED_TEST_FILES_JSON_B64}}"

EXTENSION_PATH="${TMPDIR}/extension"
PLUGIN_PATH="${TMPDIR}/component"
mkdir -p "${EXTENSION_PATH}/node_modules/.bin" "${PLUGIN_PATH}/tests"

cat > "${PLUGIN_PATH}/tests/OnlyTest.php" <<'PHP'
<?php
class OnlyTest extends WP_UnitTestCase {}
PHP
cat > "${PLUGIN_PATH}/tests/OtherTest.php" <<'PHP'
<?php
class OtherTest extends WP_UnitTestCase {}
PHP

cat > "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

wrapper=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--mount" ]; then
        mount_arg="$2"
        if [[ "$mount_arg" == *":/runner.php" ]]; then
            wrapper="${mount_arg%:/runner.php}"
        fi
        shift 2
        continue
    fi
    shift
done

if [ -z "$wrapper" ] || [ ! -f "$wrapper" ]; then
    echo "runner wrapper mount not found" >&2
    exit 1
fi

if ! grep -Fq 'WyJ0ZXN0cy9Pbmx5VGVzdC5waHAiXQ==' "$wrapper"; then
    echo "changed-test JSON was not base64-substituted into wrapper" >&2
    exit 1
fi
if grep -Fq '{{CHANGED_TEST_FILES_JSON_B64}}' "$wrapper"; then
    echo "changed-test placeholder leaked into wrapper" >&2
    exit 1
fi

cat > "${HOMEBOY_PLUGIN_PATH}/.pg-test-result.txt" <<'LOG'
STAGE_BEGIN:discover_tests
DISCOVERY: dirs=/wordpress/wp-content/plugins/example/tests suffixes=Test.php prefixes=test- excludes=0 found=2
SCOPED_TEST_FILES requested=1 matched=1
STAGE_OK:discover_tests
STAGE_BEGIN:run_tests
RUNNING 1 TEST FILES
ALL TESTS PASSED
TESTS: 1 FAILURES: 0 ERRORS: 0
STAGE_OK:run_tests
LOG
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"

php -l "$TEMPLATE" >/dev/null
bash -n "$RUNNER"

output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="example" \
    HOMEBOY_CHANGED_TEST_FILES='tests/OnlyTest.php' \
    bash "$RUNNER" 2>&1)

if [[ "$output" != *"Playground test run complete."* ]]; then
    echo "Expected successful scoped Playground run" >&2
    echo "$output" >&2
    exit 1
fi

echo "Playground changed-scope smoke passed"
