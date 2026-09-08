#!/usr/bin/env bash
set -euo pipefail

# Extra-Chill/homeboy-extensions#2719 — declared standalone PHP tests are
# canonical inventory members.
#
# The inventory must describe the same default suite normal full-suite
# execution runs: canonical PHPUnit discovery plus the declared standalone PHP
# suite (the union of standalone_php_test_paths and the test manifest's
# standalone-php entries). Covered here:
#
#   * a standalone-only component produces an inventory from empty PHPUnit
#     discovery, and both sources empty is still an error;
#   * a mixed suite emits deterministic per-member identities whose runner
#     environment rides the existing `target` field — `phpunit` versus
#     `standalone-php` — because core validates members structurally and
#     refuses unknown fields, so no new field may carry it;
#   * a file both sources claim is one standalone member, matching the
#     runner's classifier precedence;
#   * a PHPUnit-only inventory is byte-identical to the origin/main producer's
#     output for the same fixture, proving the union changed nothing for
#     components that declare no standalone tests.

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

runner_prelude="${WORKDIR}/runner-prelude.sh"
cat > "$runner_prelude" <<'SH'
homeboy_runner_init() {
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:?HOMEBOY_EXTENSION_PATH is required}"
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:?HOMEBOY_COMPONENT_PATH is required}"
    PLUGIN_PATH="$COMPONENT_PATH"
}
SH

discovery_doc() {
    # $1 = plugin slug, $2 = output file, rest = sandbox-absolute file paths.
    local slug="$1" output="$2"
    shift 2
    local files="[]"
    if [ "$#" -gt 0 ]; then
        files="$(printf '%s\n' "$@" | jq -R . | jq -s .)"
    fi
    jq -cn --arg slug "$slug" --argjson files "$files" \
        '{schema:"wp-codebox/phpunit-discovery/v1",plugin_slug:$slug,phpunit_xml:("/wordpress/wp-content/plugins/" + $slug + "/phpunit.xml.dist"),test_root:("/wordpress/wp-content/plugins/" + $slug + "/tests"),selected_testsuites:[],files:$files}' > "$output"
}

discovery_stub() {
    # $1 = stub path, $2 = discovery file it emits.
    cat > "$1" <<SH
#!/usr/bin/env bash
set -euo pipefail
if [ "\${HOMEBOY_WORDPRESS_PHPUNIT_DISCOVERY_ONLY:-}" = "1" ] || [ -z "\${HOMEBOY_WORDPRESS_PHPUNIT_DISCOVERY_ONLY:-}" ]; then
    cat "$2"
fi
SH
    chmod +x "$1"
}

run_runner_inventory() {
    # $1 = component dir, $2 = output file, $3 = discovery file, $4 = settings JSON.
    local component="$1" output="$2" discovery="$3" settings="$4"
    local stub="${output}.discovery-stub.sh"
    discovery_stub "$stub" "$discovery"
    HOMEBOY_COMPONENT_ID="$(basename "$component")" \
    HOMEBOY_COMPONENT_PATH="$component" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$runner_prelude" \
    HOMEBOY_RUNTIME_TEST_RUNNER_WP_CODEBOX="$stub" \
    HOMEBOY_TEST_INVENTORY_FILE="$output" \
    HOMEBOY_TEST_INVENTORY_ONLY=1 \
    HOMEBOY_SETTINGS_JSON="$settings" \
        bash "$RUNNER" > "${output}.out" 2> "${output}.err" \
        && return 0 || return $?
}

# --- A standalone-only component inventories its declared suite -------------

solo="${WORKDIR}/solo"
mkdir -p "${solo}/scripts" "${solo}/checks"
printf '{}\n' > "${solo}/composer.json"
printf '<?php echo "health";\n' > "${solo}/scripts/health-check.php"
printf '<?php echo "contract";\n' > "${solo}/checks/behavior-spec.php"

solo_discovery="${WORKDIR}/solo-discovery.json"
discovery_doc solo "$solo_discovery"

solo_settings='{"standalone_php_test_paths":["scripts/health-check.php","checks/*.php"]}'
run_runner_inventory "$solo" "${WORKDIR}/solo.json" "$solo_discovery" "$solo_settings" \
    || fail "standalone-only inventory failed: $(cat "${WORKDIR}/solo.json.err")"
cmp -s "${WORKDIR}/solo.json" "${WORKDIR}/solo.json.out" || fail "runner stdout does not exactly match the inventory file"

python3 - "${WORKDIR}/solo.json" <<'PY' || fail "standalone-only inventory assertions failed"
import hashlib, json, sys

