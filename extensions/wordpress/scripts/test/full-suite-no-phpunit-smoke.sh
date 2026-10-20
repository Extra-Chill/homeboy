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

smoke_runner_stub="${TMPDIR}/host-smoke-wp.sh"
cat > "$smoke_runner_stub" <<'SH'
#!/usr/bin/env bash
# Stands in for the booted-WordPress backend: records the exact selection it was
# handed and reports counts in the backend's own summary contract.
echo "HOST_SMOKE_WP_INVOKED:${HOMEBOY_WORDPRESS_HOST_SMOKE_FILES:-none}"
count=0
while IFS= read -r smoke_file; do
    [ -n "$smoke_file" ] || continue
    count=$((count + 1))
done <<< "${HOMEBOY_WORDPRESS_HOST_SMOKE_FILES:-}"
echo "HOST_SMOKE_SUMMARY:passed=${count} failed=0"
SH
chmod +x "$smoke_runner_stub"

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
    HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="$smoke_runner_stub" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh"
}

# An explicit standalone_php_test_paths: [] replaces the convention default and
# selects nothing, so the component has no tests at all and the no-PHPUnit
# policy decides the outcome.
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"skip","standalone_php_test_paths":[]}' run_router > "${TMPDIR}/skip.out"
grep -Fq "Skipping PHPUnit: no canonical test files were discovered." "${TMPDIR}/skip.out"
grep -Fq "FULL_SUITE_STANDALONE_PHP_SUMMARY:candidates=2 selected=0 routed=0 excluded=2" "${TMPDIR}/skip.out"
if grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/skip.out"; then
    echo "Skip policy must not invoke PHPUnit for a smoke-only component." >&2
    exit 1
fi
if grep -Fq "PHP_SMOKE_BEGIN:" "${TMPDIR}/skip.out"; then
    echo "An explicit empty standalone_php_test_paths must run no standalone smokes." >&2
    exit 1
fi

set +e
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","standalone_php_test_paths":[]}' run_router > "${TMPDIR}/fail.out" 2>&1
status=$?
set -e
if [ "$status" -ne 1 ]; then
    echo "Expected fail policy to exit 1, got ${status}." >&2
    exit 1
fi
grep -Fq "ERROR: no canonical PHPUnit test files were discovered." "${TMPDIR}/fail.out"

# A declared standalone_php_test_paths list replaces the convention default. Its
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

