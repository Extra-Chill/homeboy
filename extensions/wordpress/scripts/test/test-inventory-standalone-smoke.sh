#!/usr/bin/env bash
set -euo pipefail

# Extra-Chill/homeboy-extensions#2719 — declared standalone PHP tests are
# canonical inventory members.
#
# The inventory must describe the same default suite normal full-suite
# execution runs: canonical PHPUnit discovery plus the standalone PHP selection
# the shared test_path_resolver computes (convention default
# tests/**/*-smoke.php unless standalone_php_test_paths replaces it, minus
# wordpress_test_paths and test_path_excludes). Covered here:
#
#   * a standalone-only component produces an inventory from empty PHPUnit
#     discovery, and both sources empty is still an error;
#   * with no settings, nested tests/**/*-smoke.php files are discovered, a new
#     file is picked up without a settings edit, excludes remove matches, and an
#     explicit [] selects none (#2967);
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
# The standalone member is declared by exact path in homeboy.json settings, and
# a wordpress_test_paths match never joins the standalone inventory.

mixed="${WORKDIR}/mixed"
mkdir -p "${mixed}/tests/Unit" "${mixed}/checks"
printf '{}\n' > "${mixed}/composer.json"
printf '<?php\n' > "${mixed}/tests/Unit/ZetaTest.php"
printf '<?php\n' > "${mixed}/tests/test-eta.php"
printf '<?php echo "spec";\n' > "${mixed}/checks/behavior-spec.php"
printf '<?php echo "booted";\n' > "${mixed}/checks/booted-smoke.php"
mixed_settings='{"standalone_php_test_paths":["checks/behavior-spec.php","checks/booted-smoke.php"],"wordpress_test_paths":["checks/booted-smoke.php"]}'

mixed_discovery="${WORKDIR}/mixed-discovery.json"
discovery_doc mixed "$mixed_discovery" \
    "/wordpress/wp-content/plugins/mixed/tests/Unit/ZetaTest.php" \
    "/wordpress/wp-content/plugins/mixed/tests/test-eta.php"

run_runner_inventory "$mixed" "${WORKDIR}/mixed.json" "$mixed_discovery" "$mixed_settings" \
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
run_runner_inventory "$mixed" "${WORKDIR}/mixed-again.json" "$mixed_discovery" "$mixed_settings" \
    || fail "second mixed inventory failed"
cmp -s "${WORKDIR}/mixed.json" "${WORKDIR}/mixed-again.json" || fail "mixed producer is not deterministic"
printf 'PASS: mixed inventories are deterministic across runs\n'

# --- A file both sources claim is one standalone member ----------------------

both_discovery="${WORKDIR}/both-discovery.json"
discovery_doc mixed "$both_discovery" \
    "/wordpress/wp-content/plugins/mixed/checks/behavior-spec.php" \
    "/wordpress/wp-content/plugins/mixed/tests/Unit/ZetaTest.php"

run_runner_inventory "$mixed" "${WORKDIR}/both.json" "$both_discovery" "$mixed_settings" \
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

# An explicit empty standalone_php_test_paths replaces the convention default:
# a conventional smoke on disk is not selected, so the enumeration is empty.
printf '<?php\n' > "${empty}/tests/ignored-smoke.php"
if HOMEBOY_SETTINGS_JSON='{"standalone_php_test_paths":[]}' python3 "$PRODUCER" \
    --project "$empty" --extension-path "$EXTENSION_PATH" \
    --runner wordpress --discovery-file "$empty_discovery" \
    --output "${WORKDIR}/empty2.json" >/dev/null 2>&1; then
    fail "producer selected standalone smokes despite an explicit empty standalone_php_test_paths"
fi
printf 'PASS: an explicit empty standalone_php_test_paths selects no standalone tests\n'

# Invalid settings fail closed rather than silently dropping tests.
if HOMEBOY_SETTINGS_JSON='{"test_path_excludes":"tests/x.php"}' python3 "$PRODUCER" \
    --project "$empty" --extension-path "$EXTENSION_PATH" \
    --runner wordpress --discovery-file "$empty_discovery" \
    --output "${WORKDIR}/empty3.json" >/dev/null 2>"${WORKDIR}/empty3.err"; then
    fail "producer accepted a non-array test_path_excludes"
fi
grep -Fq "test_path_excludes must be an array of strings." "${WORKDIR}/empty3.err" \
    || fail "invalid test_path_excludes did not name the setting"
printf 'PASS: invalid test-path settings are refused\n'

# --- Convention-first discovery needs no settings (#2967) --------------------

