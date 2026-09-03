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
printf 'NPM_CWD:%s\n' "$PWD"
printf 'NPM_INVOKED:%s\n' "$*"
if [ "${1:-}" = "run" ] && { [ "${2:-}" = "test:unit" ] || [ "${2:-}" = "test:js" ] || [ "${2:-}" = "test" ]; }; then
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

# --- Nested packages: nearest package owns script, cwd, and relative paths --
nested_component="${WORK_DIR}/nested-component"
calendar_package="${nested_component}/inc/Blocks/Calendar"
player_package="${nested_component}/blocks/Player"
mkdir -p "${nested_component}/src" "${nested_component}/node_modules" \
    "${calendar_package}/src" "${calendar_package}/node_modules" \
    "${player_package}/src" "${player_package}/node_modules"
cat > "${nested_component}/package.json" <<'JSON'
{
  "name": "nested-root",
  "scripts": {
    "test": "root-default-tests",
    "test:unit": "root-tests"
  }
}
JSON
cat > "${player_package}/package.json" <<'JSON'
{
  "name": "player",
  "scripts": {
    "test:js": "player-tests"
  }
}
JSON
cat > "${calendar_package}/package.json" <<'JSON'
{
  "name": "calendar",
  "scripts": {
    "test": "wp-scripts test-unit-js"
  }
}
JSON
cat > "${nested_component}/src/root.test.js" <<'JS'
describe( 'root package', () => {
	it( 'uses the root runner', () => expect( true ).toBe( true ) );
} );
JS
cat > "${calendar_package}/src/frontend.test.ts" <<'JS'
describe( 'calendar package', () => {
	it( 'uses the nested runner', () => expect( true ).toBe( true ) );
} );
JS
cat > "${player_package}/src/player.test.js" <<'JS'
describe( 'player package', () => {
	it( 'uses its own nested runner', () => expect( true ).toBe( true ) );
} );
JS

PATH="${stubs}:${PATH}" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="nested" \
HOMEBOY_COMPONENT_PATH="$nested_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES=$'src/root.test.js\ninc/Blocks/Calendar/src/frontend.test.ts\nblocks/Player/src/player.test.js' \
    bash "$RUNNER" > "${WORK_DIR}/nested.out" 2>&1

assert_contains "${WORK_DIR}/nested.out" "Package: ${nested_component}"
assert_contains "${WORK_DIR}/nested.out" "Contract: package.json scripts.test:unit"
assert_contains "${WORK_DIR}/nested.out" "NPM_CWD:${nested_component}"
assert_contains "${WORK_DIR}/nested.out" "NPM_INVOKED:run test:unit -- src/root.test.js"
assert_contains "${WORK_DIR}/nested.out" "Package: ${calendar_package}"
assert_contains "${WORK_DIR}/nested.out" "Contract: inc/Blocks/Calendar/package.json scripts.test"
assert_contains "${WORK_DIR}/nested.out" "NPM_CWD:${calendar_package}"
assert_contains "${WORK_DIR}/nested.out" "NPM_INVOKED:run test -- src/frontend.test.ts"
assert_not_contains "${WORK_DIR}/nested.out" "run test -- inc/Blocks/Calendar/src/frontend.test.ts"
assert_contains "${WORK_DIR}/nested.out" "Package: ${player_package}"
assert_contains "${WORK_DIR}/nested.out" "Contract: blocks/Player/package.json scripts.test:js"
assert_contains "${WORK_DIR}/nested.out" "NPM_CWD:${player_package}"
assert_contains "${WORK_DIR}/nested.out" "NPM_INVOKED:run test:js -- src/player.test.js"
assert_contains "${WORK_DIR}/nested.out" "JS_TEST_SUMMARY:backend=package-script script=test:unit files=1 status=ok"
assert_contains "${WORK_DIR}/nested.out" "JS_TEST_SUMMARY:backend=package-script script=test files=1 status=ok"
assert_contains "${WORK_DIR}/nested.out" "JS_TEST_SUMMARY:backend=package-script script=test:js files=1 status=ok"

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

# --- Nested package without a runner: diagnostics identify the owner --------
missing_owner_component="${WORK_DIR}/missing-owner-component"
missing_owner_package="${missing_owner_component}/blocks/widget"
mkdir -p "${missing_owner_package}/src"
cat > "${missing_owner_package}/package.json" <<'JSON'
{
  "name": "widget",
  "scripts": {}
}
JSON
cat > "${missing_owner_package}/src/widget.test.js" <<'JS'
describe( 'widget', () => {
	it( 'has no package runner', () => expect( true ).toBe( true ) );
} );
JS

set +e
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="missing-owner" \
HOMEBOY_COMPONENT_PATH="$missing_owner_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES="blocks/widget/src/widget.test.js" \
    bash "$RUNNER" > "${WORK_DIR}/missing-owner.out" 2>&1
status=$?
set -e

