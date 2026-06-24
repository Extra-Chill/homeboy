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
mkdir -p "${component}/tests/Unit" "${component}/wordpress/tests" "${TMPDIR}/stubs"

cat > "${component}/tests/import-agent-ability-smoke.php" <<'PHP'
<?php
fwrite( STDOUT, "standalone smoke ran\n" );
PHP

cat > "${component}/tests/queue-routing-smoke.php" <<'PHP'
<?php
fwrite( STDOUT, "queue routing smoke ran\n" );
PHP

cat > "${component}/tests/codebox-agent-task-matrix-smoke.js" <<'JS'
console.log('codebox agent task matrix smoke ran');
JS

cat > "${component}/tests/shell-contract-smoke.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "shell contract smoke ran"
SH
chmod +x "${component}/tests/shell-contract-smoke.sh"

cat > "${component}/wordpress/tests/codebox-agent-task-matrix-smoke.js" <<'JS'
console.log('prefixed codebox agent task matrix smoke ran');
JS

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
if [ "${1:-}" = "recipe" ] && [ "${2:-}" = "build" ] && [ "${3:-}" = "phpunit" ]; then
    options_path=""
    output_path=""
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --options)
                shift
                options_path="${1:-}"
                ;;
            --output)
                shift
                output_path="${1:-}"
                ;;
        esac
        shift || true
    done
    node - "$options_path" "$output_path" <<'NODE'
const fs = require('node:fs')
const options = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'))
const recipe = {
  schema: 'wp-codebox/workspace-recipe/v1',
  runtime: { wp: options.wordpressVersion, blueprint: { steps: [] } },
  inputs: { mounts: options.mounts || [] },
  workflow: { steps: [{ command: 'wordpress.phpunit', args: [
    `plugin-slug=${options.pluginSlug}`,
    `test-file=${options.selectedTestFile || ''}`,
    `changed-tests-json=${JSON.stringify(options.changedTestFiles || [])}`,
    `env-json=${JSON.stringify(options.env || {})}`,
    `wp-config-defines-json=${JSON.stringify(options.wpConfigDefines || {})}`,
    `autoload-file=${options.autoloadFile}`,
    `tests-dir=${options.testsDir}`,
    `dependency-mounts=${(options.dependencyMounts || []).filter(Boolean).join(',')}`,
    `multisite=${options.multisite ? '1' : '0'}`,
  ] }] },
}
fs.writeFileSync(process.argv[3], `${JSON.stringify(recipe, null, 2)}\n`)
NODE
    exit 0
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
    if [ "${WP_CODEBOX_STUB_NO_PHPUNIT:-}" = "1" ]; then
        printf 'NO_TEST_FILES\n' > "${component_path}/.pg-test-result.txt"
        printf '%s\n' '{"success":false,"executions":[{"stdout":"wordpress.phpunit crashed before producing a structured response\n","stderr":""}]}'
        exit 1
    fi
    if [ "${WP_CODEBOX_STUB_REGISTRATION_DRIFT:-}" = "1" ]; then
        printf 'SOME TESTS FAILED\nTESTS: 4 FAILURES: 3 ERRORS: 1\n' > "${component_path}/.pg-test-result.txt"
        printf '%s\n' '{"success":false,"executions":[{"stdout":"Abilities not registered during plugin boot: datamachine/get-flows, datamachine/create-flow\\nAbility category '\''datamachine-content'\'' should be registered during plugin boot\\nUnexpected incorrect usage notice for WP_Abilities_Registry::get_registered.\\nAbility \\\"datamachine/execute-workflow\\\" not found.\\nFailed asserting that an array has the key '\''image_generation'\''.\\nFailed asserting that an array has the key '\''web_fetch'\''.\\n","stderr":""}]}'
        exit 1
    fi
    printf 'ALL TESTS PASSED\nTESTS: 1 FAILURES: 0 ERRORS: 0\n' > "${component_path}/.pg-test-result.txt"
    printf '{}\n' > "${component_path}/.phpunit.result.cache"
fi
printf '{"success":true,"executions":[{"stdout":"OK (1 test, 1 assertion)\n","stderr":""}]}\n'
SH
chmod +x "${TMPDIR}/stubs/wp-codebox.sh"

