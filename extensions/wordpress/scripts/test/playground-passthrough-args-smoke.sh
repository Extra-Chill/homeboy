#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/test-runner-playground.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_PATH="${TMPDIR}/extension"
PLUGIN_PATH="${TMPDIR}/component"
mkdir -p "${EXTENSION_PATH}/node_modules/.bin" "${PLUGIN_PATH}/tests"

cat > "${PLUGIN_PATH}/tests/OnlyTest.php" <<'PHP'
<?php
class OnlyTest extends WP_UnitTestCase {}
PHP

cat > "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

wrapper=""
runner_args=()
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--mount" ]; then
        mount_arg="$2"
        if [[ "$mount_arg" == *":/runner.php" ]]; then
            wrapper="${mount_arg%:/runner.php}"
        fi
        shift 2
        continue
    fi
    if [ "$1" = "--" ]; then
        shift
        runner_args=("$@")
        break
    fi
    shift
done

if [ -z "$wrapper" ] || [ ! -f "$wrapper" ]; then
    echo "runner wrapper mount not found" >&2
    exit 1
fi

printf 'PASSTHROUGH:'
printf ' [%s]' "${runner_args[@]}"
printf '\n'

if [ "${EXPECT_SELECTED_FILE:-}" = "1" ]; then
    expected_b64="$(printf '%s' 'tests/OnlyTest.php' | base64 | tr -d '\n')"
    if ! grep -Fq "$expected_b64" "$wrapper"; then
        echo "selected PHPUnit file was not substituted into wrapper" >&2
        exit 1
    fi
    echo "SELECTED_FILE_OK"
fi

cat > "${HOMEBOY_PLUGIN_PATH}/.pg-test-result.txt" <<'LOG'
STAGE_BEGIN:run_tests
ALL TESTS PASSED
TESTS: 1 FAILURES: 0 ERRORS: 0
STAGE_OK:run_tests
LOG
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/wp-playground-cli"

bash -n "$RUNNER"

filter_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="example" \
    bash "$RUNNER" --filter OnlyTest --list-tests 2>&1)

if [[ "$filter_output" != *"PASSTHROUGH: [/runner.php] [--filter] [OnlyTest] [--list-tests]"* ]]; then
    echo "Expected PHPUnit passthrough args to reach Playground runner" >&2
    echo "$filter_output" >&2
    exit 1
fi

file_output=$(HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_COMPONENT_PATH="$PLUGIN_PATH" \
    HOMEBOY_COMPONENT_ID="example" \
    EXPECT_SELECTED_FILE=1 \
    bash "$RUNNER" tests/OnlyTest.php 2>&1)

if [[ "$file_output" != *"SELECTED_FILE_OK"* ]]; then
    echo "Expected bare PHPUnit file arg to select that file in Playground wrapper" >&2
    echo "$file_output" >&2
    exit 1
fi

echo "Playground passthrough args smoke passed"
