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
mkdir -p "${component}/tests/Unit/Support" "${component}/wordpress/tests" "${component}/tools" "${component}/bin/tests/i18n-tools" "${TMPDIR}/stubs"
runner_prelude="${TMPDIR}/runner-prelude.sh"
cat > "${runner_prelude}" <<'SH'
homeboy_runner_init() {
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
    PLUGIN_PATH="$COMPONENT_PATH"
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:?HOMEBOY_EXTENSION_PATH is required}"
}
SH
export HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude"

cat > "${TMPDIR}/validation-dependencies.sh" <<'SH'
homeboy_export_validation_dependency_paths() {
    if [ "${TEST_DEPENDENCY_FAILURE:-0}" = 1 ]; then
        echo "dependency resolution failed" >&2
        return 17
    fi
    mkdir -p "${1}/resolved-dependency"
    export HOMEBOY_WORDPRESS_DEPENDENCY_PATHS="${1}/resolved-dependency"
}
SH
export HOMEBOY_RUNTIME_VALIDATION_DEPENDENCIES="${TMPDIR}/validation-dependencies.sh"

(
    source "${EXTENSION_PATH}/scripts/lib/validation-dependencies.sh"
    for suffix in default 0123456789abcdef0123456789abcdef01234567; do
        dependency="${TMPDIR}/fixture-plugin-${suffix}"
        mkdir -p "$dependency"
        touch "${dependency}/fixture-plugin.php"
        [ "$(homeboy_get_validation_dependency_slug "$dependency")" = fixture-plugin ] || exit 1
    done
)

results_writer="${TMPDIR}/write-test-results.sh"
cat > "$results_writer" <<'SH'
homeboy_write_test_results() {
    jq -n --argjson total "$1" --argjson passed "$2" --argjson failed "$3" --argjson skipped "$4" --arg source "$5" \
        '{total: $total, passed: $passed, failed: $failed, skipped: $skipped, source: $source}' > "$HOMEBOY_TEST_RESULTS_FILE"
}
SH

# Simulate a caller with a managed Codebox installation whose CLI and core module
# are incompatible with this fixture. The smoke owns its runtime inputs below.
ambient_install_dir="${TMPDIR}/ambient-codebox-install"
ambient_wp_codebox_bin="${ambient_install_dir}/source/packages/cli/dist/index.js"
ambient_core_module="${TMPDIR}/stubs/ambient-wp-codebox-core.mjs"
mkdir -p "$(dirname "${ambient_wp_codebox_bin}")"
cat > "${ambient_wp_codebox_bin}" <<'NODE'
#!/usr/bin/env node
process.stderr.write('AMBIENT_WP_CODEBOX_CLI_SELECTED\n');
process.exit(1);
NODE
chmod +x "${ambient_wp_codebox_bin}"
cat > "${ambient_core_module}" <<'NODE'
export const incompatibleAmbientCodeboxModule = true;
NODE

HOMEBOY_WP_CODEBOX_INSTALL_DIR="${ambient_install_dir}"
HOMEBOY_SETTINGS_WP_CODEBOX_BIN="${ambient_wp_codebox_bin}"
HOMEBOY_WP_CODEBOX_BIN="${ambient_wp_codebox_bin}"
HOMEBOY_SETTINGS_WP_CODEBOX_CORE_MODULE="${ambient_core_module}"
HOMEBOY_WP_CODEBOX_CORE_MODULE="${ambient_core_module}"
WP_CODEBOX_CORE_MODULE="${ambient_core_module}"
HOMEBOY_SETTINGS_JSON='{"wp_codebox_bin":"'"${ambient_wp_codebox_bin}"'","wp_codebox_core_module":"'"${ambient_core_module}"'"}'

unset HOMEBOY_SETTINGS_WP_CODEBOX_BIN HOMEBOY_WP_CODEBOX_BIN
unset HOMEBOY_SETTINGS_WP_CODEBOX_CORE_MODULE HOMEBOY_WP_CODEBOX_CORE_MODULE WP_CODEBOX_CORE_MODULE
unset HOMEBOY_SETTINGS_JSON
HOMEBOY_WP_CODEBOX_INSTALL_DIR="${TMPDIR}/fixture-codebox-install"
HOMEBOY_WP_CODEBOX_CORE_MODULE="${EXTENSION_PATH}/tests/fixtures/wp-codebox-core-recipe-builder.mjs"
export HOMEBOY_WP_CODEBOX_INSTALL_DIR HOMEBOY_WP_CODEBOX_CORE_MODULE

