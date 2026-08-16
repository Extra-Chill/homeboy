#!/usr/bin/env bash
set -euo pipefail

# Regression: homeboy-extensions#2644. WordPress inventories were split into
# immutable manifests, but replay ignored the manifest and ran the full suite.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    exit 1
}

assert_contains() {
    grep -Fq -- "$2" "$1" || fail "expected $1 to contain: $2"
}

assert_not_contains() {
    if grep -Fq -- "$2" "$1"; then
        fail "expected $1 not to contain: $2"
    fi
}

component="${WORKDIR}/component"
mkdir -p "${component}/tests/Unit" "${WORKDIR}/stubs"

jq -e '
    .test.portable_env.keys as $keys
    | all(["HOMEBOY_TEST_INVENTORY_ONLY", "HOMEBOY_TEST_INVENTORY_FILE", "HOMEBOY_TEST_SHARD_MANIFEST"][]; . as $key | $keys | index($key) != null)
' "${EXTENSION_PATH}/wordpress.json" >/dev/null || fail 'wordpress.json does not preserve the shard selection environment'

printf '<?php\nclass AlphaTest extends PHPUnit\\Framework\\TestCase {}\n' > "${component}/tests/Unit/AlphaTest.php"
printf '<?php\nclass BetaTest extends PHPUnit\\Framework\\TestCase {}\n' > "${component}/tests/Unit/BetaTest.php"
printf '<?php\necho "standalone smoke ran\\n";\n' > "${component}/tests/standalone-smoke.php"
printf '<?php\necho "wordpress smoke ran\\n";\n' > "${component}/tests/wordpress-smoke.php"
printf 'console.log("js smoke ran");\n' > "${component}/tests/browser-smoke.js"
printf 'import test from "node:test";\ntest("node shard", () => console.log("node test ran"));\n' > "${component}/tests/worker.test.mjs"
printf '#!/usr/bin/env bash\necho "shell smoke ran"\n' > "${component}/tests/contract-smoke.sh"
chmod +x "${component}/tests/contract-smoke.sh"
cat > "${component}/homeboy-test-manifest.json" <<'JSON'
{
  "schema": "homeboy/test-manifest/v1",
  "tests": {
    "tests/standalone-smoke.php": { "environment": "standalone-php" },
    "tests/wordpress-smoke.php": { "environment": "wordpress" }
  }
}
JSON

runner_prelude="${WORKDIR}/runner-prelude.sh"
cat > "$runner_prelude" <<'SH'
homeboy_runner_init() {
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
    PLUGIN_PATH="$COMPONENT_PATH"
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:?HOMEBOY_EXTENSION_PATH is required}"
}
SH

cat > "${WORKDIR}/stubs/wp-codebox.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'PHPUNIT_CHANGED=%s\n' "${HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES:-}"
while IFS= read -r test_file; do
    [ -z "$test_file" ] || printf 'PHPUNIT_EXECUTED:%s\n' "$test_file"
done <<< "${HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES:-}"
SH
chmod +x "${WORKDIR}/stubs/wp-codebox.sh"

cat > "${WORKDIR}/stubs/host-smoke-wp.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
while IFS= read -r test_file; do
    [ -z "$test_file" ] || printf 'HOST_SMOKE_OK:%s\n' "$test_file"
done <<< "${HOMEBOY_WORDPRESS_HOST_SMOKE_FILES:-}"
SH
chmod +x "${WORKDIR}/stubs/host-smoke-wp.sh"

cat > "${WORKDIR}/write-test-results.sh" <<'SH'
homeboy_write_test_results() {
    jq -n --argjson total "$1" --argjson passed "$2" --argjson failed "$3" --argjson skipped "$4" --arg partial "${5:-}" \
        '{total:$total,passed:$passed,failed:$failed,skipped:$skipped,partial:$partial}' > "$HOMEBOY_TEST_RESULTS_FILE"
}
SH

inventory="${WORKDIR}/inventory.json"
python3 "${EXTENSION_PATH}/scripts/test/test-inventory.py" \
    --project "$component" \
    --extension-path "$EXTENSION_PATH" \
    --runner wordpress \
    --package component \
    --output "$inventory" >/dev/null

