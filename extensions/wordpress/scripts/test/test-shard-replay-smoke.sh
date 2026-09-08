#!/usr/bin/env bash
set -euo pipefail

# Regression: homeboy-extensions#2644. WordPress inventories were split into
# immutable manifests, but replay ignored the manifest and ran the full suite.
#
# Extra-Chill/homeboy-extensions#2719 — a shard may mix runners. Members the
# standalone declarations claim replay through the bounded host-PHP runner;
# everything else replays through the WordPress runtime backend. Both backends
# run even when the first fails, and the exit status is the worst of the two.

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
printf '<?php\nclass BehaviorSpec extends PHPUnit\\Framework\\TestCase {}\n' > "${component}/tests/Unit/behavior-spec.php"
printf '<?php echo "contract check ok\\n";\n' > "${component}/tests/contract-check.php"
printf '<?php echo "contract check failed\\n"; exit(1);\n' > "${component}/tests/failing-check.php"
printf 'import test from "node:test";\ntest("node shard", () => console.log("node test ran"));\n' > "${component}/tests/worker.test.mjs"

# The standalone declarations are part of the environment every replay runs
# with: shard validation regenerates the current inventory through the same
# declarations, and routing classifies assigned members with them.
standalone_settings='{"standalone_php_test_paths":["tests/contract-check.php","tests/failing-check.php"]}'

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
if [ "${HOMEBOY_WORDPRESS_PHPUNIT_DISCOVERY_ONLY:-}" = "1" ]; then
    jq -cn '{schema:"wp-codebox/phpunit-discovery/v1",plugin_slug:"component",phpunit_xml:"/wordpress/wp-content/plugins/component/phpunit.xml.dist",test_root:"/wordpress/wp-content/plugins/component/tests",selected_testsuites:[],files:["/wordpress/wp-content/plugins/component/tests/Unit/AlphaTest.php","/wordpress/wp-content/plugins/component/tests/Unit/BetaTest.php","/wordpress/wp-content/plugins/component/tests/Unit/behavior-spec.php"]}'
    exit 0
fi
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
discovery="${WORKDIR}/discovery.json"
standalone_list="${WORKDIR}/standalone-list.txt"
printf '%s\n' 'tests/contract-check.php' 'tests/failing-check.php' > "$standalone_list"
HOMEBOY_WORDPRESS_PHPUNIT_DISCOVERY_ONLY=1 "${WORKDIR}/stubs/wp-codebox.sh" > "$discovery"
python3 "${EXTENSION_PATH}/scripts/test/test-inventory.py" \
    --project "$component" \
    --extension-path "$EXTENSION_PATH" \
    --runner wordpress \
    --package component \
    --discovery-file "$discovery" \
    --standalone-list "$standalone_list" \
    --output "$inventory" >/dev/null

mixed_members="$(python3 - "$inventory" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
members = [(test["id"], test["target"]) for test in doc["tests"]]
expected = [
    ("tests/Unit/AlphaTest.php", "phpunit"),
    ("tests/Unit/BetaTest.php", "phpunit"),
    ("tests/Unit/behavior-spec.php", "phpunit"),
    ("tests/contract-check.php", "standalone-php"),
    ("tests/failing-check.php", "standalone-php"),
]
raise SystemExit(0 if members == expected else f"unexpected inventory membership: {members}")
PY
)" || fail "base inventory membership does not mix runners: ${mixed_members}"
printf 'PASS: the planned inventory carries both runner identities\n'

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
    HOMEBOY_SETTINGS_JSON="$standalone_settings" \
    HOMEBOY_RUNTIME_TEST_RUNNER_HOST_SMOKE_WP="${WORKDIR}/stubs/host-smoke-wp.sh" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="${WORKDIR}/stubs/wp-codebox.sh" \
    HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${WORKDIR}/write-test-results.sh" \
    HOMEBOY_TEST_RESULTS_FILE="${output}.results.json" \
    HOMEBOY_TEST_SHARD_MANIFEST="$manifest" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "$output" 2>&1
}

first="${WORKDIR}/shard-1.json"
second="${WORKDIR}/shard-2.json"
write_manifest "$first" shard-1 tests/Unit/behavior-spec.php
write_manifest "$second" shard-2 tests/Unit/BetaTest.php

run_manifest "$first" "${WORKDIR}/first.out"
assert_contains "${WORKDIR}/first.out" 'TEST_SHARD_MANIFEST:id=shard-1 selected=1'
assert_contains "${WORKDIR}/first.out" 'PHPUNIT_CHANGED=tests/Unit/behavior-spec.php'
assert_contains "${WORKDIR}/first.out" 'PHPUNIT_EXECUTED:tests/Unit/behavior-spec.php'
assert_contains "${WORKDIR}/first.out" 'TEST_SHARD_SUMMARY:id=shard-1 selected=1 routed=1 status=passed'
if [ "$(jq -r '.total' "${WORKDIR}/first.out.results.json")" -ne 1 ]; then
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

