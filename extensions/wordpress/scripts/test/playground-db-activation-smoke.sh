#!/usr/bin/env bash
#
# Test runner DB-touching activation smoke (homeboy-extensions#431).
#
# Pins the test runner's post-install activation behavior so a future
# refactor cannot silently regress to the pre-#431 shape that fired
# `activate_<plugin>` inline during muplugins_loaded — before wp-phpunit's
# install.php had created the wptests_* tables. Mirrors the bench-side
# smoke with PHPUnit assertions instead of workload metrics.
#
# Asserts (via PHPUnit inside the runner):
#   1. The dep's add_option() activation write succeeded (wptests_options
#      existed when activation fired).
#   2. The dep's get_users() activation read succeeded (wptests_users
#      existed when activation fired).
#   3. The dep's plugins_loaded callback still fires (don't lose
#      homeboy-extensions#426/#427).
#
# Pre-#431 the dep's activation hook fataled with "no such table:
# wptests_options" / "no such table: wptests_users" and the entire test run
# crashed at the install boundary.
#
# Usage: bash wordpress/scripts/test/playground-db-activation-smoke.sh
# Exit:  0 = test runner activates plugins post-install, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_PATH}/../.." && pwd)/homeboy}"
CORE_RUNTIME_DIR="${HOMEBOY_CORE_DIR}/crates/homeboy-core/src/extension/runtime"
# The test runner chain (test-runner.sh -> test-runner-wp-codebox.sh) sources the
# core runtime helpers and hard-requires their HOMEBOY_RUNTIME_* env vars. The
# shared-core fallbacks were removed in homeboy-extensions e0f604a4, so direct
# (non-`homeboy test`) invocations must point those vars at the core helpers,
# matching the bench/build smokes. `homeboy test` exports these automatically.
RUNNER_PRELUDE_HELPER="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:-${CORE_RUNTIME_DIR}/runner-prelude.sh}"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-${CORE_RUNTIME_DIR}/resolve-context.sh}"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${CORE_RUNTIME_DIR}/runner-steps.sh}"
HOST_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/test-db-activation-host"
DEP_FIXTURE_DIR="${EXTENSION_PATH}/tests/fixtures/bench-db-activation-dep"
ARTIFACTS_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-wp-codebox-db-activation.XXXXXX")"
trap 'rm -rf "$ARTIFACTS_DIR"' EXIT

for core_helper in "$RUNNER_PRELUDE_HELPER" "$RESOLVE_CONTEXT_HELPER" "$RUNNER_STEPS_HELPER"; do
    if [ ! -f "$core_helper" ]; then
        echo "ERROR: core runtime helper not found at $core_helper" >&2
        echo "Set the HOMEBOY_RUNTIME_* helper vars or check out homeboy core at $HOMEBOY_CORE_DIR" >&2
        exit 1
    fi
done

if [ ! -d "$HOST_FIXTURE_DIR" ]; then
    echo "ERROR: host fixture not found at $HOST_FIXTURE_DIR" >&2
    exit 1
fi

if [ ! -d "$DEP_FIXTURE_DIR" ]; then
    echo "ERROR: dep fixture not found at $DEP_FIXTURE_DIR" >&2
    exit 1
fi

if [ ! -d "${EXTENSION_PATH}/vendor/wp-phpunit" ]; then
    echo "ERROR: wp-phpunit not installed." >&2
    echo "Run: cd ${EXTENSION_PATH} && composer install" >&2
    exit 1
fi

# Pass the dep fixture as an absolute-path validation dependency. The
# resolver hits the direct-path branch first so we avoid homeboy / git fallbacks.
SETTINGS_JSON=$(cat <<EOF
{"validation_dependencies": ["${DEP_FIXTURE_DIR}"]}
EOF
)

echo "============================================"
echo "Test runner DB-touching activation smoke (#431)"
echo "============================================"
echo "Host fixture: $HOST_FIXTURE_DIR"
echo "Dep fixture:  $DEP_FIXTURE_DIR"
echo ""

source_hash_before=$(tar -C "$(dirname "$HOST_FIXTURE_DIR")" -cf - "$(basename "$HOST_FIXTURE_DIR")" | shasum -a 256)
set +e
runner_output=$(HOMEBOY_COMPONENT_ID=test-db-activation-host \
    HOMEBOY_COMPONENT_PATH="$HOST_FIXTURE_DIR" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
    HOMEBOY_RUNTIME_RUNNER_STEPS="$RUNNER_STEPS_HELPER" \
    HOMEBOY_SETTINGS_JSON="$SETTINGS_JSON" \
    HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR="$ARTIFACTS_DIR" \
    bash "${SCRIPT_DIR}/test-runner.sh" 2>&1)
runner_status=$?
set -e
printf '%s\n' "$runner_output"

if [ "$runner_status" -ne 0 ]; then
    echo "ERROR: WP Codebox PHPUnit runner exited with status ${runner_status}" >&2
    exit "$runner_status"
fi

if [[ "$runner_output" != *"WP Codebox test run complete."* ]]; then
    echo "ERROR: expected WP Codebox PHPUnit runner success classification" >&2
    exit 1
fi
run_artifacts_dir=""
for candidate in "${ARTIFACTS_DIR}"/wp-codebox-phpunit.*; do
    [ -d "$candidate" ] && run_artifacts_dir="$candidate"
done
runtime_artifact_directory=$(jq -r '.paths.runtimeDirectory // empty' "${run_artifacts_dir}/latest-runtime.json")
test_results_artifact="${run_artifacts_dir}/${runtime_artifact_directory}/files/test-results.json"
if [ "$(jq -r '.status // empty' "$test_results_artifact")" != "passed" ] || [ "$(jq -r '.summary.total // 0' "$test_results_artifact")" -le 0 ]; then
    echo "ERROR: expected normalized passing PHPUnit test-results.json" >&2
    exit 1
fi
result_artifact="${run_artifacts_dir}/${runtime_artifact_directory}/files/phpunit/.pg-test-result.txt"
if [ ! -f "$result_artifact" ] || ! grep -q '^STAGE_BEGIN:run_tests' "$result_artifact"; then
    echo "ERROR: expected WP Codebox structured PHPUnit diagnostic artifact" >&2
    exit 1
fi
source_hash_after=$(tar -C "$(dirname "$HOST_FIXTURE_DIR")" -cf - "$(basename "$HOST_FIXTURE_DIR")" | shasum -a 256)
if [ "$source_hash_before" != "$source_hash_after" ]; then
    echo "ERROR: readonly WP Codebox PHPUnit source fixture changed" >&2
    exit 1
fi

echo ""
echo "============================================"
echo "✓ Test runner DB-touching activation smoke PASSED"
echo "============================================"
