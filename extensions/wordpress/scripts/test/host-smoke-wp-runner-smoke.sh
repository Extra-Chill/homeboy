#!/usr/bin/env bash
set -euo pipefail

# Smoke for the real-WordPress smoke backend (test-runner-host-smoke-wp.sh).
# Uses a fake wp-codebox bin that captures the generated recipe so we can assert
# the backend mounts the plugin and runs each smoke via wordpress.run-php with
# WordPress booted, plus the host-smoke marker/exit-code contract and routing.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_PATH}/../.." && pwd)/homeboy}"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${HOMEBOY_CORE_DIR}/crates/homeboy-extension/src/runtime/resolve-context.sh}"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

if [ ! -f "$RESOLVE_CONTEXT_HELPER" ]; then
    echo "Missing resolve-context helper: ${RESOLVE_CONTEXT_HELPER}" >&2
    exit 1
fi
export HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER"

assert_contains() {
    local file="$1" expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1" unexpected="$2"
    if grep -Fq "$unexpected" "$file"; then
        echo "Expected $file not to contain: $unexpected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

# Fake wp-codebox that records the recipe it was handed and emits a successful
# recipe-run result whose last execution echoes the smoke's "OK" line.
FAKE_CODEBOX="${TMPDIR}/fake-wp-codebox.cjs"
CAPTURE_DIR="${TMPDIR}/captures"
mkdir -p "$CAPTURE_DIR"
cat > "$FAKE_CODEBOX" <<'JS'
#!/usr/bin/env node
'use strict';
const fs = require('fs');
const args = process.argv.slice(2);
if (args[0] === 'commands') {
  process.stdout.write('commands\n');
  process.exit(0);
}
if (args.includes('--version')) {
  process.stdout.write('0.21.0');
  process.exit(0);
}
if (args[0] === 'runtime' && args[1] === 'descriptor' && args.includes('--json')) {
  process.stdout.write(JSON.stringify({
    schema: 'wp-codebox/runtime-descriptor/v1',
    readiness: { status: 'available', browserRuntime: { status: 'ready' } },
    contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } },
  }));
  process.exit(0);
}
const recipeIdx = args.indexOf('--recipe');
const recipe = JSON.parse(fs.readFileSync(args[recipeIdx + 1], 'utf8'));
const captureDir = process.env.FAKE_CODEBOX_CAPTURE_DIR;
fs.writeFileSync(`${captureDir}/recipe-${Date.now()}-${Math.random().toString(36).slice(2)}.json`, JSON.stringify(recipe, null, 2));
fs.writeFileSync(`${captureDir}/argv-${Date.now()}-${Math.random().toString(36).slice(2)}.json`, JSON.stringify(process.argv.slice(1), null, 2));
const codeFileArg = recipe.workflow.steps[0].args.find((arg) => arg.startsWith('code-file='));
fs.copyFileSync(codeFileArg.slice('code-file='.length), `${captureDir}/wrapper.php`);
// The run-php step's code-file is a wrapper that requires the smoke; we emit a
// successful run with an OK stdout to mirror a passing smoke.
process.stdout.write(JSON.stringify({
  success: true,
  executions: [{ command: 'wordpress.run-php', exitCode: 0, stdout: 'OK fake smoke passed\n', stderr: '' }],
}));
JS
chmod +x "$FAKE_CODEBOX"

make_component() {
    local target="$1"
    mkdir -p "$target/tests"
    cat > "$target/tests/alpha-smoke.php" <<'PHP'
<?php
echo "alpha ok\n";
PHP
}

component="${TMPDIR}/component"
make_component "$component"

# Managed cache state is validated before a recipe is created. An update lock
# and a missing identity both fail closed instead of selecting another runtime.
MANAGED_CACHE="${TMPDIR}/managed-cache"
MANAGED_CLI="${MANAGED_CACHE}/source/packages/cli/dist/index.js"
mkdir -p "$(dirname "$MANAGED_CLI")"
cp "$FAKE_CODEBOX" "$MANAGED_CLI"
mkdir -p "${MANAGED_CACHE}/source.update-lock"
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_WP_CODEBOX_INSTALL_DIR="$MANAGED_CACHE" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="tests/alpha-smoke.php" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/updating.out" 2>&1
updating_exit=$?
set -e
if [ "$updating_exit" -eq 0 ] || ! grep -Fq 'managed WP Codebox cache is updating' "${TMPDIR}/updating.out"; then
    echo "Expected the managed update lock to reject host-smoke execution" >&2
    cat "${TMPDIR}/updating.out" >&2
    exit 1
