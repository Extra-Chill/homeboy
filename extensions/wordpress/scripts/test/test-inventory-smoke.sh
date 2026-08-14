#!/usr/bin/env bash
set -euo pipefail

# Extra-Chill/homeboy#12394 — WordPress test inventory producer.
#
# Sharded execution needs the suite enumerated without running it. Homeboy core
# re-derives both fingerprints itself and refuses anything that does not match,
# so the producer's only job is to agree with core exactly. Two properties carry
# that agreement and are asserted here:
#
#   * the workspace fingerprint responds to declared files and ignores
#     everything else, including skipped directories;
#   * `inventory_fingerprint` is the sha256 of
#     `json.dumps(record_without_that_key, sort_keys=True, separators=(",",":"))`,
#     which is the byte layout core reproduces by hand.
#
# Enumeration must also match what the runner would actually execute. A file the
# runner cannot route must not appear, or a shard would claim a test that never
# runs and the aggregate totals would not reconcile.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
PRODUCER="${SCRIPT_DIR}/test-inventory.py"
RUNNER="${SCRIPT_DIR}/test-runner.sh"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    exit 1
}

plugin="${WORKDIR}/plugin"
mkdir -p "${plugin}/tests/nested" "${plugin}/inc" "${plugin}/vendor/pkg/tests" "${plugin}/node_modules/x"
printf '{}\n' > "${plugin}/composer.json"
printf '<?php\n' > "${plugin}/inc/Runtime.php"

runner_prelude="${WORKDIR}/runner-prelude.sh"
cat > "$runner_prelude" <<'SH'
homeboy_runner_init() {
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:?HOMEBOY_EXTENSION_PATH is required}"
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
    PLUGIN_PATH="$COMPONENT_PATH"
}
SH

# Routable by the runner: host PHP smoke, JS smoke, shell smoke, node test, PHPUnit.
printf '<?php\n' > "${plugin}/tests/alpha-smoke.php"
printf '<?php\n' > "${plugin}/tests/nested/beta-smoke.php"
printf '// js\n'  > "${plugin}/tests/gamma-smoke.js"
printf '# sh\n'   > "${plugin}/tests/delta-smoke.sh"
printf '// node\n' > "${plugin}/tests/epsilon.test.js"
printf '<?php\n'  > "${plugin}/tests/ZetaTest.php"
printf '<?php\n'  > "${plugin}/tests/test-eta.php"

# Not routable, and must not be enumerated: a fixture, a support file, and
# anything inside a skipped directory.
printf '<?php\n' > "${plugin}/tests/fixture-data.php"
printf '<?php\n' > "${plugin}/vendor/pkg/tests/vendor-smoke.php"
printf '// dep\n' > "${plugin}/node_modules/x/dep-smoke.js"

run_producer() {
    HOMEBOY_COMPONENT_ID=fixture-plugin \
    HOMEBOY_COMPONENT_PATH="$plugin" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
    HOMEBOY_TEST_INVENTORY_FILE="$1" \
    HOMEBOY_TEST_INVENTORY_ONLY=1 \
        bash "$RUNNER"
}

run_producer "${WORKDIR}/inventory.json" > "${WORKDIR}/inventory.out" || fail "runner exited non-zero"
inventory="${WORKDIR}/inventory.json"
cmp -s "$inventory" "${WORKDIR}/inventory.out" || fail "runner stdout does not exactly match the inventory file"

python3 - "$inventory" <<'PY' || fail "inventory contract assertions failed"
import hashlib, json, sys

doc = json.load(open(sys.argv[1]))

assert doc["schema"] == "homeboy/test-inventory/v1", doc["schema"]
assert doc["runner"] == "wordpress", doc["runner"]
for key in ("runner_fingerprint", "workspace_fingerprint", "inventory_fingerprint"):
    value = doc[key]
    assert len(value) == 64 and value == value.lower(), f"{key}={value!r}"
    int(value, 16)

# Core rebuilds the canonical form without `inventory_fingerprint` and with no
# `fallback_reason`, so the producer must not emit one.
assert "fallback_reason" not in doc, "producer must not emit fallback_reason"

ids = [t["id"] for t in doc["tests"]]
assert len(ids) == len(set(ids)), "test ids must be unique"

expected = {
    "tests/alpha-smoke.php": ("smoke", "host-php-smoke"),
    "tests/nested/beta-smoke.php": ("smoke", "host-php-smoke"),
    "tests/gamma-smoke.js": ("smoke", "host-js-smoke"),
    "tests/delta-smoke.sh": ("smoke", "host-shell-smoke"),
    "tests/epsilon.test.js": ("test", "node-test"),
    "tests/ZetaTest.php": ("test", "phpunit"),
    "tests/test-eta.php": ("test", "phpunit"),
}
assert set(ids) == set(expected), f"enumerated {sorted(ids)}"

