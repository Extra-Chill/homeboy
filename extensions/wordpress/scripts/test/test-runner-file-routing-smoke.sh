#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq -- "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq -- "$unexpected" "$file"; then
        echo "Expected $file not to contain: $unexpected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

component="${TMPDIR}/component"
mkdir -p "${component}/tests/Unit" "${TMPDIR}/stubs"

cat > "${component}/tests/import-agent-ability-smoke.php" <<'PHP'
<?php
fwrite( STDOUT, "standalone smoke ran\n" );
PHP

cat > "${component}/tests/queue-routing-smoke.php" <<'PHP'
<?php
fwrite( STDOUT, "queue routing smoke ran\n" );
PHP

cat > "${component}/tests/Unit/ImportAgentAbilityTest.php" <<'PHP'
<?php
// PHPUnit-shaped file; the WP Codebox backend owns execution.
PHP

cat > "${component}/tests/helper.php" <<'PHP'
<?php
// Not a standalone smoke script or PHPUnit test case.
PHP

cat > "${TMPDIR}/stubs/playground.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "PLAYGROUND_STUB"
echo "SELECTED=${HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE:-}"
printf 'CHANGED=%s\n' "${HOMEBOY_CHANGED_TEST_FILES:-}"
printf 'ARGS=%s\n' "$*"
SH
chmod +x "${TMPDIR}/stubs/playground.sh"

cat > "${TMPDIR}/stubs/wp-codebox.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "WP_CODEBOX_STUB"
echo "SELECTED=${HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE:-}"
printf 'CHANGED=%s\n' "${HOMEBOY_CHANGED_TEST_FILES:-}"
printf 'ARGS=%s\n' "$*"
if [ -n "${WP_CODEBOX_ARGS_FILE:-}" ]; then
    printf '%s\n' "$@" > "${WP_CODEBOX_ARGS_FILE}"
fi
component_path=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--mount" ]; then
        shift
        case "${1:-}" in
            *:/wordpress/wp-content/plugins/component)
                component_path="${1%:/wordpress/wp-content/plugins/component}"
                ;;
        esac
    fi
    shift || true
done
if [ -n "$component_path" ]; then
    printf 'ALL TESTS PASSED\nTESTS: 1 FAILURES: 0 ERRORS: 0\n' > "${component_path}/.pg-test-result.txt"
fi
printf '{"execution":{"stdout":"OK (1 test, 1 assertion)\n","stderr":""}}\n'
SH
chmod +x "${TMPDIR}/stubs/wp-codebox.sh"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/import-agent-ability-smoke.php > "${TMPDIR}/smoke-file.out"

assert_contains "${TMPDIR}/smoke-file.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/smoke-file.out" "standalone smoke ran"
assert_not_contains "${TMPDIR}/smoke-file.out" "PLAYGROUND_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_PLAYGROUND="${TMPDIR}/stubs/playground.sh" \
HOMEBOY_CHANGED_TEST_FILES=$'tests/import-agent-ability-smoke.php\ntests/queue-routing-smoke.php' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/changed-smoke-files.out"

assert_contains "${TMPDIR}/changed-smoke-files.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/changed-smoke-files.out" "HOST_SMOKE_BEGIN:tests/queue-routing-smoke.php"
assert_contains "${TMPDIR}/changed-smoke-files.out" "HOST_SMOKE_SUMMARY:passed=2 failed=0"
assert_not_contains "${TMPDIR}/changed-smoke-files.out" "PLAYGROUND_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/Unit/ImportAgentAbilityTest.php --filter ImportAgent > "${TMPDIR}/phpunit-file.out"

assert_contains "${TMPDIR}/phpunit-file.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/phpunit-file.out" "SELECTED=tests/Unit/ImportAgentAbilityTest.php"
assert_contains "${TMPDIR}/phpunit-file.out" "ARGS=--filter ImportAgent"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
HOMEBOY_CHANGED_TEST_FILES=$'tests/import-agent-ability-smoke.php\ntests/Unit/ImportAgentAbilityTest.php' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/changed-mixed-files.out"

assert_contains "${TMPDIR}/changed-mixed-files.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/changed-mixed-files.out" "CHANGED=tests/import-agent-ability-smoke.php"
assert_not_contains "${TMPDIR}/changed-mixed-files.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"

WP_CODEBOX_ARGS_FILE="${TMPDIR}/wp-codebox-args.txt" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WP_CODEBOX_BIN="${TMPDIR}/stubs/wp-codebox.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/Unit/ImportAgentAbilityTest.php --filter ImportAgent > "${TMPDIR}/wp-codebox-file.out"

assert_contains "${TMPDIR}/wp-codebox-file.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/wp-codebox-file.out" "Backend: wp-codebox"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "run"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "--command"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "wordpress.run-php"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "--arg"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "--wp"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "6.9"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "${component}:/wordpress/wp-content/plugins/component"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_SETTINGS_JSON='{"wp_codebox_bin":"'"${TMPDIR}/stubs/wp-codebox.sh"'","playground_wordpress_version":"latest"}' \
WP_CODEBOX_ARGS_FILE="${TMPDIR}/wp-codebox-settings-args.txt" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/Unit/ImportAgentAbilityTest.php > "${TMPDIR}/wp-codebox-settings.out"

assert_contains "${TMPDIR}/wp-codebox-settings.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/wp-codebox-settings-args.txt" "latest"

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/helper.php > "${TMPDIR}/unclassified.out" 2>&1
status=$?
set -e

if [ "$status" -ne 2 ]; then
    echo "Expected unclassified file to exit 2, got $status" >&2
    sed 's/^/  /' "${TMPDIR}/unclassified.out" >&2
    exit 1
fi
assert_contains "${TMPDIR}/unclassified.out" "ERROR: cannot classify requested WordPress test file: tests/helper.php"

echo "Test runner file routing smoke passed"