fi
rmdir "${MANAGED_CACHE}/source.update-lock"
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_WP_CODEBOX_INSTALL_DIR="$MANAGED_CACHE" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="tests/alpha-smoke.php" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/tampered.out" 2>&1
tampered_exit=$?
set -e
if [ "$tampered_exit" -eq 0 ] || ! grep -Fq 'wp_codebox_managed_source_identity_invalid' "${TMPDIR}/tampered.out"; then
    echo "Expected a managed runtime without a verified identity to reject host-smoke execution" >&2
    exit 1
fi

# --- Default backend run: no implicit discovery of ad hoc smoke files.
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX" \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/default.out"

assert_contains "${TMPDIR}/default.out" "Backend: host-smoke-wp"
assert_contains "${TMPDIR}/default.out" "Skipping real-WordPress smoke tests: no smoke files requested."
assert_not_contains "${TMPDIR}/default.out" "HOST_SMOKE_BEGIN:tests/alpha-smoke.php"

# --- Direct backend run: builds a wordpress.run-php recipe mounting the plugin.
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="tests/alpha-smoke.php" \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/direct.out"

assert_contains "${TMPDIR}/direct.out" "Backend: host-smoke-wp"
assert_contains "${TMPDIR}/direct.out" "HOST_SMOKE_BEGIN:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/direct.out" "HOST_SMOKE_PROGRESS:tests/alpha-smoke.php:phase=recipe-created"
assert_contains "${TMPDIR}/direct.out" "HOST_SMOKE_OK:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/direct.out" "HOST_SMOKE_SUMMARY:passed=1 failed=0"

# The captured recipe must mount the plugin into wp-content/plugins and run the
# smoke via wordpress.run-php (real WordPress booted).
captured_recipe="$(ls "${CAPTURE_DIR}"/recipe-*.json | head -1)"
captured_argv="$(ls "${CAPTURE_DIR}"/argv-*.json | head -1)"
assert_contains "$captured_recipe" "wordpress.run-php"
assert_contains "$captured_recipe" "/wordpress/wp-content/plugins/component"
assert_contains "$captured_recipe" "workspace-recipe/v1"
assert_contains "${CAPTURE_DIR}/wrapper.php" "\$homeboy_smoke_stderr = defined(\"STDERR\") ? STDERR : fopen(\"php://stderr\", \"w\");"
assert_contains "${CAPTURE_DIR}/wrapper.php" "fwrite(\$homeboy_smoke_stderr"
assert_not_contains "${CAPTURE_DIR}/wrapper.php" "fwrite(STDERR"
assert_contains "$captured_argv" "$FAKE_CODEBOX"
assert_contains "$captured_argv" "recipe-run"

# --- Child environment contract: runtime and dependency roots are sandbox
# paths, even when their host checkout paths need shell quoting/translation.
component_with_spaces="${TMPDIR}/component with spaces"
dependency_with_spaces="${TMPDIR}/dependency roots/dependency-plugin"
make_component "$component_with_spaces"
mkdir -p "$dependency_with_spaces"
cat > "${dependency_with_spaces}/dependency-plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Dependency Plugin
 */
PHP

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component_with_spaces" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="tests/alpha-smoke.php" \
HOMEBOY_SETTINGS_JSON="$(jq -nc --arg path "$dependency_with_spaces" '{validation_dependencies: [{path: $path, plugin_slug: "dependency-plugin"}]}')" \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/environment.out"

captured_environment_recipe="$(ls -t "${CAPTURE_DIR}"/recipe-*.json | head -1)"
assert_contains "$captured_environment_recipe" "${component_with_spaces}"
assert_contains "$captured_environment_recipe" "${dependency_with_spaces}"
assert_contains "$captured_environment_recipe" "/wordpress/wp-content/plugins/dependency-plugin"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'WP_PATH' => '/wordpress'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'HOMEBOY_WORDPRESS_DEPENDENCY_ROOTS_JSON' => '{\"dependency-plugin\":\"/wordpress/wp-content/plugins/dependency-plugin\"}'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'HOMEBOY_WORDPRESS_DEPENDENCY_LOWER_D_LOWER_E_LOWER_P_LOWER_E_LOWER_N_LOWER_D_LOWER_E_LOWER_N_LOWER_C_LOWER_Y_HYPHEN_LOWER_P_LOWER_L_LOWER_U_LOWER_G_LOWER_I_LOWER_N_ROOT' => '/wordpress/wp-content/plugins/dependency-plugin'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'DEPENDENCY_PLUGIN_PATH' => '/wordpress/wp-content/plugins/dependency-plugin'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "putenv(\$name . \"=\" . \$value)"