# A mixed shard routes each member to the runner its declarations select and
# reports validated membership across both backends.
mixed="${WORKDIR}/shard-mixed.json"
write_manifest "$mixed" shard-3 tests/Unit/AlphaTest.php tests/contract-check.php
run_manifest "$mixed" "${WORKDIR}/mixed.out"
assert_contains "${WORKDIR}/mixed.out" 'TEST_SHARD_MANIFEST:id=shard-3 selected=2'
assert_contains "${WORKDIR}/mixed.out" 'TEST_SHARD_ROUTE:tests/Unit/AlphaTest.php:runner=phpunit'
assert_contains "${WORKDIR}/mixed.out" 'TEST_SHARD_ROUTE:tests/contract-check.php:runner=host-php-smoke'
assert_contains "${WORKDIR}/mixed.out" 'PHPUNIT_CHANGED=tests/Unit/AlphaTest.php'
assert_contains "${WORKDIR}/mixed.out" 'PHPUNIT_EXECUTED:tests/Unit/AlphaTest.php'
assert_contains "${WORKDIR}/mixed.out" 'PHP_SMOKE_BEGIN:tests/contract-check.php'
assert_contains "${WORKDIR}/mixed.out" 'PHP_SMOKE_OK:tests/contract-check.php'
assert_contains "${WORKDIR}/mixed.out" 'TEST_SHARD_SUMMARY:id=shard-3 selected=2 routed=2 status=passed'
if [ "$(jq -r '.total' "${WORKDIR}/mixed.out.results.json")" -ne 2 ]; then
    fail 'shard result sidecar does not match mixed manifest membership'
fi
assert_not_contains "${WORKDIR}/mixed.out" 'PHPUNIT_CHANGED=tests/Unit/AlphaTest.php tests/contract-check.php'
assert_not_contains "${WORKDIR}/mixed.out" 'HOST_SMOKE_OK:'

# A standalone-only shard never invokes the PHPUnit backend at all.
solo_shard="${WORKDIR}/shard-solo.json"
write_manifest "$solo_shard" shard-4 tests/contract-check.php
run_manifest "$solo_shard" "${WORKDIR}/solo.out"
assert_contains "${WORKDIR}/solo.out" 'TEST_SHARD_ROUTE:tests/contract-check.php:runner=host-php-smoke'
assert_contains "${WORKDIR}/solo.out" 'PHP_SMOKE_OK:tests/contract-check.php'
assert_contains "${WORKDIR}/solo.out" 'TEST_SHARD_SUMMARY:id=shard-4 selected=1 routed=1 status=passed'
assert_not_contains "${WORKDIR}/solo.out" 'PHPUNIT_CHANGED='
assert_not_contains "${WORKDIR}/solo.out" 'PHPUNIT_EXECUTED:'
if [ "$(jq -r '.total' "${WORKDIR}/solo.out.results.json")" -ne 1 ]; then
    fail 'shard result sidecar does not match standalone-only manifest membership'
fi

# A failing standalone member must not be erased by a passing PHPUnit half:
# both backends run and the exit status is the worst of the two.
worst="${WORKDIR}/shard-worst.json"
write_manifest "$worst" shard-5 tests/Unit/AlphaTest.php tests/failing-check.php
set +e
run_manifest "$worst" "${WORKDIR}/worst.out"
worst_status=$?
set -e
if [ "$worst_status" -eq 0 ]; then
    fail 'a failing standalone member was replayed as passing'
fi
assert_contains "${WORKDIR}/worst.out" 'TEST_SHARD_ROUTE:tests/failing-check.php:runner=host-php-smoke'
assert_contains "${WORKDIR}/worst.out" 'PHP_SMOKE_FAIL:tests/failing-check.php:exit=1'
assert_contains "${WORKDIR}/worst.out" 'PHPUNIT_EXECUTED:tests/Unit/AlphaTest.php'
assert_contains "${WORKDIR}/worst.out" 'TEST_SHARD_SUMMARY:id=shard-5 selected=2 routed=2 status=failed'
assert_not_contains "${WORKDIR}/worst.out" 'status=passed'

HOMEBOY_TEST_SHARD_MANIFEST="$first" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${WORKDIR}/write-test-results.sh" \
HOMEBOY_TEST_RESULTS_FILE="${WORKDIR}/parsed-results.json" \
    bash "${EXTENSION_PATH}/scripts/test/parse-test-results.sh" "${WORKDIR}/first.out"
if [ "$(jq -r '.total' "${WORKDIR}/parsed-results.json")" -ne 1 ]; then
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
    HOMEBOY_TEST_SCOPE_ENV_VALUE="tests/diagnostic-smoke.php" \
        bash "${EXTENSION_PATH}/scripts/test/test-runner.sh" > "${WORKDIR}/exclusive.out" 2>&1; then
    fail 'exclusive scope bypassed the shard manifest'
fi
assert_contains "${WORKDIR}/exclusive.out" 'is mutually exclusive with HOMEBOY_TEST_SCOPE_KIND=exclusive_env'
assert_not_contains "${WORKDIR}/exclusive.out" 'PHP_SMOKE_BEGIN:'

printf 'All WordPress test shard replay checks passed.\n'
