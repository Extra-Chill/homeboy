#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

assert_contains() {
    local file="$1"
    local expected="$2"
    if ! grep -Fq "$expected" "$file"; then
        echo "Expected $file to contain: $expected" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq "$unexpected" "$file"; then
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
// PHPUnit-shaped file; the playground backend owns execution.
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
HOMEBOY_RUNTIME_TEST_RUNNER_PLAYGROUND="${TMPDIR}/stubs/playground.sh" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --file tests/Unit/ImportAgentAbilityTest.php --filter ImportAgent > "${TMPDIR}/phpunit-file.out"

assert_contains "${TMPDIR}/phpunit-file.out" "PLAYGROUND_STUB"
assert_contains "${TMPDIR}/phpunit-file.out" "SELECTED=tests/Unit/ImportAgentAbilityTest.php"
assert_contains "${TMPDIR}/phpunit-file.out" "ARGS=--filter ImportAgent"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_RUNTIME_TEST_RUNNER_PLAYGROUND="${TMPDIR}/stubs/playground.sh" \
HOMEBOY_CHANGED_TEST_FILES=$'tests/import-agent-ability-smoke.php\ntests/Unit/ImportAgentAbilityTest.php' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/changed-mixed-files.out"

assert_contains "${TMPDIR}/changed-mixed-files.out" "PLAYGROUND_STUB"
assert_contains "${TMPDIR}/changed-mixed-files.out" "CHANGED=tests/import-agent-ability-smoke.php"
assert_not_contains "${TMPDIR}/changed-mixed-files.out" "HOST_SMOKE_BEGIN:tests/import-agent-ability-smoke.php"

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