# --- Normalized legacy aliases cannot overwrite one another or WP_PATH. The
# canonical JSON map and injective namespaced variables retain every root.
collision_component="${TMPDIR}/collision-component"
mkdir -p "${collision_component}/tests"
cp "${component}/tests/alpha-smoke.php" "${collision_component}/tests/alpha-smoke.php"
collision_dependencies=()
for slug in foo-bar foo.bar foo_bar foo FOO wp data-machine; do
    dependency="${TMPDIR}/collision-dependencies/${slug}"
    mkdir -p "$dependency"
    cat > "${dependency}/${slug}.php" <<PHP
<?php
/**
 * Plugin Name: ${slug}
 */
PHP
    collision_dependencies+=("$dependency")
done

collision_settings="$(jq -nc \
    --arg fooHyphen "${collision_dependencies[0]}" \
    --arg fooDot "${collision_dependencies[1]}" \
    --arg fooUnderscore "${collision_dependencies[2]}" \
    --arg fooLower "${collision_dependencies[3]}" \
    --arg fooUpper "${collision_dependencies[4]}" \
    --arg wp "${collision_dependencies[5]}" \
    --arg dataMachine "${collision_dependencies[6]}" \
    '{validation_dependencies: [$fooHyphen, $fooDot, $fooUnderscore, $fooLower, $fooUpper, $wp, $dataMachine]}')"
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="collision-component" \
HOMEBOY_COMPONENT_PATH="$collision_component" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="tests/alpha-smoke.php" \
HOMEBOY_SETTINGS_JSON="$collision_settings" \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/collision.out"

assert_contains "${CAPTURE_DIR}/wrapper.php" "'WP_PATH' => '/wordpress'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'HOMEBOY_WORDPRESS_DEPENDENCY_LOWER_F_LOWER_O_LOWER_O_HYPHEN_LOWER_B_LOWER_A_LOWER_R_ROOT' => '/wordpress/wp-content/plugins/foo-bar'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'HOMEBOY_WORDPRESS_DEPENDENCY_LOWER_F_LOWER_O_LOWER_O_DOT_LOWER_B_LOWER_A_LOWER_R_ROOT' => '/wordpress/wp-content/plugins/foo.bar'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'HOMEBOY_WORDPRESS_DEPENDENCY_LOWER_F_LOWER_O_LOWER_O_UNDERSCORE_LOWER_B_LOWER_A_LOWER_R_ROOT' => '/wordpress/wp-content/plugins/foo_bar'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'HOMEBOY_WORDPRESS_DEPENDENCY_LOWER_F_LOWER_O_LOWER_O_ROOT' => '/wordpress/wp-content/plugins/foo'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'HOMEBOY_WORDPRESS_DEPENDENCY_UPPER_F_UPPER_O_UPPER_O_ROOT' => '/wordpress/wp-content/plugins/FOO'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'HOMEBOY_WORDPRESS_DEPENDENCY_LOWER_W_LOWER_P_ROOT' => '/wordpress/wp-content/plugins/wp'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'HOMEBOY_WORDPRESS_DEPENDENCY_LOWER_D_LOWER_A_LOWER_T_LOWER_A_HYPHEN_LOWER_M_LOWER_A_LOWER_C_LOWER_H_LOWER_I_LOWER_N_LOWER_E_ROOT' => '/wordpress/wp-content/plugins/data-machine'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "'DATA_MACHINE_PATH' => '/wordpress/wp-content/plugins/data-machine'"
assert_contains "${CAPTURE_DIR}/wrapper.php" "\"foo-bar\":\"/wordpress/wp-content/plugins/foo-bar\""
assert_contains "${CAPTURE_DIR}/wrapper.php" "\"foo.bar\":\"/wordpress/wp-content/plugins/foo.bar\""
assert_contains "${CAPTURE_DIR}/wrapper.php" "\"foo_bar\":\"/wordpress/wp-content/plugins/foo_bar\""
assert_contains "${CAPTURE_DIR}/wrapper.php" "\"foo\":\"/wordpress/wp-content/plugins/foo\""
assert_contains "${CAPTURE_DIR}/wrapper.php" "\"FOO\":\"/wordpress/wp-content/plugins/FOO\""
assert_not_contains "${CAPTURE_DIR}/wrapper.php" "'FOO_BAR_PATH'"
assert_not_contains "${CAPTURE_DIR}/wrapper.php" "'FOO_PATH'"
assert_not_contains "${CAPTURE_DIR}/wrapper.php" "'WP_PATH' => '/wordpress/wp-content/plugins/wp'"