cat > "${TMPDIR}/stubs/phpunit-recipe-builder.mjs" <<'NODE'
#!/usr/bin/env node
import fs from 'node:fs';

const input = JSON.parse(fs.readFileSync(0, 'utf8'));
const options = input.options || input;
const recipe = {
  schema: 'wp-codebox/workspace-recipe/v1',
  runtime: { wp: options.wordpressVersion, blueprint: { steps: [] } },
  inputs: { mounts: options.mounts || [] },
  workflow: { steps: [{ command: 'wordpress.phpunit', args: [
    `plugin-slug=${options.pluginSlug}`,
    `test-file=${options.selectedTestFile || ''}`,
    `changed-tests-json=${JSON.stringify(options.changedTestFiles || [])}`,
    `env-json=${JSON.stringify(options.env || {})}`,
    `wp-config-defines-json=${JSON.stringify(options.wpConfigDefines || {})}`,
    `autoload-file=${options.autoloadFile}`,
    `tests-dir=${options.testsDir}`,
    `dependency-mounts=${(options.dependencyMounts || []).filter(Boolean).join(',')}`,
    `multisite=${options.multisite ? '1' : '0'}`,
  ] }] },
};
process.stdout.write(`${JSON.stringify(recipe, null, 2)}\n`);
NODE
chmod +x "${TMPDIR}/stubs/phpunit-recipe-builder.mjs"

# Stub for the real-WordPress smoke runner. The real runner boots WordPress via
# WP Codebox; for routing assertions we only need to confirm smoke files reach it
# and the HOST_SMOKE marker contract, so this stub emits those markers for each
# selected file without booting anything.
cat > "${TMPDIR}/stubs/host-smoke-wp.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
PLUGIN_PATH="${HOMEBOY_COMPONENT_PATH:-$(pwd)}"
files=()
if [ -n "${HOMEBOY_WORDPRESS_HOST_SMOKE_FILES:-}" ]; then
    while IFS= read -r f; do [ -n "$f" ] && files+=("$f"); done <<< "${HOMEBOY_WORDPRESS_HOST_SMOKE_FILES}"
elif [ -n "${HOMEBOY_WORDPRESS_HOST_SMOKE_FILE:-}" ]; then
    files=("${HOMEBOY_WORDPRESS_HOST_SMOKE_FILE}")
else
    while IFS= read -r f; do files+=("${f#"${PLUGIN_PATH}/"}"); done < <(find "${PLUGIN_PATH}/tests" -type f -name '*-smoke.php' | sort)
fi
echo "Backend: host-smoke-wp"
passed=0
for f in "${files[@]}"; do
    echo "HOST_SMOKE_BEGIN:${f}"
    echo "standalone smoke ran"
    echo "HOST_SMOKE_OK:${f}"
    passed=$((passed + 1))
done
echo "HOST_SMOKE_SUMMARY:passed=${passed} failed=0"
SH
chmod +x "${TMPDIR}/stubs/host-smoke-wp.sh"


cat > "${TMPDIR}/stubs/composer" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" != "test" ]; then
    echo "Unexpected composer command: $*" >&2
    exit 2
fi
echo "contract smoke passed"
SH
chmod +x "${TMPDIR}/stubs/composer"

cat > "${TMPDIR}/stubs/npm" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" != "run" ] || [ "${2:-}" != "headless-preview-boot-smoke" ]; then
    echo "Unexpected npm command: $*" >&2
    exit 2
fi
echo "headless preview boot smoke passed"
SH
chmod +x "${TMPDIR}/stubs/npm"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WORDPRESS_TEST_RUNTIME_BACKEND="experimental-runtime" \
HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${TMPDIR}/stubs/host-smoke-wp.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/import-agent-ability-smoke.php > "${TMPDIR}/smoke-file.out"

assert_contains "${TMPDIR}/smoke-file.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/smoke-file.out" "standalone smoke ran"
assert_not_contains "${TMPDIR}/smoke-file.out" "WP_CODEBOX_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${TMPDIR}/stubs/host-smoke-wp.sh" \
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
HOMEBOY_CHANGED_TEST_FILES='tests/codebox-agent-task-matrix-smoke.js' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/changed-js-smoke-files.out"

