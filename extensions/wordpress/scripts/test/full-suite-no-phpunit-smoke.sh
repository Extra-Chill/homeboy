#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

component="${TMPDIR}/component"
runtime_stub="${TMPDIR}/runtime.sh"
results_writer="${TMPDIR}/write-test-results.sh"
mkdir -p "${component}/tests"
printf '%s\n' '<?php // Standalone smoke, not PHPUnit.' > "${component}/tests/contract-smoke.php"
printf '%s\n' '<?php // Support file, not an executable test.' > "${component}/tests/_stub-wp-and-rest.php"

cat > "$runtime_stub" <<'SH'
#!/usr/bin/env bash
echo "PHPUNIT_RUNTIME_INVOKED"
SH
chmod +x "$runtime_stub"

cat > "$results_writer" <<'SH'
homeboy_write_test_results() {
    jq -n --argjson total "$1" --argjson passed "$2" --argjson failed "$3" --argjson skipped "$4" --arg source "$5" \
        '{total: $total, passed: $passed, failed: $failed, skipped: $skipped, source: $source}' > "$HOMEBOY_TEST_RESULTS_FILE"
}
SH

run_router() {
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="$runtime_stub" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh"
}

HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"skip"}' run_router > "${TMPDIR}/skip.out"
grep -Fq "Skipping PHPUnit: no canonical test files were discovered." "${TMPDIR}/skip.out"
if grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/skip.out"; then
    echo "Skip policy must not invoke PHPUnit for a smoke-only component." >&2
    exit 1
fi

set +e
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail"}' run_router > "${TMPDIR}/fail.out" 2>&1
status=$?
set -e
if [ "$status" -ne 1 ]; then
    echo "Expected fail policy to exit 1, got ${status}." >&2
    exit 1
fi
grep -Fq "ERROR: no canonical PHPUnit test files were discovered." "${TMPDIR}/fail.out"

# Full-suite runs consume the generic standalone_php_test_paths contract. Its
# declared executable runs through bounded host PHP; nearby support files remain
# excluded and the standalone-only result is normalized with exact counts.
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","standalone_php_test_paths":["tests/contract-smoke.php"]}' \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$results_writer" \
HOMEBOY_TEST_RESULTS_FILE="${TMPDIR}/standalone-results.json" \
run_router > "${TMPDIR}/standalone.out"
grep -Fq "PHP_SMOKE_BEGIN:tests/contract-smoke.php" "${TMPDIR}/standalone.out"
grep -Fq "PHP_SMOKE_OK:tests/contract-smoke.php" "${TMPDIR}/standalone.out"
grep -Fq "FULL_SUITE_STANDALONE_PHP_SUMMARY:candidates=2 selected=1 routed=1 excluded=1 passed=1 failed=0" "${TMPDIR}/standalone.out"
if grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/standalone.out"; then
    echo "Standalone full suite must not invoke PHPUnit when no PHPUnit tests exist." >&2
    exit 1
fi
jq -e '.total == 1 and .passed == 1 and .failed == 0 and .skipped == 0 and .source == "full-suite-host-php"' "${TMPDIR}/standalone-results.json" >/dev/null

HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"skip","wp_codebox_phpunit_test_root":"/unresolved/tests"}' run_router > "${TMPDIR}/explicit.out"
grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/explicit.out"

printf '%s\n' '<?php // Canonical PHPUnit test.' > "${component}/tests/ContractTest.php"
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"skip"}' run_router > "${TMPDIR}/phpunit.out"
grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/phpunit.out"

echo "Full-suite no-PHPUnit routing smoke passed"