doc = json.load(open(sys.argv[1]))
assert doc["schema"] == "homeboy/test-inventory/v1", doc["schema"]
assert doc["runner"] == "wordpress", doc["runner"]
assert set(doc) == {"schema", "runner", "runner_fingerprint", "workspace_fingerprint", "tests", "inventory_fingerprint"}, sorted(doc)

ids = [test["id"] for test in doc["tests"]]
assert ids == ["checks/behavior-spec.php", "scripts/health-check.php"], ids
for test in doc["tests"]:
    assert set(test) == {"id", "package", "target", "target_kind", "name", "expected_outcome"}, test
    assert test["target"] == "standalone-php", test
    assert test["target_kind"] == "test", test
    assert test["expected_outcome"] == "executed", test
    assert test["package"] == "solo", test

record = {k: v for k, v in doc.items() if k != "inventory_fingerprint"}
canonical = json.dumps(record, sort_keys=True, separators=(",", ":"))
assert hashlib.sha256(canonical.encode()).hexdigest() == doc["inventory_fingerprint"]
print("PASS: a standalone-only component inventories its declared suite")
PY

# --- A mixed suite emits deterministic identities for both runners ----------

mixed="${WORKDIR}/mixed"
mkdir -p "${mixed}/tests/Unit" "${mixed}/checks"
printf '{}\n' > "${mixed}/composer.json"
printf '<?php\n' > "${mixed}/tests/Unit/ZetaTest.php"
printf '<?php\n' > "${mixed}/tests/test-eta.php"
printf '<?php echo "spec";\n' > "${mixed}/checks/behavior-spec.php"
cat > "${mixed}/homeboy-test-manifest.json" <<'JSON'
{
    "schema": "homeboy/test-manifest/v1",
    "tests": {
        "checks/behavior-spec.php": {"environment": "standalone-php"}
    }
}
JSON

mixed_discovery="${WORKDIR}/mixed-discovery.json"
discovery_doc mixed "$mixed_discovery" \
    "/wordpress/wp-content/plugins/mixed/tests/Unit/ZetaTest.php" \
    "/wordpress/wp-content/plugins/mixed/tests/test-eta.php"

run_runner_inventory "$mixed" "${WORKDIR}/mixed.json" "$mixed_discovery" '{}' \
    || fail "mixed inventory failed: $(cat "${WORKDIR}/mixed.json.err")"

python3 - "${WORKDIR}/mixed.json" <<'PY' || fail "mixed inventory assertions failed"
import json, sys

doc = json.load(open(sys.argv[1]))
members = [(test["id"], test["target"]) for test in doc["tests"]]
assert members == [
    ("checks/behavior-spec.php", "standalone-php"),
    ("tests/Unit/ZetaTest.php", "phpunit"),
    ("tests/test-eta.php", "phpunit"),
], members
print("PASS: a mixed suite emits deterministic identities for both runners")
PY

# Determinism across runs, including the standalone members.
run_runner_inventory "$mixed" "${WORKDIR}/mixed-again.json" "$mixed_discovery" '{}' \
    || fail "second mixed inventory failed"
cmp -s "${WORKDIR}/mixed.json" "${WORKDIR}/mixed-again.json" || fail "mixed producer is not deterministic"
printf 'PASS: mixed inventories are deterministic across runs\n'

# --- A file both sources claim is one standalone member ----------------------

both_discovery="${WORKDIR}/both-discovery.json"
discovery_doc mixed "$both_discovery" \
    "/wordpress/wp-content/plugins/mixed/checks/behavior-spec.php" \
    "/wordpress/wp-content/plugins/mixed/tests/Unit/ZetaTest.php"

run_runner_inventory "$mixed" "${WORKDIR}/both.json" "$both_discovery" '{}' \
    || fail "overlapping-declaration inventory failed: $(cat "${WORKDIR}/both.json.err")"

python3 - "${WORKDIR}/both.json" <<'PY' || fail "overlapping-declaration assertions failed"
import json, sys

doc = json.load(open(sys.argv[1]))
members = [(test["id"], test["target"]) for test in doc["tests"]]
assert members == [
    ("checks/behavior-spec.php", "standalone-php"),
    ("tests/Unit/ZetaTest.php", "phpunit"),
], members
print("PASS: a file both sources claim is a single standalone member")
PY

# --- Both sources empty remains an error -------------------------------------

empty="${WORKDIR}/empty"
mkdir -p "${empty}/tests"
printf '{}\n' > "${empty}/composer.json"
printf '<?php\n' > "${empty}/tests/support-file.php"
empty_discovery="${WORKDIR}/empty-discovery.json"
discovery_doc empty "$empty_discovery"