assert_contains "${TMPDIR}/changed-js-smoke-files.out" "JS_SMOKE_BEGIN:tests/codebox-agent-task-matrix-smoke.js"
assert_contains "${TMPDIR}/changed-js-smoke-files.out" "codebox agent task matrix smoke ran"
assert_contains "${TMPDIR}/changed-js-smoke-files.out" "JS_SMOKE_SUMMARY:passed=1 failed=0"
assert_not_contains "${TMPDIR}/changed-js-smoke-files.out" "WP_CODEBOX_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
HOMEBOY_CHANGED_TEST_FILES='wordpress/tests/codebox-agent-task-matrix-smoke.js' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/changed-prefixed-js-smoke-files.out"

assert_contains "${TMPDIR}/changed-prefixed-js-smoke-files.out" "JS_SMOKE_BEGIN:wordpress/tests/codebox-agent-task-matrix-smoke.js"
assert_contains "${TMPDIR}/changed-prefixed-js-smoke-files.out" "prefixed codebox agent task matrix smoke ran"
assert_contains "${TMPDIR}/changed-prefixed-js-smoke-files.out" "JS_SMOKE_SUMMARY:passed=1 failed=0"
assert_not_contains "${TMPDIR}/changed-prefixed-js-smoke-files.out" "WP_CODEBOX_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
HOMEBOY_CHANGED_TEST_FILES='wordpress/tests/shell-contract-smoke.sh' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/changed-prefixed-shell-smoke-files.out"

assert_contains "${TMPDIR}/changed-prefixed-shell-smoke-files.out" "SHELL_SMOKE_BEGIN:tests/shell-contract-smoke.sh"
assert_contains "${TMPDIR}/changed-prefixed-shell-smoke-files.out" "shell contract smoke ran"
assert_contains "${TMPDIR}/changed-prefixed-shell-smoke-files.out" "SHELL_SMOKE_SUMMARY:passed=1 failed=0"
assert_not_contains "${TMPDIR}/changed-prefixed-shell-smoke-files.out" "WP_CODEBOX_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/codebox-agent-task-matrix-smoke.js > "${TMPDIR}/js-smoke-file.out"

assert_contains "${TMPDIR}/js-smoke-file.out" "JS_SMOKE_BEGIN:tests/codebox-agent-task-matrix-smoke.js"
assert_contains "${TMPDIR}/js-smoke-file.out" "codebox agent task matrix smoke ran"
assert_contains "${TMPDIR}/js-smoke-file.out" "JS_SMOKE_SUMMARY:passed=1 failed=0"
assert_not_contains "${TMPDIR}/js-smoke-file.out" "WP_CODEBOX_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file wordpress/tests/shell-contract-smoke.sh > "${TMPDIR}/shell-smoke-file.out"

assert_contains "${TMPDIR}/shell-smoke-file.out" "SHELL_SMOKE_BEGIN:tests/shell-contract-smoke.sh"
assert_contains "${TMPDIR}/shell-smoke-file.out" "shell contract smoke ran"
assert_contains "${TMPDIR}/shell-smoke-file.out" "SHELL_SMOKE_SUMMARY:passed=1 failed=0"
assert_not_contains "${TMPDIR}/shell-smoke-file.out" "WP_CODEBOX_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WORDPRESS_TEST_RUNTIME_BACKEND="wp-codebox" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER="${TMPDIR}/stubs/phpunit-recipe-builder.mjs" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/Unit/ImportAgentAbilityTest.php --filter ImportAgent > "${TMPDIR}/phpunit-file.out"

assert_contains "${TMPDIR}/phpunit-file.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/phpunit-file.out" "SELECTED=tests/Unit/ImportAgentAbilityTest.php"
assert_contains "${TMPDIR}/phpunit-file.out" "ARGS=--filter ImportAgent"

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WORDPRESS_TEST_RUNTIME_BACKEND="experimental-runtime" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/Unit/ImportAgentAbilityTest.php > "${TMPDIR}/unsupported-runtime.out" 2>&1
status=$?
set -e

if [ "$status" -ne 2 ]; then
    echo "Expected unsupported runtime backend to exit 2, got $status" >&2
    sed 's/^/  /' "${TMPDIR}/unsupported-runtime.out" >&2
    exit 1
fi
assert_contains "${TMPDIR}/unsupported-runtime.out" "ERROR: unsupported WordPress test runtime backend: experimental-runtime"
assert_contains "${TMPDIR}/unsupported-runtime.out" "Supported backends: wp-codebox"