# Glob declarations select every matching standalone file exactly once, and
# the glob-declared standalone-only suite satisfies the no-PHPUnit contract.
printf '%s\n' '<?php // Standalone smoke, matched by the same glob.' > "${component}/tests/overlapping-smoke.php"
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","standalone_php_test_paths":["tests/*-smoke.php","tests/overlapping-smoke.php"]}' \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$results_writer" \
HOMEBOY_TEST_RESULTS_FILE="${TMPDIR}/overlap-results.json" \
run_router > "${TMPDIR}/overlap.out"
grep -Fq "FULL_SUITE_STANDALONE_PHP_SUMMARY:candidates=3 selected=2 routed=2 excluded=1 passed=2 failed=0" "${TMPDIR}/overlap.out"
overlap_routes="$(grep -cF "FULL_SUITE_STANDALONE_PHP_ROUTE:tests/overlapping-smoke.php:" "${TMPDIR}/overlap.out" || true)"
[ "$overlap_routes" -eq 1 ] || { echo "Expected exactly one route for the overlapping standalone file, got ${overlap_routes}." >&2; exit 1; }
contract_routes="$(grep -cF "FULL_SUITE_STANDALONE_PHP_ROUTE:tests/contract-smoke.php:" "${TMPDIR}/overlap.out" || true)"
[ "$contract_routes" -eq 1 ] || { echo "Expected exactly one route for the glob-declared file, got ${contract_routes}." >&2; exit 1; }
if grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/overlap.out"; then
    echo "Declared standalone full suite must not invoke PHPUnit when no PHPUnit tests exist." >&2
    exit 1
fi
jq -e '.total == 2 and .passed == 2 and .failed == 0 and .source == "full-suite-host-php"' "${TMPDIR}/overlap-results.json" >/dev/null

# wordpress_test_paths declares booted-WordPress smokes. A file it matches
# leaves the standalone suite even when a broad standalone glob also matches,
# and is dispatched to the booted-WordPress backend in the same full-suite run,
# so a declared WordPress smoke is executed rather than silently skipped.
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","standalone_php_test_paths":["tests/*-smoke.php"],"wordpress_test_paths":["tests/overlapping-smoke.php","tests/_stub-wp-and-rest.php"]}' \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$results_writer" \
HOMEBOY_TEST_RESULTS_FILE="${TMPDIR}/wordpress-paths-results.json" \
run_router > "${TMPDIR}/wordpress-paths.out"
grep -Fq "FULL_SUITE_STANDALONE_PHP_SUMMARY:candidates=3 selected=1 routed=1 excluded=2 passed=1 failed=0" "${TMPDIR}/wordpress-paths.out"
if grep -Fq "FULL_SUITE_STANDALONE_PHP_ROUTE:tests/overlapping-smoke.php:" "${TMPDIR}/wordpress-paths.out"; then
    echo "A wordpress_test_paths file must be excluded from the standalone suite." >&2
    exit 1
fi
# The declaration is not merely excluded: it reaches the booted-WordPress backend.
grep -Fq "FULL_SUITE_WORDPRESS_SMOKE_ROUTE:tests/overlapping-smoke.php:runner=host-smoke-wp" "${TMPDIR}/wordpress-paths.out"
grep -Fq "HOST_SMOKE_WP_INVOKED:tests/overlapping-smoke.php" "${TMPDIR}/wordpress-paths.out"
# A declared support file is an explicit, counted exclusion rather than a
# selection the backend would reject.
grep -Fq "FULL_SUITE_WORDPRESS_SMOKE_EXCLUDED:tests/_stub-wp-and-rest.php:reason=unsupported_test_shape" "${TMPDIR}/wordpress-paths.out"
grep -Fq "FULL_SUITE_WORDPRESS_SMOKE_SUMMARY:candidates=2 selected=1 routed=1 excluded=1" "${TMPDIR}/wordpress-paths.out"
# Both declarations reconcile into one executed-test ledger.
jq -e '.total == 2 and .passed == 2 and .failed == 0 and .source == "full-suite-host-php"' "${TMPDIR}/wordpress-paths-results.json" >/dev/null

# A component declaring only wordpress_test_paths keeps the convention default
# for standalone smokes. The file both match (overlapping-smoke.php matches the
# default tests/**/*-smoke.php) runs only as a booted-WordPress test.
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","wordpress_test_paths":["tests/overlapping-smoke.php"]}' \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$results_writer" \
HOMEBOY_TEST_RESULTS_FILE="${TMPDIR}/wordpress-only-results.json" \
run_router > "${TMPDIR}/wordpress-only.out"
grep -Fq "HOST_SMOKE_WP_INVOKED:tests/overlapping-smoke.php" "${TMPDIR}/wordpress-only.out"
grep -Fq "FULL_SUITE_STANDALONE_PHP_ROUTE:tests/contract-smoke.php:runner=host-php-smoke" "${TMPDIR}/wordpress-only.out"
if grep -Fq "FULL_SUITE_STANDALONE_PHP_ROUTE:tests/overlapping-smoke.php:" "${TMPDIR}/wordpress-only.out"; then
    echo "A file matched by both the default and wordpress_test_paths must run only as a WordPress test." >&2
    exit 1
fi
jq -e '.total == 2 and .passed == 2 and .failed == 0 and .source == "full-suite-host-php"' "${TMPDIR}/wordpress-only-results.json" >/dev/null

# Invalid declarations fail closed instead of silently dropping tests.
set +e
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","wordpress_test_paths":"tests/overlapping-smoke.php"}' run_router > "${TMPDIR}/invalid.out" 2>&1
invalid_status=$?
set -e
[ "$invalid_status" -ne 0 ] || { echo "A non-array wordpress_test_paths must fail." >&2; exit 1; }
grep -Fq "ERROR: wordpress_test_paths must be an array of strings." "${TMPDIR}/invalid.out"
set +e
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","test_path_excludes":[1]}' run_router > "${TMPDIR}/invalid-excludes.out" 2>&1
invalid_status=$?
set -e
[ "$invalid_status" -ne 0 ] || { echo "A non-string test_path_excludes entry must fail." >&2; exit 1; }
grep -Fq "ERROR: test_path_excludes must be an array of strings." "${TMPDIR}/invalid-excludes.out"
rm -f "${component}/tests/overlapping-smoke.php"