cat > "${component}/tests/import-agent-ability-smoke.php" <<'PHP'
<?php
if ( getenv( 'HOMEBOY_WORDPRESS_DEPENDENCY_PATHS' ) !== getenv( 'HOMEBOY_COMPONENT_PATH' ) . '/resolved-dependency' ) {
    throw new RuntimeException( 'Standalone dependency paths were not exported before execution.' );
}
fwrite( STDOUT, "standalone smoke ran\n" );
PHP

cat > "${component}/tests/queue-routing-smoke.php" <<'PHP'
<?php
fwrite( STDOUT, "queue routing smoke ran\n" );
PHP

cat > "${component}/tests/worktree-command-help-snapshots.php" <<'PHP'
<?php
fwrite( STDOUT, "settings-declared standalone test ran\n" );
PHP

cat > "${component}/tests/failing-smoke.php" <<'PHP'
<?php
fwrite( STDERR, "failing smoke\n" );
exit( 3 );
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

cat > "${component}/tools/fixture-matrix.test.mjs" <<'JS'
import test from 'node:test';
import assert from 'node:assert/strict';

test('composed Node test runs', () => assert.equal(2 + 2, 4));
JS

cat > "${component}/wordpress/tests/codebox-agent-task-matrix-smoke.js" <<'JS'
console.log('prefixed codebox agent task matrix smoke ran');
JS

cat > "${component}/tests/Unit/ImportAgentAbilityTest.php" <<'PHP'
<?php
// PHPUnit-shaped file; the WP Codebox backend owns execution.
PHP

cat > "${component}/tests/Unit/Support/TestDoubles.php" <<'PHP'
<?php
// Shared helper; intentionally not an executable test case.
PHP

cat > "${component}/bin/tests/i18n-tools/ExtractTest.php" <<'PHP'
<?php
// PHPUnit-shaped file under a configured non-default test root.
PHP

cat > "${component}/tests/helper.php" <<'PHP'
<?php
// Not a standalone smoke script or PHPUnit test case.
PHP

cat > "${TMPDIR}/stubs/wp-codebox.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
    printf '0.21.0\n'
    exit 0
fi
if [ "${1:-}" = "runtime" ] && [ "${2:-}" = "descriptor" ] && [ "${3:-}" = "--json" ]; then
    printf '%s\n' '{"schema":"wp-codebox/runtime-descriptor/v1","readiness":{"status":"available","browserRuntime":{"status":"ready"}},"contractManifest":{"schemas":{"runtimeBoundary":{"browserContainedSiteOpen":"wp-codebox/browser-contained-site-open/v1"}}}}'
    exit 0
fi
echo "WP_CODEBOX_STUB"
echo "SELECTED=${HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE:-}"
echo "CORE_MODULE=${HOMEBOY_WP_CODEBOX_CORE_MODULE:-}"
printf 'CHANGED=%s\n' "${HOMEBOY_CHANGED_TEST_FILES:-}"
printf 'PHPUNIT_CHANGED=%s\n' "${HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES:-}"
printf 'NODE_OPTIONS=%s\n' "${NODE_OPTIONS:-}"
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
    `test-root=${options.testRoot || ''}`,
    `dependency-mounts=${(options.dependencyMounts || []).filter(Boolean).join(',')}`,
    `multisite=${options.multisite ? '1' : '0'}`,
  ] }] },
}
fs.writeFileSync(process.argv[3], `${JSON.stringify(recipe, null, 2)}\n`)
NODE
    # Per-invocation options capture, used by the suite-membership section to
    # attribute each recipe's options to the suite that produced it.
    if [ -n "${WP_CODEBOX_OPTIONS_LOG:-}" ] && [ -n "$options_path" ]; then
        printf '=== OPTIONS ===\n' >> "${WP_CODEBOX_OPTIONS_LOG}"
        cat "$options_path" >> "${WP_CODEBOX_OPTIONS_LOG}"
    fi
    exit 0