# A mixed smoke + PHPUnit changeset has no single exclusive scope, so the run
# falls through to the canonical full-suite PHPUnit runtime backend. Ad hoc PHP
# smokes still require explicit --file/--host-smoke-file selection.
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER="${TMPDIR}/stubs/phpunit-recipe-builder.mjs" \
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
HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER="${TMPDIR}/stubs/phpunit-recipe-builder.mjs" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/Unit/ImportAgentAbilityTest.php --filter ImportAgent > "${TMPDIR}/wp-codebox-file.out"

assert_contains "${TMPDIR}/wp-codebox-file.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/wp-codebox-file.out" "Backend: wp-codebox"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "recipe-run"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "--recipe"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "wordpress.phpunit"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "autoload-file=/wp-codebox-vendor/autoload.php"
assert_not_contains "${TMPDIR}/wp-codebox-args.txt" "6.9"
assert_contains "${TMPDIR}/wp-codebox-args.txt" '"target": "/wordpress/wp-content/plugins/component"'
if [ -e "${component}/.phpunit.result.cache" ]; then
    echo "Expected WP Codebox runner to clean PHPUnit result cache" >&2
    exit 1
fi

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_SETTINGS_JSON='{"wp_codebox_bin":"'"${TMPDIR}/stubs/wp-codebox.sh"'","wordpress_runtime_version":"latest"}' \
HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER="${TMPDIR}/stubs/phpunit-recipe-builder.mjs" \
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
HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER="${TMPDIR}/stubs/phpunit-recipe-builder.mjs" \
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

no_phpunit_component="${TMPDIR}/no-phpunit-component"
mkdir -p "${no_phpunit_component}/tests"
cat > "${no_phpunit_component}/composer.json" <<'JSON'
{"scripts":{"test":"php tests/contract-smoke.php"}}
JSON

WP_CODEBOX_STUB_NO_PHPUNIT=1 \
PATH="${TMPDIR}/stubs:${PATH}" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$no_phpunit_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WP_CODEBOX_BIN="${TMPDIR}/stubs/wp-codebox.sh" \
HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER="${TMPDIR}/stubs/phpunit-recipe-builder.mjs" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/no-phpunit-composer.out" 2>&1

assert_contains "${TMPDIR}/no-phpunit-composer.out" "Running Composer test script"
assert_contains "${TMPDIR}/no-phpunit-composer.out" "contract smoke passed"
assert_not_contains "${TMPDIR}/no-phpunit-composer.out" "NO PHPUNIT TEST FILES DISCOVERED"
assert_not_contains "${TMPDIR}/no-phpunit-composer.out" "wordpress.phpunit crashed before producing a structured response"

no_phpunit_npm_component="${TMPDIR}/no-phpunit-npm-component"
mkdir -p "${no_phpunit_npm_component}/tests"
cat > "${no_phpunit_npm_component}/package.json" <<'JSON'
{"scripts":{"headless-preview-boot-smoke":"node tests/headless-preview-boot-smoke.mjs"}}
JSON

WP_CODEBOX_STUB_NO_PHPUNIT=1 \
PATH="${TMPDIR}/stubs:${PATH}" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$no_phpunit_npm_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_SETTINGS_JSON='{"npm_test_script":"headless-preview-boot-smoke"}' \
HOMEBOY_WP_CODEBOX_BIN="${TMPDIR}/stubs/wp-codebox.sh" \
HOMEBOY_WP_CODEBOX_PHPUNIT_RECIPE_BUILDER="${TMPDIR}/stubs/phpunit-recipe-builder.mjs" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/no-phpunit-npm.out" 2>&1

assert_contains "${TMPDIR}/no-phpunit-npm.out" "Running npm test script"
assert_contains "${TMPDIR}/no-phpunit-npm.out" "Script: headless-preview-boot-smoke"
assert_contains "${TMPDIR}/no-phpunit-npm.out" "headless preview boot smoke passed"
assert_not_contains "${TMPDIR}/no-phpunit-npm.out" "NO PHPUNIT TEST FILES DISCOVERED"
assert_not_contains "${TMPDIR}/no-phpunit-npm.out" "wordpress.phpunit crashed before producing a structured response"

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
