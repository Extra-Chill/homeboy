#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_RUNNER="${SCRIPT_DIR}/test-runner-playground.sh"
BENCH_RUNNER="${SCRIPT_DIR}/../bench/bench-runner-playground.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

EXTENSION_PATH="${TMP_ROOT}/extension"
PLUGIN_PATH="${TMP_ROOT}/component"
BENCH_HELPER_SH="${TMP_ROOT}/bench-helper.sh"
BENCH_HELPER_PHP="${TMP_ROOT}/bench-helper.php"

mkdir -p "${EXTENSION_PATH}/node_modules/.bin" "${PLUGIN_PATH}/tests/bench" "${PLUGIN_PATH}/tests"

cat > "${PLUGIN_PATH}/tests/OnlyTest.php" <<'PHP'
<?php
class OnlyTest extends WP_UnitTestCase {}
PHP

cat > "${PLUGIN_PATH}/tests/bench/noop.php" <<'PHP'
<?php
function bench_main(): void {}
PHP

cat > "$BENCH_HELPER_SH" <<'SH'
#!/usr/bin/env bash
homeboy_write_empty_bench_results() {
    printf '{"component_id":"%s","iterations":%s,"scenarios":[]}' "$1" "$2" > "$3"
}
SH
chmod +x "$BENCH_HELPER_SH"

cat > "$BENCH_HELPER_PHP" <<'PHP'
<?php
PHP

cat > "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

actual=""
for arg in "$@"; do
    case "$arg" in
        --wp=*)
            actual="${arg#--wp=}"
            ;;
    esac
done

if [ -z "$actual" ]; then
    echo "missing --wp argument" >&2
    exit 1
fi

if [ "$actual" != "${EXPECT_PLAYGROUND_WP}" ]; then
    echo "expected --wp=${EXPECT_PLAYGROUND_WP}, got --wp=${actual}" >&2
    exit 1
fi

echo "WP_VERSION_OK:${actual}"

if [ -n "${HOMEBOY_PLUGIN_PATH:-}" ]; then
    cat > "${HOMEBOY_PLUGIN_PATH}/.pg-test-result.txt" <<'LOG'
STAGE_BEGIN:run_tests
ALL TESTS PASSED
TESTS: 1 FAILURES: 0 ERRORS: 0
STAGE_OK:run_tests
LOG
    cat > "${HOMEBOY_PLUGIN_PATH}/.pg-bench-result.txt" <<'LOG'
STAGE_BEGIN:run_bench
STAGE_OK:run_bench
LOG
    cat > "${HOMEBOY_PLUGIN_PATH}/.pg-bench-results.json" <<'JSON'
{"component_id":"example","iterations":1,"scenarios":[]}
JSON
fi
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"

SETTINGS_JSON='{"playground_wordpress_version":"6.10"}'

bash -n "$TEST_RUNNER"
bash -n "$BENCH_RUNNER"

test_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="example" \
    HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    EXPECT_PLAYGROUND_WP="6.10" \
    bash "$TEST_RUNNER" 2>&1)

if [[ "$test_output" != *"WP_VERSION_OK:6.10"* ]]; then
    echo "Expected configured WordPress version to reach Playground test runner" >&2
    echo "$test_output" >&2
    exit 1
fi

bench_results="${TMP_ROOT}/bench-results.json"
bench_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="example" \
    HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    HOMEBOY_BENCH_ITERATIONS=1 \
    HOMEBOY_BENCH_RESULTS_FILE="$bench_results" \
    HOMEBOY_RUNTIME_BENCH_HELPER_SH="$BENCH_HELPER_SH" \
    HOMEBOY_RUNTIME_BENCH_HELPER_PHP="$BENCH_HELPER_PHP" \
    EXPECT_PLAYGROUND_WP="6.10" \
    bash "$BENCH_RUNNER" 2>&1)

if [[ "$bench_output" != *"WP_VERSION_OK:6.10"* ]]; then
    echo "Expected configured WordPress version to reach Playground bench runner" >&2
    echo "$bench_output" >&2
    exit 1
fi

default_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="example" \
    EXPECT_PLAYGROUND_WP="6.9" \
    bash "$TEST_RUNNER" 2>&1)

if [[ "$default_output" != *"WP_VERSION_OK:6.9"* ]]; then
    echo "Expected omitted WordPress version setting to preserve --wp=6.9" >&2
    echo "$default_output" >&2
    exit 1
fi

echo "Playground WordPress version smoke passed"
