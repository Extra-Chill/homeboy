#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

component="${TMPDIR}/component"
runtime_stub="${TMPDIR}/runtime.sh"
mkdir -p "${component}/tests"
printf '%s\n' '<?php // Standalone smoke, not PHPUnit.' > "${component}/tests/contract-smoke.php"

cat > "$runtime_stub" <<'SH'
#!/usr/bin/env bash
echo "PHPUNIT_RUNTIME_INVOKED"
SH
chmod +x "$runtime_stub"

run_router() {
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="$runtime_stub" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh"
}

HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"skip"}' run_router > "${TMPDIR}/skip.out"
grep -Fq "Skipping PHPUnit: no canonical test files were discovered." "${TMPDIR}/skip.out"
if grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/skip.out"; then
    echo "Skip policy must not invoke PHPUnit for a smoke-only component." >&2
    exit 1
fi

set +e
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"fail"}' run_router > "${TMPDIR}/fail.out" 2>&1
status=$?
set -e
if [ "$status" -ne 1 ]; then
    echo "Expected fail policy to exit 1, got ${status}." >&2
    exit 1
fi
grep -Fq "ERROR: no canonical PHPUnit test files were discovered." "${TMPDIR}/fail.out"

HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"skip","wp_codebox_phpunit_test_root":"/unresolved/tests"}' run_router > "${TMPDIR}/explicit.out"
grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/explicit.out"

printf '%s\n' '<?php // Canonical PHPUnit test.' > "${component}/tests/ContractTest.php"
HOMEBOY_SETTINGS_JSON='{"phpunit_no_tests":"skip"}' run_router > "${TMPDIR}/phpunit.out"
grep -Fq "PHPUNIT_RUNTIME_INVOKED" "${TMPDIR}/phpunit.out"

echo "Full-suite no-PHPUnit routing smoke passed"
