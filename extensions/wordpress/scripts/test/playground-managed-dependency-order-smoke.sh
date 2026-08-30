#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_PATH}/../.." && pwd)/homeboy}"
CORE_RUNTIME_DIR="${HOMEBOY_CORE_DIR}/crates/homeboy-core/src/extension/runtime"
HOST_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/test-managed-dependency-order-host"
DEP_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/managed-dependency-order-dep"
ARTIFACTS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-managed-dependency-order.XXXXXX")"
trap 'rm -rf "$ARTIFACTS_DIR"' EXIT

if [ -e "${DEP_FIXTURE_DIR}/vendor/autoload.php" ]; then
    echo "ERROR: dependency fixture unexpectedly has a prepared Composer autoloader" >&2
    exit 1
fi

SETTINGS_JSON=$(jq -nc --arg dependency "$DEP_FIXTURE_DIR" '{validation_dependencies: [$dependency]}')

set +e
runner_output=$(HOMEBOY_COMPONENT_ID=test-managed-dependency-order-host \
    HOMEBOY_COMPONENT_PATH="$HOST_FIXTURE_DIR" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${CORE_RUNTIME_DIR}/runner-prelude.sh}" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${CORE_RUNTIME_DIR}/resolve-context.sh}" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${CORE_RUNTIME_DIR}/runner-steps.sh}" \
    HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="$ARTIFACTS_DIR" \
    bash "${SCRIPT_DIR}/test-runner.sh" 2>&1)
runner_status=$?
set -e
printf '%s\n' "$runner_output"

if [ "$runner_status" -ne 0 ]; then
    exit "$runner_status"
fi

if [[ "$runner_output" =~ Constant\ (DB_NAME|DB_USER|DB_HOST|ABSPATH)\ already\ defined ]] || \
   [[ "$runner_output" == *'Undefined variable $wpdb'* ]]; then
    echo "ERROR: dependency loaded before the managed PHPUnit install completed" >&2
    exit 1
fi

if [[ "$runner_output" != *"WP Codebox test run complete."* ]]; then
    echo "ERROR: expected managed PHPUnit success classification" >&2
    exit 1
fi

echo "Managed PHPUnit dependency ordering smoke passed."