write_manifest() {
    local target="$1" id="$2" tests_json canonical fingerprint
    shift 2
    tests_json="$(printf '%s\n' "$@" | jq -R . | jq -s .)"
    canonical="$(jq -cS --argjson tests "$tests_json" '
        ($tests | reduce .[] as $test_id ({}; .[$test_id] = true)) as $selected
        | {schema,runner,runner_fingerprint,workspace_fingerprint,tests:(.tests | map(select($selected[.id])) | sort_by(.id))}
    ' "$inventory")"
    fingerprint="$(printf '%s' "$canonical" | sha256sum | cut -d ' ' -f 1)"
    jq -cn --arg id "$id" --argjson tests "$tests_json" --arg fingerprint "$fingerprint" --slurpfile inventory "$inventory" \
        '{schema:"homeboy/test-shard-manifest/v1",id:$id,runner:$inventory[0].runner,runner_fingerprint:$inventory[0].runner_fingerprint,workspace_fingerprint:$inventory[0].workspace_fingerprint,inventory_fingerprint:$fingerprint,tests:$tests}' > "$target"
}

run_manifest() {
    local manifest="$1"
    local output="$2"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${WORKDIR}/stubs/host-smoke-wp.sh" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${WORKDIR}/stubs/wp-codebox.sh" \
    HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${WORKDIR}/write-test-results.sh" \
    HOMEBOY_TEST_RESULTS_FILE="${output}.results.json" \
    HOMEBOY_TEST_SHARD_MANIFEST="$manifest" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "$output" 2>&1
}

first="${WORKDIR}/shard-1.json"
second="${WORKDIR}/shard-2.json"
write_manifest "$first" shard-1 tests/Unit/AlphaTest.php tests/standalone-smoke.php tests/wordpress-smoke.php tests/browser-smoke.js tests/worker.test.mjs tests/contract-smoke.sh
write_manifest "$second" shard-2 tests/Unit/BetaTest.php

run_manifest "$first" "${WORKDIR}/first.out"
assert_contains "${WORKDIR}/first.out" 'TEST_SHARD_MANIFEST:id=shard-1 selected=6'
assert_contains "${WORKDIR}/first.out" 'PHP_SMOKE_OK:tests/standalone-smoke.php'
assert_contains "${WORKDIR}/first.out" 'HOST_SMOKE_OK:tests/wordpress-smoke.php'
assert_contains "${WORKDIR}/first.out" 'js smoke ran'
assert_contains "${WORKDIR}/first.out" 'node test ran'
assert_contains "${WORKDIR}/first.out" 'shell smoke ran'
assert_contains "${WORKDIR}/first.out" 'PHPUNIT_CHANGED=tests/Unit/AlphaTest.php'
assert_contains "${WORKDIR}/first.out" 'PHPUNIT_EXECUTED:tests/Unit/AlphaTest.php'
assert_contains "${WORKDIR}/first.out" 'TEST_SHARD_SUMMARY:id=shard-1 selected=6 routed=6 status=passed'
if [ "$(jq -r '.total' "${WORKDIR}/first.out.results.json")" -ne 6 ]; then
    fail 'shard result sidecar does not match first manifest membership'
fi
assert_not_contains "${WORKDIR}/first.out" 'BetaTest.php'

run_manifest "$second" "${WORKDIR}/second.out"
assert_contains "${WORKDIR}/second.out" 'TEST_SHARD_MANIFEST:id=shard-2 selected=1'
assert_contains "${WORKDIR}/second.out" 'PHPUNIT_CHANGED=tests/Unit/BetaTest.php'
assert_contains "${WORKDIR}/second.out" 'PHPUNIT_EXECUTED:tests/Unit/BetaTest.php'
assert_contains "${WORKDIR}/second.out" 'TEST_SHARD_SUMMARY:id=shard-2 selected=1 routed=1 status=passed'
if [ "$(jq -r '.total' "${WORKDIR}/second.out.results.json")" -ne 1 ]; then
    fail 'shard result sidecar does not match second manifest membership'
