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
        echo "Actual contents:" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local unexpected="$2"
    if grep -Fq "$unexpected" "$file"; then
        echo "Expected $file not to contain: $unexpected" >&2
        echo "Actual contents:" >&2
        sed 's/^/  /' "$file" >&2
        exit 1
    fi
}

make_component() {
    local target="$1"
    mkdir -p "$target/tests/nested"
    cat > "$target/tests/alpha-smoke.php" <<'PHP'
<?php
fwrite( STDOUT, "alpha ok\n" );
PHP
    cat > "$target/tests/nested/bravo-smoke.php" <<'PHP'
<?php
fwrite( STDOUT, "bravo ok\n" );
PHP
}

component_pass="${TMPDIR}/component-pass"
make_component "$component_pass"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component-pass" \
HOMEBOY_COMPONENT_PATH="$component_pass" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke.sh" > "${TMPDIR}/direct-pass.out"

assert_contains "${TMPDIR}/direct-pass.out" "HOST_SMOKE_BEGIN:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/direct-pass.out" "HOST_SMOKE_BEGIN:tests/nested/bravo-smoke.php"
assert_contains "${TMPDIR}/direct-pass.out" "HOST_SMOKE_SUMMARY:passed=2 failed=0"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component-pass" \
HOMEBOY_COMPONENT_PATH="$component_pass" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILE="tests/nested/bravo-smoke.php" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke.sh" > "${TMPDIR}/direct-single.out"

assert_not_contains "${TMPDIR}/direct-single.out" "HOST_SMOKE_BEGIN:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/direct-single.out" "HOST_SMOKE_BEGIN:tests/nested/bravo-smoke.php"
assert_contains "${TMPDIR}/direct-single.out" "HOST_SMOKE_SUMMARY:passed=1 failed=0"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component-pass" \
HOMEBOY_COMPONENT_PATH="$component_pass" \
HOMEBOY_WORDPRESS_HOST_SMOKE_FILES=$'tests/nested/bravo-smoke.php\ntests/alpha-smoke.php' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke.sh" > "${TMPDIR}/direct-selected-list.out"

assert_contains "${TMPDIR}/direct-selected-list.out" "HOST_SMOKE_BEGIN:tests/nested/bravo-smoke.php"
assert_contains "${TMPDIR}/direct-selected-list.out" "HOST_SMOKE_BEGIN:tests/alpha-smoke.php"
assert_contains "${TMPDIR}/direct-selected-list.out" "HOST_SMOKE_SUMMARY:passed=2 failed=0"

component_fail="${TMPDIR}/component-fail"
make_component "$component_fail"
cat > "$component_fail/tests/zzz-smoke.php" <<'PHP'
<?php
fwrite( STDERR, "zzz failed\n" );
exit( 7 );
PHP

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component-fail" \
HOMEBOY_COMPONENT_PATH="$component_fail" \
    bash "${EXTENSION_PATH}/scripts/test/test-runner-host-smoke.sh" > "${TMPDIR}/direct-fail.out" 2>&1
exit_code=$?
set -e

if [ "$exit_code" -ne 7 ]; then
    echo "Expected failing smoke runner to exit 7, got $exit_code" >&2
    sed 's/^/  /' "${TMPDIR}/direct-fail.out" >&2
    exit 1
fi
assert_contains "${TMPDIR}/direct-fail.out" "HOST_SMOKE_FAIL:tests/zzz-smoke.php:exit=7"
assert_contains "${TMPDIR}/direct-fail.out" "Host smoke test failed: tests/zzz-smoke.php"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component-pass" \
HOMEBOY_COMPONENT_PATH="$component_pass" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_SETTINGS_JSON='{"test_backend":"host-smoke"}' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/router-host-smoke.out"
assert_contains "${TMPDIR}/router-host-smoke.out" "Backend: host-smoke"
assert_not_contains "${TMPDIR}/router-host-smoke.out" "Backend: playground"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component-pass" \
HOMEBOY_COMPONENT_PATH="$component_pass" \
HOMEBOY_COMPONENT_SHAPE="plugin" \
HOMEBOY_SETTINGS_JSON='{"test_backend":"host"}' \
    bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${TMPDIR}/router-host-alias.out"
assert_contains "${TMPDIR}/router-host-alias.out" "Backend: host-smoke"

echo "Host smoke runner smoke passed"