fi
component_path=""
recipe_path=""
artifacts_path=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--recipe" ]; then
        shift
        recipe_path="${1:-}"
    elif [ "$1" = "--artifacts" ]; then
        shift
        artifacts_path="${1:-}"
    fi
    shift || true
done

# WP Codebox publishes a runtime pointer plus a structured result sidecar, and
# the adapter's artifact handoff is built on both: without them validTestResults
# is false, the status resolves to `unknown`, and the runner exits non-zero
# whatever the run actually did. A stub that omits them is not modelling the
# contract it stands in for. See Extra-Chill/homeboy#12025.
homeboy_stub_publish_runtime_artifacts() {
    local total="$1" passed="$2" failed="$3" status="$4" stage_log="$5"
    [ -n "$artifacts_path" ] || return 0
    local runtime_dir="${artifacts_path}/runtime-fixture"
    mkdir -p "${runtime_dir}/files/phpunit"
    printf '{"paths":{"runtimeDirectory":"runtime-fixture"}}\n' > "${artifacts_path}/latest-runtime.json"
    printf '%s\n' "$stage_log" > "${runtime_dir}/files/phpunit/.pg-test-result.txt"
    cat > "${runtime_dir}/files/test-results.json" <<JSON
{
  "schema": "wp-codebox/test-results/v1",
  "status": "${status}",
  "summary": { "total": ${total}, "passed": ${passed}, "failed": ${failed}, "skipped": 0, "unknown": 0 },
  "suites": [],
  "rawLogReferences": []
}
JSON
}
if [ -n "$recipe_path" ] && [ -f "$recipe_path" ]; then
    if [ -n "${WP_CODEBOX_ARGS_FILE:-}" ]; then
        printf '\n--- recipe ---\n' >> "${WP_CODEBOX_ARGS_FILE}"
        cat "$recipe_path" >> "${WP_CODEBOX_ARGS_FILE}"
    fi
    component_path="$(jq -r '.inputs.mounts[]? | select(.target == "/wordpress/wp-content/plugins/component") | .source' "$recipe_path" | head -n 1)"
fi
# The component mount is no longer an inputs.mounts entry — the CLI derives it —
# so the recipe lookup above now yields nothing and every failure-injection
# branch below silently stopped firing. Fall back to the component path the
# runner already exports.
if [ -z "$component_path" ]; then
    component_path="${HOMEBOY_COMPONENT_PATH:-}"
fi
if [ -n "$component_path" ]; then
    if [ "${WP_CODEBOX_STUB_NO_PHPUNIT:-}" = "1" ]; then
        printf 'NO_TEST_FILES\n' > "${component_path}/.pg-test-result.txt"
        homeboy_stub_publish_runtime_artifacts 0 0 0 unknown 'NO_TEST_FILES'
        printf '%s\n' '{"success":false,"executions":[{"stdout":"wordpress.phpunit crashed before producing a structured response\n","stderr":""}]}'
        exit 1
    fi
    if [ "${WP_CODEBOX_STUB_REGISTRATION_DRIFT:-}" = "1" ]; then
        printf 'SOME TESTS FAILED\nTESTS: 4 FAILURES: 3 ERRORS: 1\n' > "${component_path}/.pg-test-result.txt"
        homeboy_stub_publish_runtime_artifacts 4 0 4 failed 'SOME TESTS FAILED'
        printf '%s\n' '{"success":false,"executions":[{"stdout":"Abilities not registered during plugin boot: datamachine/get-flows, datamachine/create-flow\\nAbility category '\''datamachine-content'\'' should be registered during plugin boot\\nUnexpected incorrect usage notice for WP_Abilities_Registry::get_registered.\\nAbility \\\"datamachine/execute-workflow\\\" not found.\\nFailed asserting that an array has the key '\''image_generation'\''.\\nFailed asserting that an array has the key '\''web_fetch'\''.\\n","stderr":""}]}'
        exit 1
    fi
    printf 'ALL TESTS PASSED\nTESTS: 1 FAILURES: 0 ERRORS: 0\n' > "${component_path}/.pg-test-result.txt"
    printf '{}\n' > "${component_path}/.phpunit.result.cache"