conventional="${WORKDIR}/conventional"
mkdir -p "${conventional}/tests/Unit" "${conventional}/tests/x/y" "${conventional}/tests/fixtures"
printf '{}\n' > "${conventional}/composer.json"
printf '<?php\n' > "${conventional}/tests/Unit/AlphaTest.php"
printf '<?php echo "a";\n' > "${conventional}/tests/a-smoke.php"
printf '<?php echo "b";\n' > "${conventional}/tests/x/y/b-smoke.php"
printf '<?php echo "fixture";\n' > "${conventional}/tests/fixtures/fixture-smoke.php"
printf '<?php\n' > "${conventional}/tests/helper.php"
conventional_discovery="${WORKDIR}/conventional-discovery.json"
discovery_doc conventional "$conventional_discovery" \
    "/wordpress/wp-content/plugins/conventional/tests/Unit/AlphaTest.php"

assert_members() {
    # $1 = inventory file, $2 = expected "id=target" lines (sorted).
    python3 - "$1" "$2" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
members = "\n".join(f'{test["id"]}={test["target"]}' for test in doc["tests"])
raise SystemExit(0 if members == sys.argv[2] else f"unexpected members:\n{members}")
PY
}

run_runner_inventory "$conventional" "${WORKDIR}/conventional.json" "$conventional_discovery" '{}' \
    || fail "conventional inventory failed: $(cat "${WORKDIR}/conventional.json.err")"
assert_members "${WORKDIR}/conventional.json" $'tests/Unit/AlphaTest.php=phpunit\ntests/a-smoke.php=standalone-php\ntests/fixtures/fixture-smoke.php=standalone-php\ntests/x/y/b-smoke.php=standalone-php' \
    || fail "default discovery did not select nested tests/**/*-smoke.php"
printf 'PASS: default discovery selects nested tests/**/*-smoke.php with no settings\n'

excludes_settings='{"test_path_excludes":["tests/fixtures/**"]}'
run_runner_inventory "$conventional" "${WORKDIR}/excludes.json" "$conventional_discovery" "$excludes_settings" \
    || fail "excludes inventory failed: $(cat "${WORKDIR}/excludes.json.err")"
assert_members "${WORKDIR}/excludes.json" $'tests/Unit/AlphaTest.php=phpunit\ntests/a-smoke.php=standalone-php\ntests/x/y/b-smoke.php=standalone-php' \
    || fail "test_path_excludes did not remove the fixture"
printf 'PASS: test_path_excludes removes matching fixtures\n'

overlap_settings='{"wordpress_test_paths":["tests/x/**/*.php"],"test_path_excludes":["tests/fixtures/**"]}'
run_runner_inventory "$conventional" "${WORKDIR}/overlap.json" "$conventional_discovery" "$overlap_settings" \
    || fail "overlap inventory failed: $(cat "${WORKDIR}/overlap.json.err")"
assert_members "${WORKDIR}/overlap.json" $'tests/Unit/AlphaTest.php=phpunit\ntests/a-smoke.php=standalone-php' \
    || fail "a wordpress_test_paths match joined the standalone inventory"
printf 'PASS: a file matched by both lists is not a standalone member\n'

explicit_settings='{"standalone_php_test_paths":["tests/x/**/*-smoke.php"]}'
run_runner_inventory "$conventional" "${WORKDIR}/explicit.json" "$conventional_discovery" "$explicit_settings" \
    || fail "explicit inventory failed: $(cat "${WORKDIR}/explicit.json.err")"
assert_members "${WORKDIR}/explicit.json" $'tests/Unit/AlphaTest.php=phpunit\ntests/x/y/b-smoke.php=standalone-php' \
    || fail "an explicit standalone_php_test_paths did not replace the default"
printf 'PASS: an explicit standalone_php_test_paths replaces the default\n'

mkdir -p "${conventional}/tests/added"
printf '<?php echo "new";\n' > "${conventional}/tests/added/new-smoke.php"
run_runner_inventory "$conventional" "${WORKDIR}/added.json" "$conventional_discovery" "$excludes_settings" \
    || fail "added-file inventory failed: $(cat "${WORKDIR}/added.json.err")"
assert_members "${WORKDIR}/added.json" $'tests/Unit/AlphaTest.php=phpunit\ntests/a-smoke.php=standalone-php\ntests/added/new-smoke.php=standalone-php\ntests/x/y/b-smoke.php=standalone-php' \
    || fail "a new smoke under tests/ was not picked up without a settings edit"
printf 'PASS: a new smoke under tests/ is picked up without a settings edit\n'

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