if [ "$status" -ne 2 ]; then
    echo "Expected a nested package without a runner to exit 2, got $status" >&2
    sed 's/^/  /' "${WORK_DIR}/missing-owner.out" >&2
    exit 1
fi
assert_contains "${WORK_DIR}/missing-owner.out" "owning package ${missing_owner_package}"
assert_contains "${WORK_DIR}/missing-owner.out" "${missing_owner_package}/package.json declares no JavaScript test script"

# --- Declared-script runs must publish structured counts (#2778) -----------
# JS_TEST_SUMMARY carries no passed=/failed= tokens, so the generic parser
# resolved zero counts and graded a passing JavaScript-only scope as a
# failure. Jest and wp-scripts print their summary on stderr, so these cases
# also pin that the runner captures both streams.
counts_component="${WORK_DIR}/counts-component"
mkdir -p "${counts_component}/src" "${counts_component}/node_modules"
cat > "${counts_component}/package.json" <<'JSON'
{
  "name": "counts",
  "scripts": {
    "test:unit": "wp-scripts test-unit-js"
  }
}
JSON
cat > "${counts_component}/src/counts.test.js" <<'JS'
describe( 'counts', () => {
	it( 'reports structured counts', () => expect( true ).toBe( true ) );
} );
JS

counts_write_helper="${WORK_DIR}/write-test-results.sh"
cat > "$counts_write_helper" <<'SH'
homeboy_write_test_results() {
    printf 'total=%s passed=%s failed=%s skipped=%s partial=%s\n' \
        "$1" "$2" "$3" "$4" "${5:-}" > "$HOMEBOY_TEST_RESULTS_FILE"
}
SH

# Jest reports its summary on stderr and exits 0.
cat > "${stubs}/npm" <<'SH'
#!/usr/bin/env bash
set -uo pipefail
printf 'NPM_INVOKED:%s\n' "$*"
if [ "${1:-}" = "run" ]; then
    printf 'Tests:       8 passed, 8 total\n' >&2
fi
SH
chmod +x "${stubs}/npm"

counts_results="${WORK_DIR}/counts-pass.txt"
PATH="${stubs}:${PATH}" \
HOMEBOY_TEST_RESULTS_FILE="$counts_results" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$counts_write_helper" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="counts" \
HOMEBOY_COMPONENT_PATH="$counts_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES="src/counts.test.js" \
    bash "$RUNNER" > "${WORK_DIR}/counts-pass.out" 2>&1

assert_contains "${WORK_DIR}/counts-pass.out" "JS_TEST_SUMMARY:backend=package-script script=test:unit files=1 status=ok"
# The defect reported total=0 for this run, which the phase graded as failed.
assert_contains "$counts_results" "total=8 passed=8 failed=0 skipped=0"

# A failing declared run must still report its failures, never a passing shape.
cat > "${stubs}/npm" <<'SH'
#!/usr/bin/env bash
set -uo pipefail
printf 'NPM_INVOKED:%s\n' "$*"
if [ "${1:-}" = "run" ]; then
    printf 'Tests:       2 failed, 6 passed, 8 total\n' >&2
    exit 1
fi
SH
chmod +x "${stubs}/npm"

counts_fail_results="${WORK_DIR}/counts-fail.txt"
set +e
PATH="${stubs}:${PATH}" \
HOMEBOY_TEST_RESULTS_FILE="$counts_fail_results" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$counts_write_helper" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="counts" \
HOMEBOY_COMPONENT_PATH="$counts_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES="src/counts.test.js" \
    bash "$RUNNER" > "${WORK_DIR}/counts-fail.out" 2>&1
counts_status=$?
set -e

if [ "$counts_status" -eq 0 ]; then
    echo "Expected a failing declared JavaScript run to exit non-zero" >&2
    sed 's/^/  /' "${WORK_DIR}/counts-fail.out" >&2
    exit 1
fi
assert_contains "${WORK_DIR}/counts-fail.out" "JS_TEST_SUMMARY:backend=package-script script=test:unit files=1 status=failed exit=1"
assert_contains "$counts_fail_results" "total=8 passed=6 failed=2 skipped=0"

# A declared runner that prints nothing parseable must still not report a
# passing run as zero executed tests.
cat > "${stubs}/npm" <<'SH'
#!/usr/bin/env bash
set -uo pipefail
printf 'NPM_INVOKED:%s\n' "$*"
SH
chmod +x "${stubs}/npm"

counts_opaque_results="${WORK_DIR}/counts-opaque.txt"
PATH="${stubs}:${PATH}" \
HOMEBOY_TEST_RESULTS_FILE="$counts_opaque_results" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$counts_write_helper" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="counts" \
HOMEBOY_COMPONENT_PATH="$counts_component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_CHANGED_TEST_FILES="src/counts.test.js" \
    bash "$RUNNER" > "${WORK_DIR}/counts-opaque.out" 2>&1

assert_contains "$counts_opaque_results" "total=1 passed=1 failed=0 skipped=0 partial=declared-js-files"

echo "WordPress JavaScript test framework routing smoke passed"
