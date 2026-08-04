#!/usr/bin/env bash
set -euo pipefail

# Regression: a `*.test.js` suffix must not replace the framework the component
# declares. A WordPress package whose contract is `wp-scripts test-unit-js`
# runs Jest, whose describe/it/expect globals do not exist under `node --test`.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/.." && pwd)"
RUNNER="${EXTENSION_PATH}/scripts/test/test-runner.sh"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

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

runner_prelude="${WORK_DIR}/runner-prelude.sh"
cat > "$runner_prelude" <<'SH'
homeboy_runner_init() {
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
    PLUGIN_PATH="$COMPONENT_PATH"
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:?HOMEBOY_EXTENSION_PATH is required}"
}
SH

stubs="${WORK_DIR}/stubs"
mkdir -p "$stubs"
# Stand in for the package manager so the smoke asserts the delegated contract
# without installing a real Jest toolchain.
cat > "${stubs}/npm" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NPM_INVOKED:%s\n' "$*"
if [ "${1:-}" = "run" ] && [ "${2:-}" = "test:unit" ]; then
    echo "PASS ${*:4}"
    echo "Test Suites: 1 passed, 1 total"
    exit 0
fi
echo "Unexpected npm command: $*" >&2
exit 2
SH
chmod +x "${stubs}/npm"

# --- Jest-shaped component: package contract owns the runner ---------------
jest_component="${WORK_DIR}/jest-component"
mkdir -p "${jest_component}/src/blocks/studio/tabs" "${jest_component}/node_modules"
cat > "${jest_component}/package.json" <<'JSON'
{
  "name": "studio",
  "scripts": {
    "build": "wp-scripts build",
    "test:unit": "wp-scripts test-unit-js"
  }
}
JSON
cat > "${jest_component}/src/blocks/studio/tabs/usability-contracts.test.js" <<'JS'
describe( 'usability contracts', () => {
	it( 'exposes Jest globals', () => {
		expect( 2 + 2 ).toBe( 4 );
	} );
} );
JS

PATH="${stubs}:${PATH}" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="studio" \
HOMEBOY_COMPONENT_PATH="$jest_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES="src/blocks/studio/tabs/usability-contracts.test.js" \
    bash "$RUNNER" > "${WORK_DIR}/jest.out" 2>&1

assert_contains "${WORK_DIR}/jest.out" "Backend: package-script"
assert_contains "${WORK_DIR}/jest.out" "Contract: package.json scripts.test:unit"
assert_contains "${WORK_DIR}/jest.out" "JS_TEST_BEGIN:src/blocks/studio/tabs/usability-contracts.test.js"
assert_contains "${WORK_DIR}/jest.out" "NPM_INVOKED:run test:unit -- src/blocks/studio/tabs/usability-contracts.test.js"
assert_contains "${WORK_DIR}/jest.out" "JS_TEST_SUMMARY:backend=package-script script=test:unit files=1 status=ok"
assert_not_contains "${WORK_DIR}/jest.out" "Backend: node-test"
assert_not_contains "${WORK_DIR}/jest.out" "describe is not defined"

# The same routing must hold for an explicitly selected file.
PATH="${stubs}:${PATH}" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="studio" \
HOMEBOY_COMPONENT_PATH="$jest_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
    bash "$RUNNER" --file src/blocks/studio/tabs/usability-contracts.test.js > "${WORK_DIR}/jest-file.out" 2>&1

assert_contains "${WORK_DIR}/jest-file.out" "Backend: package-script"
assert_not_contains "${WORK_DIR}/jest-file.out" "Backend: node-test"

# --- Native node:test component: built-in runner stays selected ------------
native_component="${WORK_DIR}/native-component"
mkdir -p "${native_component}/tools"
cat > "${native_component}/tools/fixture-matrix.test.mjs" <<'JS'
import test from 'node:test';
import assert from 'node:assert/strict';

test( 'composed Node test runs', () => assert.equal( 2 + 2, 4 ) );
JS

HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="native" \
HOMEBOY_COMPONENT_PATH="$native_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES="tools/fixture-matrix.test.mjs" \
    bash "$RUNNER" > "${WORK_DIR}/native.out" 2>&1

assert_contains "${WORK_DIR}/native.out" "Backend: node-test"
assert_contains "${WORK_DIR}/native.out" "Contract: native node:test import"
assert_contains "${WORK_DIR}/native.out" "NODE_TEST_OK:tools/fixture-matrix.test.mjs"
assert_contains "${WORK_DIR}/native.out" "NODE_TEST_SUMMARY:passed=1 failed=0"

# --- Ambiguous file: no declared script and no native import ---------------
ambiguous_component="${WORK_DIR}/ambiguous-component"
mkdir -p "${ambiguous_component}/src"
cat > "${ambiguous_component}/src/widget.test.js" <<'JS'
describe( 'widget', () => {
	it( 'has no resolvable runner', () => {
		expect( true ).toBe( true );
	} );
} );
JS

set +e
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="ambiguous" \
HOMEBOY_COMPONENT_PATH="$ambiguous_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES="src/widget.test.js" \
    bash "$RUNNER" > "${WORK_DIR}/ambiguous.out" 2>&1
status=$?
set -e

if [ "$status" -ne 2 ]; then
    echo "Expected ambiguous JavaScript test selection to exit 2, got $status" >&2
    sed 's/^/  /' "${WORK_DIR}/ambiguous.out" >&2
    exit 1
fi
assert_contains "${WORK_DIR}/ambiguous.out" "cannot select a JavaScript test framework for: src/widget.test.js"
assert_not_contains "${WORK_DIR}/ambiguous.out" "NODE_TEST_BEGIN:src/widget.test.js"

# --- Declared setting overrides the package script candidates --------------
setting_component="${WORK_DIR}/setting-component"
mkdir -p "${setting_component}/src" "${setting_component}/node_modules"
cat > "${setting_component}/package.json" <<'JSON'
{
  "name": "setting",
  "scripts": {
    "test:unit": "wp-scripts test-unit-js"
  }
}
JSON
cat > "${setting_component}/src/widget.test.js" <<'JS'
describe( 'widget', () => {
	it( 'runs under the declared runner', () => {
		expect( true ).toBe( true );
	} );
} );
JS

set +e
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="setting" \
HOMEBOY_COMPONENT_PATH="$setting_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_SETTINGS_JSON='{"wordpress_js_test_script":"test:missing"}' \
HOMEBOY_CHANGED_TEST_FILES="src/widget.test.js" \
    bash "$RUNNER" > "${WORK_DIR}/missing-script.out" 2>&1
status=$?
set -e

if [ "$status" -ne 2 ]; then
    echo "Expected a missing declared script to exit 2, got $status" >&2
    sed 's/^/  /' "${WORK_DIR}/missing-script.out" >&2
    exit 1
fi
assert_contains "${WORK_DIR}/missing-script.out" "declared JavaScript test script 'test:missing' is not defined"

echo "WordPress JavaScript test framework routing smoke passed"
