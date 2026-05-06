#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUNNER_SH="${ROOT}/wordpress/scripts/test/test-runner-playground.sh"
RUNNER_PHP="${ROOT}/wordpress/scripts/test/playground-runner.php"
PHPUNIT_XML="${ROOT}/wordpress/phpunit.xml.dist"

assert_contains() {
    local file="$1"
    local needle="$2"
    local label="$3"

    if ! grep -Fq -- "$needle" "$file"; then
        echo "FAIL: ${label}" >&2
        echo "Missing: ${needle}" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local needle="$2"
    local label="$3"

    if grep -Fq -- "$needle" "$file"; then
        echo "FAIL: ${label}" >&2
        echo "Unexpected: ${needle}" >&2
        exit 1
    fi
}

assert_contains "$RUNNER_SH" 'PASSTHROUGH_ARGS=()' "Playground runner captures PHPUnit passthrough args"
assert_contains "$RUNNER_SH" '-- /runner.php "${PASSTHROUGH_ARGS[@]}"' "Playground runner forwards parsed PHPUnit passthrough args"

assert_contains "$RUNNER_PHP" 'function pg_is_component_phpunit_directory($raw_path)' "Component directory filter exists"
assert_contains "$RUNNER_PHP" "return \$normalized === 'tests' || strpos(\$normalized, 'tests/') === 0;" "Only plugin tests/ entries are accepted"
assert_contains "$RUNNER_PHP" 'new RecursiveIteratorIterator(' "Test discovery recurses into nested directories"
assert_contains "$RUNNER_PHP" "pg_log(\"DISCOVERY: dirs=\"" "Discovery summary is logged"
assert_contains "$RUNNER_PHP" 'function pg_parse_phpunit_args(array $argv)' "PHPUnit passthrough parser exists"
assert_contains "$RUNNER_PHP" "if (\$arg === '--filter')" "Separate --filter arg is supported"
assert_contains "$RUNNER_PHP" "if (strpos(\$arg, '--filter=') === 0)" "Equals --filter arg is supported"
assert_contains "$RUNNER_PHP" "if (\$arg === '--list-tests')" "--list-tests arg is supported"
assert_contains "$RUNNER_PHP" "\$arguments['filter']" "Filter reaches PHPUnit arguments"
assert_contains "$RUNNER_PHP" "\$arguments['listTests'] = true" "listTests reaches PHPUnit arguments"
assert_contains "$RUNNER_PHP" 'pg_parse_phpunit_args($argv ?? [])' "Parsed args feed TestRunner"
assert_contains "$RUNNER_PHP" 'function pg_print_test_list($test)' "--list-tests prints discovered suite names"
assert_contains "$RUNNER_PHP" "if (!empty(\$phpunit_args['listTests']))" "--list-tests exits before running assertions"

assert_contains "$PHPUNIT_XML" '<directory>tests/</directory>' "Default suite points at component tests/"
assert_contains "$PHPUNIT_XML" '<directory suffix="UnitTest.php">HomeboyWordPress/Tests/</directory>' "Extension self-tests stay configured"

assert_not_contains "$RUNNER_PHP" "\$plugin_path/HomeboyWordPress/Tests" "Runner does not hard-code extension self-test path as component path"

echo "OK: Playground test discovery smoke passed"