if python3 "$PRODUCER" --project "$empty" --extension-path "$EXTENSION_PATH" \
    --runner wordpress --discovery-file "$empty_discovery" \
    --output "${WORKDIR}/empty.json" >/dev/null 2>&1; then
    fail "producer accepted empty discovery with no standalone declarations"
fi
printf 'PASS: empty discovery with no standalone declarations is refused\n'

# An empty standalone list — the file the runner writes for an empty union —
# is treated exactly like an omitted list.
: > "${WORKDIR}/empty-list.txt"
if python3 "$PRODUCER" --project "$empty" --extension-path "$EXTENSION_PATH" \
    --runner wordpress --discovery-file "$empty_discovery" \
    --standalone-list "${WORKDIR}/empty-list.txt" \
    --output "${WORKDIR}/empty2.json" >/dev/null 2>&1; then
    fail "producer accepted empty discovery with an empty standalone list"
fi
printf 'PASS: empty discovery with an empty standalone list is refused\n'

# A declared path that is not on the host is a broken producer input, not a
# silent absence: replay would fail to resolve it later.
printf 'tests/missing-spec.php\n' > "${WORKDIR}/missing-list.txt"
if python3 "$PRODUCER" --project "$empty" --extension-path "$EXTENSION_PATH" \
    --runner wordpress --discovery-file "$empty_discovery" \
    --standalone-list "${WORKDIR}/missing-list.txt" \
    --output "${WORKDIR}/empty3.json" >/dev/null 2>&1; then
    fail "producer accepted a standalone declaration for a missing file"
fi
printf 'PASS: a standalone declaration for a missing file is refused\n'

# --- PHPUnit-only inventories are byte-identical to origin/main --------------

classic="${WORKDIR}/classic"
mkdir -p "${classic}/tests"
printf '{}\n' > "${classic}/composer.json"
printf '<?php\n' > "${classic}/tests/ZetaTest.php"
printf '<?php\n' > "${classic}/tests/test-eta.php"
classic_discovery="${WORKDIR}/classic-discovery.json"
discovery_doc classic "$classic_discovery" \
    "/wordpress/wp-content/plugins/classic/tests/ZetaTest.php" \
    "/wordpress/wp-content/plugins/classic/tests/test-eta.php"

python3 "$PRODUCER" --project "$classic" --extension-path "$EXTENSION_PATH" \
    --runner wordpress --package classic \
    --discovery-file "$classic_discovery" \
    --output "${WORKDIR}/classic-new.json" >/dev/null

main_producer="${WORKDIR}/main-test-inventory.py"
if git -C "${EXTENSION_PATH}/.." show origin/main:wordpress/scripts/test/test-inventory.py > "$main_producer" 2>/dev/null; then
    python3 "$main_producer" --project "$classic" --extension-path "$EXTENSION_PATH" \
        --runner wordpress --package classic \
        --discovery-file "$classic_discovery" \
        --output "${WORKDIR}/classic-main.json" >/dev/null
    cmp -s "${WORKDIR}/classic-main.json" "${WORKDIR}/classic-new.json" \
        || fail "PHPUnit-only inventory is not byte-identical to the origin/main producer's output"
    printf 'PASS: PHPUnit-only inventories are byte-identical to origin/main\n'
else
    # Shallow checkout without origin/main: assert the structural form of the
    # same contract — no new fields anywhere, exact member key sets, and the
    # canonical fingerprint core re-derives.
    python3 - "${WORKDIR}/classic-new.json" <<'PY' || fail "PHPUnit-only structural identity assertions failed"
import hashlib, json, sys

doc = json.load(open(sys.argv[1]))
assert set(doc) == {"schema", "runner", "runner_fingerprint", "workspace_fingerprint", "tests", "inventory_fingerprint"}, sorted(doc)
for test in doc["tests"]:
    assert set(test) == {"id", "package", "target", "target_kind", "name", "expected_outcome"}, test
    assert test["target"] == "phpunit", test
record = {k: v for k, v in doc.items() if k != "inventory_fingerprint"}
canonical = json.dumps(record, sort_keys=True, separators=(",", ":"))
assert hashlib.sha256(canonical.encode()).hexdigest() == doc["inventory_fingerprint"]
print("PASS: PHPUnit-only inventories keep the exact structural contract")
PY
    printf 'NOTE: origin/main producer unavailable; structural identity asserted instead of a byte diff\n'
fi

printf 'All WordPress standalone inventory checks passed.\n'