# --- Failure propagation: a fake codebox returning success=false fails the run.
FAKE_CODEBOX_FAIL="${TMPDIR}/fake-wp-codebox-fail.cjs"
cat > "$FAKE_CODEBOX_FAIL" <<'JS'
#!/usr/bin/env node
'use strict';
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args[0] === 'runtime' && args[1] === 'descriptor') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }
process.stdout.write(JSON.stringify({
  success: false,
  executions: [{ command: 'wordpress.run-php', exitCode: 1, stdout: '', stderr: 'smoke threw\n' }],
}));
JS

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX_FAIL" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="tests/alpha-smoke.php" \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/direct-fail.out" 2>&1
fail_exit=$?
set -e
if [ "$fail_exit" -eq 0 ]; then
    echo "Expected real-WP smoke runner to fail when the recipe run fails" >&2
    sed 's/^/  /' "${TMPDIR}/direct-fail.out" >&2
    exit 1
fi
assert_contains "${TMPDIR}/direct-fail.out" "HOST_SMOKE_FAIL:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/direct-fail.out" "HOST_SMOKE_OUTPUT_BEGIN:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/direct-fail.out" "smoke threw"
assert_contains "${TMPDIR}/direct-fail.out" "HOST_SMOKE_OUTPUT_END:tests/alpha-smoke.php:artifacts="

# --- Aggregation (#4682): when several smokes run and one fails in the middle,
# the runner must keep going and report EVERY smoke's pass/fail in ONE run
# rather than bailing at the first failure. A fake codebox fails only beta; the
# run must still execute gamma, surface both pass and fail markers, emit an
# aggregated summary (failed != 0), list the failing smoke, and exit non-zero.
multi_component="${TMPDIR}/multi-component"
mkdir -p "${multi_component}/tests"
printf '<?php echo "alpha ok\\n";\n' > "${multi_component}/tests/alpha-smoke.php"
printf '<?php echo "beta ok\\n";\n'  > "${multi_component}/tests/beta-smoke.php"
printf '<?php echo "gamma ok\\n";\n' > "${multi_component}/tests/gamma-smoke.php"

FAKE_CODEBOX_BETA_FAIL="${TMPDIR}/fake-wp-codebox-beta-fail.cjs"
cat > "$FAKE_CODEBOX_BETA_FAIL" <<'JS'
#!/usr/bin/env node
'use strict';
const fs = require('fs');
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args[0] === 'runtime' && args[1] === 'descriptor') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }
const recipe = JSON.parse(fs.readFileSync(args[args.indexOf('--recipe') + 1], 'utf8'));
const codeFileArg = recipe.workflow.steps[0].args.find((arg) => arg.startsWith('code-file='));
const wrapper = fs.readFileSync(codeFileArg.slice('code-file='.length), 'utf8');
const fail = wrapper.includes('beta-smoke.php');
process.stdout.write(JSON.stringify({
  success: !fail,
  executions: [{ command: 'wordpress.run-php', exitCode: fail ? 1 : 0, stdout: fail ? '' : 'OK\n', stderr: fail ? 'beta blew up\n' : '' }],
}));
JS

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="multi-component" \
HOMEBOY_COMPONENT_PATH="$multi_component" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX_BETA_FAIL" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILES=$'tests/alpha-smoke.php\ntests/beta-smoke.php\ntests/gamma-smoke.php' \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/aggregate.out" 2>&1
aggregate_exit=$?
set -e
if [ "$aggregate_exit" -eq 0 ]; then
    echo "Expected aggregated host-smoke run to exit non-zero when a smoke fails" >&2
    sed 's/^/  /' "${TMPDIR}/aggregate.out" >&2
    exit 1
fi
assert_contains "${TMPDIR}/aggregate.out" "HOST_SMOKE_OK:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/aggregate.out" "HOST_SMOKE_FAIL:tests/beta-smoke.php:exit=1"
# gamma running proves the runner did NOT bail at beta's failure.
assert_contains "${TMPDIR}/aggregate.out" "HOST_SMOKE_OK:tests/gamma-smoke.php"
assert_contains "${TMPDIR}/aggregate.out" "HOST_SMOKE_SUMMARY:passed=2 failed=1"
assert_contains "${TMPDIR}/aggregate.out" "tests/beta-smoke.php (exit 1)"

