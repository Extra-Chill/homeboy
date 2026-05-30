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
recipe_path=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--recipe" ]; then
        shift
        recipe_path="${1:-}"
    fi
    shift || true
done
if [ -n "$recipe_path" ] && [ -f "$recipe_path" ]; then
    if [ -n "${WP_CODEBOX_ARGS_FILE:-}" ]; then
        printf '\n--- recipe ---\n' >> "${WP_CODEBOX_ARGS_FILE}"
        cat "$recipe_path" >> "${WP_CODEBOX_ARGS_FILE}"
    fi
    component_path="$(jq -r '.inputs.mounts[]? | select(.target == "/wordpress/wp-content/plugins/component") | .source' "$recipe_path" | head -n 1)"
fi
if [ -n "$component_path" ]; then
    if [ "${WP_CODEBOX_STUB_REGISTRATION_DRIFT:-}" = "1" ]; then
        printf 'SOME TESTS FAILED\nTESTS: 4 FAILURES: 3 ERRORS: 1\n' > "${component_path}/.pg-test-result.txt"
        printf '%s\n' '{"success":false,"executions":[{"stdout":"Abilities not registered during plugin boot: datamachine/get-flows, datamachine/create-flow\\nAbility category '\''datamachine-content'\'' should be registered during plugin boot\\nUnexpected incorrect usage notice for WP_Abilities_Registry::get_registered.\\nAbility \\\"datamachine/execute-workflow\\\" not found.\\nFailed asserting that an array has the key '\''image_generation'\''.\\nFailed asserting that an array has the key '\''web_fetch'\''.\\n","stderr":""}]}'
        exit 1
    fi
    printf 'ALL TESTS PASSED\nTESTS: 1 FAILURES: 0 ERRORS: 0\n' > "${component_path}/.pg-test-result.txt"
fi
printf '{"success":true,"executions":[{"stdout":"OK (1 test, 1 assertion)\n","stderr":""}]}\n'
SH
chmod +x "${TMPDIR}/stubs/wp-codebox.sh"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/import-agent-ability-smoke.php > "${TMPDIR}/smoke-file.out"

assert_contains "${TMPDIR}/smoke-file.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/smoke-file.out" "standalone smoke ran"
assert_not_contains "${TMPDIR}/smoke-file.out" "WP_CODEBOX_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES=$'tests/import-agent-ability-smoke.php\ntests/queue-routing-smoke.php' \
HOMEBOY_TEST_SCOPE_KIND="exclusive_env" \
HOMEBOY_TEST_SCOPE_ENV_NAME="HOMEBOY_WORDPRESS_HOST_SMOKE_FILES" \
HOMEBOY_TEST_SCOPE_ENV_VALUE=$'tests/import-agent-ability-smoke.php\ntests/queue-routing-smoke.php' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/changed-smoke-files.out"

assert_contains "${TMPDIR}/changed-smoke-files.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/changed-smoke-files.out" "HOST_SMOKE_BEGIN:tests/queue-routing-smoke.php"
assert_contains "${TMPDIR}/changed-smoke-files.out" "HOST_SMOKE_SUMMARY:passed=2 failed=0"
assert_not_contains "${TMPDIR}/changed-smoke-files.out" "WP_CODEBOX_STUB"

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
assert_contains "${TMPDIR}/wp-codebox-args.txt" "recipe-run"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "--recipe"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "wordpress.phpunit"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "autoload-file=/wp-codebox-vendor/autoload.php"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "6.9"
assert_contains "${TMPDIR}/wp-codebox-args.txt" '"target": "/wordpress/wp-content/plugins/component"'

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_SETTINGS_JSON='{"wp_codebox_bin":"'"${TMPDIR}/stubs/wp-codebox.sh"'","wp_codebox_wordpress_version":"latest"}' \
WP_CODEBOX_ARGS_FILE="${TMPDIR}/wp-codebox-settings-args.txt" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/Unit/ImportAgentAbilityTest.php > "${TMPDIR}/wp-codebox-settings.out"

assert_contains "${TMPDIR}/wp-codebox-settings.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/wp-codebox-settings-args.txt" "latest"

set +e
WP_CODEBOX_STUB_REGISTRATION_DRIFT=1 \
HOMEBOY_CHANGED_SINCE="origin/main" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WP_CODEBOX_BIN="${TMPDIR}/stubs/wp-codebox.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/registration-drift.out" 2>&1
status=$?
set -e

if [ "$status" -ne 1 ]; then
    echo "Expected registration drift preflight to exit 1, got $status" >&2
    sed 's/^/  /' "${TMPDIR}/registration-drift.out" >&2
    exit 1
fi
assert_contains "${TMPDIR}/registration-drift.out" "HARNESS PREFLIGHT FAILURE: WordPress bootstrap registration drift"
assert_contains "${TMPDIR}/registration-drift.out" "Changed-since PHPUnit hit broad missing registration drift"
assert_contains "${TMPDIR}/registration-drift.out" "changed-since: origin/main"

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
