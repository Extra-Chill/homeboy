#!/usr/bin/env bash
# Regression: #2978 — failed shard logs must retain standalone and PHPUnit IDs.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/component/tests"
printf '<?php exit(255);\n' > "$work/component/tests/failing-smoke.php"
printf '<?php // passing smoke\n' > "$work/component/tests/passing-smoke.php"
cat > "$work/writer.sh" <<'SH'
homeboy_write_test_results() {
    jq -n --argjson total "$1" --argjson passed "$2" --argjson failed "$3" --argjson skipped "$4" \
        '{total:$total,passed:$passed,failed:$failed,skipped:$skipped}' > "$HOMEBOY_TEST_RESULTS_FILE"
}
SH
cat > "$work/prelude.sh" <<'SH'
homeboy_runner_init() {
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$COMPONENT_PATH"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
SH
export HOMEBOY_RUNTIME_RUNNER_PRELUDE="$work/prelude.sh"
export HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH"
export HOMEBOY_COMPONENT_PATH="$work/component"
export HOMEBOY_COMPONENT_ID=component HOMEBOY_COMPONENT_SHAPE=plugin
export HOMEBOY_SETTINGS_JSON='{}'
export HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$work/writer.sh"
export HOMEBOY_TEST_RESULTS_FILE="$work/counts.json"
export HOMEBOY_TEST_FAILURES_FILE="$work/failures.json"
printf '%s\n' '{"schema":"wp-codebox/phpunit-discovery/v1","plugin_slug":"component","files":[]}' > "$work/discovery.json"
python3 "$SCRIPT_DIR/test-inventory.py" --discovery-file "$work/discovery.json" --project "$HOMEBOY_COMPONENT_PATH" \
    --extension-path "$EXTENSION_PATH" --runner wordpress --package component --output "$work/inventory.json" >/dev/null
python3 - "$work/inventory.json" "$work/shard.json" <<'PY'
import hashlib, json, sys
inventory = json.load(open(sys.argv[1]))
tests = sorted(t['id'] for t in inventory['tests'])
canonical = {k: inventory[k] for k in ('schema', 'runner', 'runner_fingerprint', 'workspace_fingerprint')}
canonical['tests'] = sorted(inventory['tests'], key=lambda t: t['id'])
fingerprint = hashlib.sha256(json.dumps(canonical, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
manifest = {k: inventory[k] for k in ('runner', 'runner_fingerprint', 'workspace_fingerprint')}
manifest.update(schema='homeboy/test-shard-manifest/v1', id='shard-1', tests=tests, inventory_fingerprint=fingerprint)
json.dump(manifest, open(sys.argv[2], 'w'))
PY
cat > "$work/runtime.sh" <<SH
#!/usr/bin/env bash
if [ "\${HOMEBOY_WORDPRESS_PHPUNIT_DISCOVERY_ONLY:-}" = 1 ]; then
    printf '%s\\n' '{"schema":"wp-codebox/phpunit-discovery/v1","plugin_slug":"component","files":[]}'
else
    exit 99
fi
SH
export HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="$work/runtime.sh"
export HOMEBOY_TEST_SHARD_MANIFEST="$work/shard.json"
# No runtime sidecar writer is needed: exercise the post-command parsers that
# consume the combined log, including when backend parsing overwrites a sidecar.
set +e
bash "$SCRIPT_DIR/test-runner.sh" > "$work/output" 2>&1
status=$?
set -e
[ "$status" -eq 255 ] || { printf 'Expected exit 255, got %s\n' "$status"; grep . "$work/output"; exit 1; }
grep -Fq 'PHP_SMOKE_FAIL:tests/failing-smoke.php:exit=255' "$work/output"
grep -Fq 'TEST_SHARD_SUMMARY:id=shard-1 selected=2 routed=2 status=failed' "$work/output"
bash "$SCRIPT_DIR/parse-test-results.sh" "$work/output"
bash "$SCRIPT_DIR/parse-test-failures.sh" "$work/output" "$HOMEBOY_COMPONENT_PATH"
jq -e '.total == 2 and .passed == 1 and .failed == 1 and .skipped == 0' "$work/counts.json" >/dev/null
jq -e '.total == 2 and .passed == 1 and [.failures[].test_id] == ["tests/failing-smoke.php"] and .failures[0].file == "tests/failing-smoke.php"' "$work/failures.json" >/dev/null
# The same failed smoke must not mask a coincident PHPUnit failure (or its
# skipped tests). These are the standard backend output contracts.
cat >> "$work/output" <<'LOG'
There was 1 failure:

1) ExampleTest::testBroken
Failed asserting that false is true.

Tests: 3, Assertions: 2, Failures: 1, Skipped: 1.
LOG
bash "$SCRIPT_DIR/parse-test-results.sh" "$work/output"
bash "$SCRIPT_DIR/parse-test-failures.sh" "$work/output" "$HOMEBOY_COMPONENT_PATH"
jq -e '.total == 5 and .passed == 2 and .failed == 2 and .skipped == 1' "$work/counts.json" >/dev/null
jq -e '[.failures[].test_id] | sort == ["ExampleTest::testBroken", "tests/failing-smoke.php"]' "$work/failures.json" >/dev/null
echo 'Standalone PHP failure evidence smoke passed'