by_id = {t["id"]: t for t in doc["tests"]}
for test_id, (kind, target) in expected.items():
    entry = by_id[test_id]
    assert entry["target_kind"] == kind, (test_id, entry)
    assert entry["target"] == target, (test_id, entry)
    assert entry["expected_outcome"] == "executed", entry
    assert entry["package"] == "fixture-plugin", entry
    assert entry["name"] and entry["name"] == test_id.rsplit("/", 1)[-1], entry

# The fingerprint identity core re-derives.
record = {k: v for k, v in doc.items() if k != "inventory_fingerprint"}
canonical = json.dumps(record, sort_keys=True, separators=(",", ":"))
assert hashlib.sha256(canonical.encode()).hexdigest() == doc["inventory_fingerprint"], \
    "inventory_fingerprint is not the canonical digest core will recompute"

print(f"PASS: enumerated {len(ids)} routable tests, fingerprints are canonical")
PY

# Determinism: identical input must produce an identical document, or a shard
# plan cannot be replayed.
run_producer "${WORKDIR}/again.json" > "${WORKDIR}/again.out" || fail "second runner run exited non-zero"
cmp -s "$inventory" "${WORKDIR}/again.json" || fail "producer is not deterministic"
cmp -s "${WORKDIR}/again.json" "${WORKDIR}/again.out" || fail "second runner stdout does not exactly match the inventory file"
printf 'PASS: producer is deterministic across runs\n'

before_ws="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["workspace_fingerprint"])' "$inventory")"

# A skipped directory must not move the workspace fingerprint.
printf '<?php\n// noise\n' > "${plugin}/vendor/pkg/Ignored.php"
run_producer "${WORKDIR}/skipped.json" > /dev/null || fail "producer failed after vendor write"
after_skip="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["workspace_fingerprint"])' "${WORKDIR}/skipped.json")"
[ "$before_ws" = "$after_skip" ] || fail "vendor/ is skipped but changed the workspace fingerprint"
printf 'PASS: skipped directories do not move the workspace fingerprint\n'

# An undeclared extension must not move it either.
printf 'notes\n' > "${plugin}/inc/notes.md"
run_producer "${WORKDIR}/md.json" > /dev/null || fail "producer failed after markdown write"
after_md="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["workspace_fingerprint"])' "${WORKDIR}/md.json")"
[ "$before_ws" = "$after_md" ] || fail "markdown is undeclared but changed the workspace fingerprint"
printf 'PASS: undeclared extensions do not move the workspace fingerprint\n'

# A declared source file must move it, or the fingerprint proves nothing.
printf '<?php\n// changed\n' > "${plugin}/inc/Runtime.php"
run_producer "${WORKDIR}/changed.json" > /dev/null || fail "producer failed after php edit"
after_php="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["workspace_fingerprint"])' "${WORKDIR}/changed.json")"
[ "$before_ws" != "$after_php" ] || fail "a declared PHP source edit did not move the workspace fingerprint"
printf 'PASS: declared source edits move the workspace fingerprint\n'

# An empty enumeration is refused: it cannot be told apart from a broken
# producer, and sharding nothing would report a green suite that ran no tests.
empty="${WORKDIR}/empty"
mkdir -p "${empty}"
printf '{}\n' > "${empty}/composer.json"
if python3 "$PRODUCER" --project "$empty" --extension-path "$EXTENSION_PATH" \
    --runner wordpress --output "${WORKDIR}/empty.json" >/dev/null 2>&1; then
    fail "producer accepted a component with no tests"
fi
printf 'PASS: an empty enumeration is refused\n'

# An undeclared runner has no version command and therefore no identity.
if python3 "$PRODUCER" --project "$plugin" --extension-path "$EXTENSION_PATH" \
    --runner not-declared --output "${WORKDIR}/bad-runner.json" >/dev/null 2>&1; then
    fail "producer accepted an undeclared runner"
fi
printf 'PASS: an undeclared runner is refused\n'

# The runner must emit its document only after the producer has succeeded. A
# failing producer leaves stdout empty and does not replace the requested file.
printf 'existing inventory\n' > "${WORKDIR}/failed.json"
if HOMEBOY_COMPONENT_ID=fixture-plugin \
    HOMEBOY_COMPONENT_PATH="$plugin" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
    HOMEBOY_TEST_INVENTORY_FILE="${WORKDIR}/failed.json" \
    HOMEBOY_TEST_INVENTORY_ONLY=1 \
    HOMEBOY_WORDPRESS_INVENTORY_RUNNER=not-declared \
        bash "$RUNNER" > "${WORKDIR}/failed.out" 2> "${WORKDIR}/failed.err"; then
    fail "runner accepted an undeclared inventory runner"
fi
[ ! -s "${WORKDIR}/failed.out" ] || fail "failing runner emitted partial inventory JSON"
[ "$(<"${WORKDIR}/failed.json")" = "existing inventory" ] || fail "failing runner replaced the requested inventory file"
printf 'PASS: runner failure emits no inventory JSON and preserves the requested file\n'

printf 'All WordPress test inventory checks passed.\n'