# Convention-first discovery (#2967): with no test-path settings at all, every
# tests/**/*-smoke.php runs as a standalone PHP smoke, including nested ones
# (`**` matches zero or more directories), in sorted order.
mkdir -p "${component}/tests/nested/deep" "${component}/tests/fixtures"
printf '%s\n' '<?php // Nested standalone smoke.' > "${component}/tests/nested/deep/nested-smoke.php"
printf '%s\n' '<?php // Fixture that happens to match the convention.' > "${component}/tests/fixtures/fixture-smoke.php"
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail"}' \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$results_writer" \
HOMEBOY_TEST_RESULTS_FILE="${TMPDIR}/default-results.json" \
run_router > "${TMPDIR}/default.out"
grep -Fq "FULL_SUITE_STANDALONE_PHP_SUMMARY:candidates=4 selected=3 routed=3 excluded=1 passed=3 failed=0" "${TMPDIR}/default.out"
default_routes="$(grep -F "FULL_SUITE_STANDALONE_PHP_ROUTE:" "${TMPDIR}/default.out" | cut -d: -f2)"
expected_routes=$'tests/contract-smoke.php\ntests/fixtures/fixture-smoke.php\ntests/nested/deep/nested-smoke.php'
[ "$default_routes" = "$expected_routes" ] || { printf 'Unexpected default discovery routes:\n%s\n' "$default_routes" >&2; exit 1; }
grep -Fq "PHP_SMOKE_OK:tests/nested/deep/nested-smoke.php" "${TMPDIR}/default.out"
if grep -Fq "HOST_SMOKE_WP_INVOKED" "${TMPDIR}/default.out"; then
    echo "Default discovery must not route anything to booted WordPress." >&2
    exit 1
fi
jq -e '.total == 3 and .passed == 3 and .failed == 0 and .source == "full-suite-host-php"' "${TMPDIR}/default-results.json" >/dev/null

# test_path_excludes removes fixtures/helpers the convention would select.
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","test_path_excludes":["tests/fixtures/**"]}' run_router > "${TMPDIR}/excludes.out"
grep -Fq "FULL_SUITE_STANDALONE_PHP_SUMMARY:candidates=4 selected=2 routed=2 excluded=2 passed=2 failed=0" "${TMPDIR}/excludes.out"
if grep -Fq "tests/fixtures/fixture-smoke.php" "${TMPDIR}/excludes.out"; then
    echo "An excluded fixture must not be selected." >&2
    exit 1
fi

# Excludes also remove wordpress_test_paths matches.
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","wordpress_test_paths":["tests/fixtures/*.php"],"test_path_excludes":["tests/fixtures/**"]}' run_router > "${TMPDIR}/excludes-wordpress.out"
grep -Fq "FULL_SUITE_WORDPRESS_SMOKE_SUMMARY:candidates=0 selected=0 routed=0 excluded=0" "${TMPDIR}/excludes-wordpress.out"
if grep -Fq "HOST_SMOKE_WP_INVOKED" "${TMPDIR}/excludes-wordpress.out"; then
    echo "An excluded wordpress_test_paths match must not reach booted WordPress." >&2
    exit 1
fi

# A new smoke added under tests/ is picked up without any settings edit.
mkdir -p "${component}/tests/added"
printf '%s\n' '<?php // Added later; no homeboy.json change.' > "${component}/tests/added/later-smoke.php"
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail","test_path_excludes":["tests/fixtures/**"]}' run_router > "${TMPDIR}/added.out"
grep -Fq "FULL_SUITE_STANDALONE_PHP_ROUTE:tests/added/later-smoke.php:runner=host-php-smoke" "${TMPDIR}/added.out"
grep -Fq "PHP_SMOKE_OK:tests/added/later-smoke.php" "${TMPDIR}/added.out"
grep -Fq "FULL_SUITE_STANDALONE_PHP_SUMMARY:candidates=5 selected=3 routed=3 excluded=2 passed=3 failed=0" "${TMPDIR}/added.out"
rm -rf "${component}/tests/nested" "${component}/tests/fixtures" "${component}/tests/added"

HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"skip","wp_codebox_phpunit_test_root":"/unresolved/tests"}' run_router > "${TMPDIR}/explicit.out"
grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/explicit.out"

printf '%s\n' '<?php // Canonical PHPUnit test.' > "${component}/tests/ContractTest.php"
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"skip"}' run_router > "${TMPDIR}/phpunit.out"
grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/phpunit.out"

echo "Full-suite no-PHPUnit routing smoke passed"