# --- Timeout diagnostics: a hung recipe-run is bounded per file and reports the
# phase/artifact directory before returning the conventional timeout status.
FAKE_CODEBOX_HANG="${TMPDIR}/fake-wp-codebox-hang.cjs"
cat > "$FAKE_CODEBOX_HANG" <<'JS'
#!/usr/bin/env node
'use strict';
const fs = require('fs');
const { spawn } = require('child_process');
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args[0] === 'runtime' && args[1] === 'descriptor') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }
const descendant = spawn(process.execPath, ['-e', 'process.on("SIGTERM", () => {}); setInterval(() => {}, 1e9);'], { stdio: 'inherit' });
fs.writeFileSync(process.env.FIXTURE_PIDS_PATH, JSON.stringify({ descendant: descendant.pid }));
process.on('SIGTERM', () => process.exit(0));
setTimeout(() => {}, 10000);
JS

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX_HANG" \
HOMEBOY_WORDPRESS_HOST_SMOKE_TIMEOUT_SECONDS=1 \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="tests/alpha-smoke.php" \
FIXTURE_PIDS_PATH="${TMPDIR}/host-timeout-pids.json" \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/direct-timeout.out" 2>&1
timeout_exit=$?
set -e
if [ "$timeout_exit" -ne 124 ]; then
    echo "Expected real-WP smoke runner to exit 124 on timeout, got ${timeout_exit}" >&2
    sed 's/^/  /' "${TMPDIR}/direct-timeout.out" >&2
    exit 1
fi
assert_contains "${TMPDIR}/direct-timeout.out" "HOST_SMOKE_TIMEOUT:tests/alpha-smoke.php:phase=wp-codebox-recipe-run"
assert_contains "${TMPDIR}/direct-timeout.out" "HOST_SMOKE_FAIL:tests/alpha-smoke.php:exit=124"
assert_contains "${TMPDIR}/direct-timeout.out" "artifacts="
assert_contains "${TMPDIR}/direct-timeout.out" '"schema":"homeboy/wp-codebox-timeout-diagnostics/v1"'
assert_contains "${TMPDIR}/direct-timeout.out" '"phase":"wp-codebox-recipe-run"'
assert_contains "${TMPDIR}/direct-timeout.out" '"selected":{"count":1,"items":["tests/alpha-smoke.php"]}'
assert_contains "${TMPDIR}/direct-timeout.out" '"termination":{"result":"timeout"'
host_descendant_pid="$(node -e 'process.stdout.write(String(JSON.parse(require("node:fs").readFileSync(process.argv[1], "utf8")).descendant))' "${TMPDIR}/host-timeout-pids.json")"
for _ in $(seq 1 200); do
    if ! kill -0 "$host_descendant_pid" 2>/dev/null; then
        break
    fi
    sleep 0.025
done
if kill -0 "$host_descendant_pid" 2>/dev/null; then
    echo "Expected timeout descendant ${host_descendant_pid} to be reaped" >&2
    exit 1
fi

# --- Routing: the dispatcher routes *-smoke.php to this real-WP smoke backend
# purely by file type (no test_backend toggle). A --file smoke run goes straight
# to the smoke backend; the PHPUnit backend is not involved.
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX" \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/alpha-smoke.php > "${TMPDIR}/router.out"
assert_contains "${TMPDIR}/router.out" "Backend: host-smoke-wp"
assert_contains "${TMPDIR}/router.out" "HOST_SMOKE_OK:tests/alpha-smoke.php"

# --- Operator affordance: maintainers can rerun one real-WP host smoke on
# demand without relying on changed-file routing or running the full suite.
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX" \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --host-smoke-file tests/alpha-smoke.php > "${TMPDIR}/operator.out"
assert_contains "${TMPDIR}/operator.out" "Backend: host-smoke-wp"
assert_contains "${TMPDIR}/operator.out" "HOST_SMOKE_BEGIN:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/operator.out" "HOST_SMOKE_OK:tests/alpha-smoke.php"

help_output="$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" HOMEBOY_COMPONENT_ID="component" HOMEBOY_COMPONENT_PATH="$component" HOMEBOY_COMPONENT_SHAPE="plugin" bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --help)"
if [[ "$help_output" != *"--host-smoke-file <path>"* ]] || [[ "$help_output" != *"HOST_SMOKE_BEGIN"* ]]; then
    echo "Expected runner help to document --host-smoke-file and HOST_SMOKE markers" >&2
    printf '%s\n' "$help_output" >&2
    exit 1
fi

echo "Real-WordPress smoke runner smoke passed"
