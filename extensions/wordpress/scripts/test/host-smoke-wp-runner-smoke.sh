#!/usr/bin/env bash
set -euo pipefail

# Smoke for the real-WordPress smoke backend (test-runner-host-smoke-wp.sh).
# Uses a fake wp-codebox bin that captures the generated recipe so we can assert
# the backend mounts the plugin and runs each smoke via wordpress.run-php with
# WordPress booted, plus the host-smoke marker/exit-code contract and routing.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

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
const recipeIdx = args.indexOf('--recipe');
const recipe = JSON.parse(fs.readFileSync(args[recipeIdx + 1], 'utf8'));
const captureDir = process.env.FAKE_CODEBOX_CAPTURE_DIR;
fs.writeFileSync(`${captureDir}/recipe-${Date.now()}-${Math.random().toString(36).slice(2)}.json`, JSON.stringify(recipe, null, 2));
// The run-php step's code-file is a wrapper that requires the smoke; we emit a
// successful run with an OK stdout to mirror a passing smoke.
process.stdout.write(JSON.stringify({
  success: true,
  executions: [{ command: 'wordpress.run-php', exitCode: 0, stdout: 'OK fake smoke passed\n', stderr: '' }],
}));
JS

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

# --- Direct backend run: builds a wordpress.run-php recipe mounting the plugin.
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_WP_CODEBOX_BIN="$FAKE_CODEBOX" \
FAKE_CODEBOX_CAPTURE_DIR="$CAPTURE_DIR" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke-wp.sh" > "${TMPDIR}/direct.out"

assert_contains "${TMPDIR}/direct.out" "Backend: host-smoke-wp"
assert_contains "${TMPDIR}/direct.out" "HOST_SMOKE_BEGIN:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/direct.out" "HOST_SMOKE_OK:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/direct.out" "HOST_SMOKE_SUMMARY:passed=1 failed=0"

# The captured recipe must mount the plugin into wp-content/plugins and run the
# smoke via wordpress.run-php (real WordPress booted).
captured_recipe="$(ls "${CAPTURE_DIR}"/recipe-*.json | head -1)"
assert_contains "$captured_recipe" "wordpress.run-php"
assert_contains "$captured_recipe" "/wordpress/wp-content/plugins/component"
assert_contains "$captured_recipe" "workspace-recipe/v1"

# --- Failure propagation: a fake codebox returning success=false fails the run.
FAKE_CODEBOX_FAIL="${TMPDIR}/fake-wp-codebox-fail.cjs"
cat > "$FAKE_CODEBOX_FAIL" <<'JS'
#!/usr/bin/env node
'use strict';
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

echo "Real-WordPress smoke runner smoke passed"