fi
assert_not_contains "${WORKDIR}/second.out" 'AlphaTest.php'
assert_not_contains "${WORKDIR}/second.out" 'standalone smoke ran'

HOMEBOY_TEST_SHARD_MANIFEST="$first" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${WORKDIR}/write-test-results.sh" \
HOMEBOY_TEST_RESULTS_FILE="${WORKDIR}/parsed-results.json" \
    bash "${EXTENSION_PATH}/scripts/test/parse-test-results.sh" "${WORKDIR}/first.out"
if [ "$(jq -r '.total' "${WORKDIR}/parsed-results.json")" -ne 6 ]; then
    fail 'result parser does not recover validated shard membership'
fi

malformed="${WORKDIR}/malformed.json"
printf '{"schema":"wrong","tests":["tests/Unit/AlphaTest.php"]}\n' > "$malformed"
if run_manifest "$malformed" "${WORKDIR}/malformed.out"; then
    fail 'malformed shard manifest was accepted'
fi
assert_contains "${WORKDIR}/malformed.out" 'ERROR: invalid WordPress test shard manifest'
assert_not_contains "${WORKDIR}/malformed.out" 'PHPUNIT_CHANGED='

stale="${WORKDIR}/stale.json"
jq '.workspace_fingerprint = ("d" * 64)' "$first" > "$stale"
if run_manifest "$stale" "${WORKDIR}/stale.out"; then
    fail 'stale shard manifest was accepted'
fi
assert_contains "${WORKDIR}/stale.out" 'is stale for the current runner or workspace'
assert_not_contains "${WORKDIR}/stale.out" 'PHPUNIT_EXECUTED:'

missing="${WORKDIR}/missing.json"
write_manifest "$missing" shard-3 tests/Unit/MissingTest.php
if run_manifest "$missing" "${WORKDIR}/missing.out"; then
    fail 'missing shard assignment was accepted'
fi
assert_contains "${WORKDIR}/missing.out" 'contains a test outside the current inventory'
assert_not_contains "${WORKDIR}/missing.out" 'PHPUNIT_CHANGED='

if HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${WORKDIR}/stubs/wp-codebox.sh" \
    HOMEBOY_TEST_SHARD_MANIFEST="$first" \
    HOMEBOY_CHANGED_TEST_FILES="tests/Unit/BetaTest.php" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${WORKDIR}/collision.out" 2>&1; then
    fail 'shard and changed-test selectors were accepted together'
fi
assert_contains "${WORKDIR}/collision.out" 'is mutually exclusive with other test selectors and passthrough arguments'

if HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${WORKDIR}/stubs/wp-codebox.sh" \
    HOMEBOY_TEST_SHARD_MANIFEST="$first" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" --filter Alpha > "${WORKDIR}/passthrough.out" 2>&1; then
    fail 'passthrough filter altered immutable shard replay'
fi
assert_contains "${WORKDIR}/passthrough.out" 'is mutually exclusive with other test selectors and passthrough arguments'

if HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
    HOMEBOY_COMPONENT_ID="component" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${WORKDIR}/stubs/wp-codebox.sh" \
    HOMEBOY_TEST_SHARD_MANIFEST="$first" \
    HOMEBOY_TEST_SCOPE_KIND="exclusive_env" \
    HOMEBOY_TEST_SCOPE_ENV_NAME="HOMEBOY_WORDPRESS_HOST_SMOKE_FILES" \
    HOMEBOY_TEST_SCOPE_ENV_VALUE="tests/standalone-smoke.php" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${WORKDIR}/exclusive.out" 2>&1; then
    fail 'exclusive scope bypassed the shard manifest'
fi
assert_contains "${WORKDIR}/exclusive.out" 'is mutually exclusive with HOMEBOY_TEST_SCOPE_KIND=exclusive_env'
assert_not_contains "${WORKDIR}/exclusive.out" 'standalone smoke ran'

printf 'All WordPress test shard replay checks passed.\n'