fi
homeboy_stub_publish_runtime_artifacts 1 1 0 passed 'PLUGIN_DETECTED component/component.php'
printf '{"success":true,"executions":[{"stdout":"OK (1 test, 1 assertion)\n","stderr":""}]}\n'
SH
chmod +x "${TMPDIR}/stubs/wp-codebox.sh"

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

run_changed_scope() {
    local outfile="$1"
    local changed="$2"
    local status=0

    set +e
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${TMPDIR}/stubs/host-smoke-wp.sh" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
    HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$results_writer" \
    HOMEBOY_TEST_RESULTS_FILE="${outfile}.results.json" \
    HOMEBOY_CHANGED_TEST_FILES="$changed" \
    HOMEBOY_SETTINGS_JSON='{"standalone_php_test_paths":["tests/*-command-help-snapshots.php"]}' \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "$outfile" 2>&1
    status=$?
    set -e
    return "$status"
}

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WORDPRESS_TEST_RUNTIME_BACKEND="experimental-runtime" \
HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${TMPDIR}/stubs/host-smoke-wp.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/import-agent-ability-smoke.php > "${TMPDIR}/smoke-file.out"

assert_contains "${TMPDIR}/smoke-file.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/smoke-file.out" "standalone smoke ran"
assert_not_contains "${TMPDIR}/smoke-file.out" "WP_CODEBOX_STUB"

cat > "${component}/homeboy-test-manifest.json" <<'JSON'
{
  "schema": "homeboy/test-manifest/v1",
  "default_environment": "standalone-php",
  "tests": {
    "tests/queue-routing-smoke.php": {
      "environment": "wordpress"
    }
  }
}
JSON

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

assert_contains "${TMPDIR}/changed-smoke-files.out" "PHP_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/changed-smoke-files.out" "PHP_SMOKE_OK:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/changed-smoke-files.out" "PHP_SMOKE_SUMMARY:passed=1 failed=0"

if TEST_DEPENDENCY_FAILURE=1 run_changed_scope "${TMPDIR}/dependency-failure.out" 'tests/import-agent-ability-smoke.php'; then
    echo "Expected unresolved standalone dependencies to fail the test command" >&2
    exit 1
fi
assert_contains "${TMPDIR}/dependency-failure.out" "dependency resolution failed"
assert_not_contains "${TMPDIR}/dependency-failure.out" "PHP_SMOKE_BEGIN:"

# A mixed changed scope accounts for every selection, including a settings-
# declared standalone route and a deliberately unsupported helper.
run_changed_scope "${TMPDIR}/reconciled-mixed.out" $'tests/Unit/ImportAgentAbilityTest.php\ntests/import-agent-ability-smoke.php\ntests/worktree-command-help-snapshots.php\ntests/queue-routing-smoke.php\ntests/Unit/Support/TestDoubles.php'
assert_contains "${TMPDIR}/reconciled-mixed.out" "CHANGED_SCOPE_ROUTE:tests/Unit/ImportAgentAbilityTest.php:runner=phpunit"
assert_contains "${TMPDIR}/reconciled-mixed.out" "CHANGED_SCOPE_ROUTE:tests/import-agent-ability-smoke.php:runner=host-php-smoke"
assert_contains "${TMPDIR}/reconciled-mixed.out" "CHANGED_SCOPE_ROUTE:tests/worktree-command-help-snapshots.php:runner=host-php-smoke"
assert_contains "${TMPDIR}/reconciled-mixed.out" "CHANGED_SCOPE_ROUTE:tests/queue-routing-smoke.php:runner=host-php-smoke"
assert_contains "${TMPDIR}/reconciled-mixed.out" "CHANGED_SCOPE_EXCLUDED:tests/Unit/Support/TestDoubles.php:reason=unsupported_test_shape"
assert_contains "${TMPDIR}/reconciled-mixed.out" "CHANGED_SCOPE_SUMMARY:selected=5 routed=4 excluded=1"
assert_contains "${TMPDIR}/reconciled-mixed.out" "PHP_SMOKE_OK:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/reconciled-mixed.out" "PHP_SMOKE_OK:tests/worktree-command-help-snapshots.php"
assert_contains "${TMPDIR}/reconciled-mixed.out" "HOST_SMOKE_BEGIN:tests/queue-routing-smoke.php"
assert_contains "${TMPDIR}/reconciled-mixed.out" "PHPUNIT_CHANGED=tests/Unit/ImportAgentAbilityTest.php"

# A standalone-only changed scope must not widen into PHPUnit, and its sidecar
# reports exact host-PHP counts and ownership.
run_changed_scope "${TMPDIR}/declared-only.out" 'tests/worktree-command-help-snapshots.php'
assert_contains "${TMPDIR}/declared-only.out" "CHANGED_SCOPE_SUMMARY:selected=1 routed=1 excluded=0"
assert_contains "${TMPDIR}/declared-only.out" "PHP_SMOKE_OK:tests/worktree-command-help-snapshots.php"
assert_not_contains "${TMPDIR}/declared-only.out" "WP_CODEBOX_STUB"
jq -e '.total == 1 and .passed == 1 and .failed == 0 and .skipped == 0 and .source == "changed-scope-host-php"' "${TMPDIR}/declared-only.out.results.json" >/dev/null

# Standalone failure remains authoritative both alone and when followed by a
# passing PHPUnit backend.
standalone_failure_status=0
run_changed_scope "${TMPDIR}/standalone-failure.out" 'tests/failing-smoke.php' || standalone_failure_status=$?
if [ "$standalone_failure_status" -eq 0 ]; then
    echo "Expected a failing standalone-only changed scope to fail" >&2
    exit 1
fi
assert_contains "${TMPDIR}/standalone-failure.out" "PHP_SMOKE_FAIL:tests/failing-smoke.php:exit=3"
jq -e '.total == 1 and .passed == 0 and .failed == 1 and .skipped == 0 and .source == "changed-scope-host-php"' "${TMPDIR}/standalone-failure.out.results.json" >/dev/null

mixed_failure_status=0
run_changed_scope "${TMPDIR}/mixed-failure.out" $'tests/Unit/ImportAgentAbilityTest.php\ntests/failing-smoke.php' || mixed_failure_status=$?
if [ "$mixed_failure_status" -eq 0 ]; then
    echo "Expected a passing PHPUnit backend not to erase a standalone failure" >&2
    exit 1
fi
assert_contains "${TMPDIR}/mixed-failure.out" "PHP_SMOKE_FAIL:tests/failing-smoke.php:exit=3"
assert_contains "${TMPDIR}/mixed-failure.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/mixed-failure.out" "PHPUNIT_CHANGED=tests/Unit/ImportAgentAbilityTest.php"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES="tools/fixture-matrix.test.mjs" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/node-test-file.out"

assert_contains "${TMPDIR}/node-test-file.out" "Backend: node-test"
assert_contains "${TMPDIR}/node-test-file.out" "NODE_TEST_BEGIN:tools/fixture-matrix.test.mjs"
assert_contains "${TMPDIR}/node-test-file.out" "NODE_TEST_OK:tools/fixture-matrix.test.mjs"
assert_contains "${TMPDIR}/node-test-file.out" "NODE_TEST_SUMMARY:passed=1 failed=0"
assert_not_contains "${TMPDIR}/node-test-file.out" "WP_CODEBOX_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES=$'tools/fixture-matrix.test.mjs\ntests/Unit/ImportAgentAbilityTest.php' \
HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${TMPDIR}/stubs/wp-codebox.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/mixed-test-files.out"

assert_contains "${TMPDIR}/mixed-test-files.out" "NODE_TEST_BEGIN:tools/fixture-matrix.test.mjs"
assert_contains "${TMPDIR}/mixed-test-files.out" "NODE_TEST_OK:tools/fixture-matrix.test.mjs"
assert_contains "${TMPDIR}/mixed-test-files.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/mixed-test-files.out" "CHANGED=tools/fixture-matrix.test.mjs"
assert_contains "${TMPDIR}/mixed-test-files.out" "tests/Unit/ImportAgentAbilityTest.php"
assert_contains "${TMPDIR}/changed-smoke-files.out" "HOST_SMOKE_BEGIN:tests/queue-routing-smoke.php"
assert_contains "${TMPDIR}/changed-smoke-files.out" "HOST_SMOKE_SUMMARY:passed=1 failed=0"
assert_not_contains "${TMPDIR}/changed-smoke-files.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"
assert_not_contains "${TMPDIR}/changed-smoke-files.out" "WP_CODEBOX_STUB"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${TMPDIR}/stubs/host-smoke-wp.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/import-agent-ability-smoke.php > "${TMPDIR}/declared-standalone-smoke-file.out"

assert_contains "${TMPDIR}/declared-standalone-smoke-file.out" "PHP_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"
assert_contains "${TMPDIR}/declared-standalone-smoke-file.out" "PHP_SMOKE_OK:tests/import-agent-ability-smoke.php"
assert_not_contains "${TMPDIR}/declared-standalone-smoke-file.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${TMPDIR}/stubs/host-smoke-wp.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/queue-routing-smoke.php > "${TMPDIR}/declared-wordpress-smoke-file.out"

assert_contains "${TMPDIR}/declared-wordpress-smoke-file.out" "HOST_SMOKE_BEGIN:tests/queue-routing-smoke.php"
assert_not_contains "${TMPDIR}/declared-wordpress-smoke-file.out" "PHP_SMOKE_BEGIN:tests/queue-routing-smoke.php"

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
# The Node heap default moved upstream: ca924281 delegated the WordPress runners
# to the WP Codebox CLI, which sizes its own host heap
# (packages/cli/src/host-node-heap.ts). Nothing in this extension sets
# NODE_OPTIONS any more, so asserting it here pinned behaviour that had already
# been deliberately handed off.
assert_contains "${TMPDIR}/wp-codebox-file.out" "CORE_MODULE=${EXTENSION_PATH}/tests/fixtures/wp-codebox-core-recipe-builder.mjs"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "recipe-run"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "--recipe"
# The recipe is no longer built in-process through a core module: 0c181de9
# ("remove obsolete codebox recipe builders") and 64586bfc ("remove codebox
# compatibility discovery") handed recipe building to the WP Codebox CLI, so the
# workflow command is the CLI's own `wordpress.phpunit`. HOMEBOY_WP_CODEBOX_CORE_MODULE
# is still forwarded (asserted above) but no longer selects a builder.
assert_contains "${TMPDIR}/wp-codebox-args.txt" "wordpress.phpunit"
assert_contains "${TMPDIR}/wp-codebox-args.txt" "autoload-file=/wp-codebox-vendor/autoload.php"
assert_not_contains "${TMPDIR}/wp-codebox-args.txt" "6.9"
# The plugin's sandbox location still reaches the recipe, but as a declared
# dependency mount rather than an inputs.mounts entry: the CLI derives the
# component mount itself now, so the extension declares the sandbox path and
# lets the CLI place it.
assert_contains "${TMPDIR}/wp-codebox-args.txt" "dependency-mounts=/wordpress/wp-content/plugins/component"
assert_not_contains "${TMPDIR}/wp-codebox-file.out" "AMBIENT_WP_CODEBOX_CLI_SELECTED"
assert_not_contains "${TMPDIR}/wp-codebox-file.out" "${ambient_core_module}"
assert_not_contains "${TMPDIR}/wp-codebox-args.txt" "AMBIENT_WP_CODEBOX_CLI_SELECTED"
# d256319f made the shell runner delete .phpunit.result.cache after a run;
# ca924281 then replaced that runner with the Node adapter. Cleanup did not go
# missing, it changed owner: wordpress.json declares `phpunit-result-cache` as a
# disposable build_cache artifact, so retention prunes it rather than the runner
# deleting it inline. Asserting the runner still does it pins the wrong owner.

WP_CODEBOX_ARGS_FILE="${TMPDIR}/wp-codebox-configured-root-args.txt" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_SETTINGS_JSON='{"wp_codebox_phpunit_mounts":[{"source":"'"${component}"'","target":"/home/example/public_html","mode":"readwrite"}],"wp_codebox_phpunit_test_root":"/home/example/public_html/bin/tests/i18n-tools"}' \
HOMEBOY_WP_CODEBOX_BIN="${TMPDIR}/stubs/wp-codebox.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file /home/example/public_html/bin/tests/i18n-tools/ExtractTest.php > "${TMPDIR}/wp-codebox-configured-root-file.out"

assert_contains "${TMPDIR}/wp-codebox-configured-root-file.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/wp-codebox-configured-root-args.txt" "test-file=ExtractTest.php"
assert_contains "${TMPDIR}/wp-codebox-configured-root-args.txt" '"target": "/home/example/public_html"'

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_SETTINGS_JSON='{"wp_codebox_bin":"'"${TMPDIR}/stubs/wp-codebox.sh"'","wordpress_runtime_version":"latest"}' \
WP_CODEBOX_ARGS_FILE="${TMPDIR}/wp-codebox-settings-args.txt" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/Unit/ImportAgentAbilityTest.php > "${TMPDIR}/wp-codebox-settings.out"

assert_contains "${TMPDIR}/wp-codebox-settings.out" "WP_CODEBOX_STUB"
assert_contains "${TMPDIR}/wp-codebox-settings-args.txt" "latest"

# Suite membership (Extra-Chill/homeboy#14761): a changed PHPUnit file may only
# run in the suites whose own config declares it. The alpha suite declares the
# changed file via <file>; the beta suite scans tests/Beta via <directory> but
# excludes the other changed file; no suite declares that file at all, so it
# must surface as orphaned rather than being injected into either suite.
suite_component="${TMPDIR}/suite-component"
mkdir -p "${suite_component}/tests/Unit" "${suite_component}/tests/Beta"
cat > "${suite_component}/tests/Unit/ImportAgentAbilityTest.php" <<'PHP'
<?php
// Declared by the alpha suite only.
PHP
cat > "${suite_component}/tests/Beta/BetaOwnTest.php" <<'PHP'
<?php
// Discovered by the beta suite's directory scan.
PHP
cat > "${suite_component}/tests/Beta/ExcludedBetaTest.php" <<'PHP'
<?php
// Inside beta's scanned directory but excluded from the suite.
PHP
cat > "${suite_component}/phpunit-alpha.xml.dist" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<phpunit>
	<testsuites>
		<testsuite name="alpha">
			<file>tests/Unit/ImportAgentAbilityTest.php</file>
		</testsuite>
	</testsuites>
</phpunit>
XML
cat > "${suite_component}/phpunit-beta.xml.dist" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<phpunit>
	<testsuites>
		<testsuite name="beta">
			<directory>tests/Beta</directory>
			<exclude>tests/Beta/ExcludedBetaTest.php</exclude>
		</testsuite>
	</testsuites>
</phpunit>
XML

suite_membership_status=0
WP_CODEBOX_OPTIONS_LOG="${TMPDIR}/suite-membership-options.log" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$suite_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WP_CODEBOX_BIN="${TMPDIR}/stubs/wp-codebox.sh" \
HOMEBOY_SETTINGS_JSON='{"wp_codebox_phpunit_suites":[{"name":"alpha","config":"phpunit-alpha.xml.dist"},{"name":"beta","config":"phpunit-beta.xml.dist"}]}' \
HOMEBOY_CHANGED_TEST_FILES=$'tests/Unit/ImportAgentAbilityTest.php\ntests/Beta/ExcludedBetaTest.php' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/suite-membership.out" 2>&1 || suite_membership_status=$?

if [ "$suite_membership_status" -ne 0 ]; then
    echo "Expected the suite-membership changed scope to pass" >&2
    sed 's/^/  /' "${TMPDIR}/suite-membership.out" >&2
    exit 1
fi
assert_contains "${TMPDIR}/suite-membership.out" "PHPUNIT_SUITE_RESULT:name=alpha status=passed"
assert_contains "${TMPDIR}/suite-membership.out" "PHPUNIT_SUITE_RESULT:name=beta status=passed"
# (a) The declaring suite receives exactly its own changed file.
alpha_options="$(grep -F '"phpunitXml":"phpunit-alpha.xml.dist"' "${TMPDIR}/suite-membership-options.log" || true)"
[ -n "$alpha_options" ] || { echo "Expected alpha suite options in the options log" >&2; exit 1; }
case "$alpha_options" in
    *'"changedTestFiles":["/wordpress/wp-content/plugins/component/tests/Unit/ImportAgentAbilityTest.php"]'*) ;;
    *) echo "Expected the alpha suite to receive only its declared changed file, got: $alpha_options" >&2; exit 1 ;;
esac
# (b) The suite the changed file does not belong to receives no scope, so its
# own declared tests still run instead of an empty intersection.
beta_options="$(grep -F '"phpunitXml":"phpunit-beta.xml.dist"' "${TMPDIR}/suite-membership-options.log" || true)"
[ -n "$beta_options" ] || { echo "Expected beta suite options in the options log" >&2; exit 1; }
case "$beta_options" in
    *changedTestFiles*) echo "Expected the beta suite to keep its full declared set, got: $beta_options" >&2; exit 1 ;;
esac
# (c) The excluded file is declared by nobody here: reported as orphaned,
# never run, and never injected into an unrelated suite.
assert_contains "${TMPDIR}/suite-membership.out" "PHPUNIT_ORPHANED_TEST_FILE:tests/Beta/ExcludedBetaTest.php"
assert_not_contains "${TMPDIR}/suite-membership.out" "PHPUNIT_ORPHANED_TEST_FILE:tests/Unit/ImportAgentAbilityTest.php"

# The same membership rule when the only changed file is one a suite excludes:
# both suites keep their full declared sets and the file is orphaned again.
suite_excluded_status=0
WP_CODEBOX_OPTIONS_LOG="${TMPDIR}/suite-excluded-options.log" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$suite_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WP_CODEBOX_BIN="${TMPDIR}/stubs/wp-codebox.sh" \
HOMEBOY_SETTINGS_JSON='{"wp_codebox_phpunit_suites":[{"name":"alpha","config":"phpunit-alpha.xml.dist"},{"name":"beta","config":"phpunit-beta.xml.dist"}]}' \
HOMEBOY_CHANGED_TEST_FILES='tests/Beta/ExcludedBetaTest.php' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/suite-excluded.out" 2>&1 || suite_excluded_status=$?

if [ "$suite_excluded_status" -ne 0 ]; then
    echo "Expected the excluded-file changed scope to pass" >&2
    sed 's/^/  /' "${TMPDIR}/suite-excluded.out" >&2
    exit 1
fi
alpha_excluded_options="$(grep -F '"phpunitXml":"phpunit-alpha.xml.dist"' "${TMPDIR}/suite-excluded-options.log" || true)"
case "$alpha_excluded_options" in
    *changedTestFiles*) echo "Expected alpha to keep its full declared set when only an unrelated file changed, got: $alpha_excluded_options" >&2; exit 1 ;;
esac
assert_contains "${TMPDIR}/suite-excluded.out" "PHPUNIT_ORPHANED_TEST_FILE:tests/Beta/ExcludedBetaTest.php"

# The registration-drift preflight classification (d0ffb219, #745) was removed
# by ca924281 when the shell runner was replaced by the Node adapter, and it was
# never reimplemented there. Nothing emits "HARNESS PREFLIGHT FAILURE" today, so
# this scenario asserted a diagnostic that no longer exists. Whether that
# classification should come back is a product question, tracked separately —
# it is not something this smoke can assert into existence.

# The Composer- and npm-test-script fallbacks these scenarios exercised no
# longer exist: ca924281 removed them, and homeboy_wordpress_handle_no_phpunit_tests
# now applies the phpunit_no_tests policy instead ("Skipping PHPUnit: no canonical
# test files were discovered."). That current behaviour is already covered by the
# registered scripts/test/full-suite-no-phpunit-smoke.sh, so these assertions were
# both fossilised and superseded.

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
