#!/usr/bin/env bash
# Prove the installed extension layout can load shared agent-runtime files.
#
# `agent-runtimes` is a shared asset: Homeboy installs it at
# <homeboy>/agent-runtimes, a sibling of <homeboy>/extensions, while a monorepo
# checkout keeps it one level closer to the extension. A linked dev install
# hides the difference because Node resolves symlinked modules back to the
# checkout, so this fixture COPIES both the extension and shared runtimes. That
# is the shape a fresh CI runner has, including the shim failure from #2690.
set -euo pipefail

WORDPRESS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPOSITORY_ROOT="$(cd "${WORDPRESS_ROOT}/.." && pwd)"
FIXTURE_ROOT="$(mktemp -d)"
FIXTURE_ROOT="$(cd "${FIXTURE_ROOT}" && pwd -P)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

HOMEBOY_ROOT="${FIXTURE_ROOT}/config/homeboy-2508-snapshot"
EXTENSION_DIR="${HOMEBOY_ROOT}/extensions/wordpress"
SHARED_LIB_DIR="${HOMEBOY_ROOT}/extensions/scripts/lib"
AGENT_RUNTIMES_DIR="${HOMEBOY_ROOT}/agent-runtimes"
HASHED_SOURCE_DIR="${FIXTURE_ROOT}/source-snapshots/2508-a1b2c3/wordpress"
mkdir -p "${EXTENSION_DIR}" "${SHARED_LIB_DIR}"
cp -R "${WORDPRESS_ROOT}/scripts" "${EXTENSION_DIR}/scripts"
cp -R "${WORDPRESS_ROOT}/lib" "${EXTENSION_DIR}/lib"
cp "${WORDPRESS_ROOT}/wordpress.json" "${EXTENSION_DIR}/wordpress.json"
cp -R "${REPOSITORY_ROOT}/scripts/lib/." "${SHARED_LIB_DIR}/"
cp -R "${REPOSITORY_ROOT}/agent-runtimes" "${AGENT_RUNTIMES_DIR}"
cp -R "${REPOSITORY_ROOT}/agent-task-contracts" "${HOMEBOY_ROOT}/agent-task-contracts"
cp -R "${REPOSITORY_ROOT}/dependency-adapters" "${HOMEBOY_ROOT}/dependency-adapters"
cp -R "${REPOSITORY_ROOT}/runtime-agent-ci" "${HOMEBOY_ROOT}/runtime-agent-ci"
mkdir -p "${HASHED_SOURCE_DIR}"
cp -R "${WORDPRESS_ROOT}/scripts" "${HASHED_SOURCE_DIR}/scripts"
cp -R "${WORDPRESS_ROOT}/lib" "${HASHED_SOURCE_DIR}/lib"
cp "${WORDPRESS_ROOT}/wordpress.json" "${HASHED_SOURCE_DIR}/wordpress.json"

# Every step below asserts its own outcome, including exit status. Leaving
# errexit on would abort on a probe that is *expected* to fail and discard the
# captured output with it, reporting the failure as a silent exit 1.
set +e

fail() {
    printf '%s\n' "$1" >&2
    exit 1
}

# HOMEBOY_EXTENSION_PATH is an accepted resolver input, so an ambient value
# would let these probes resolve against the caller's extension instead of the
# fixture. Unset it wherever the point is what a script's own location finds.
fixture_node() {
    env -u HOMEBOY_EXTENSION_PATH -u HOMEBOY_AGENT_RUNTIMES_DIR node "$@"
}

# Resolution is the invariant under test, so assert the resolved paths directly
# instead of inferring them from a runtime's exit code. The shared runtimes
# themselves depend on ambient tooling (the opencode executor spawns `homeboy`
# for contract constants), which is not what an installed-layout packaging test
# should be gated on.
for target in \
    "wp-codebox/lib/runtime-setup.cjs" \
    "opencode/scripts/agent/homeboy-opencode-agent-task-executor.cjs"; do
    resolved="$(fixture_node "${EXTENSION_DIR}/scripts/lib/agent-runtime-paths.cjs" "${target}" 2>&1)"
    expected="$(cd "$(dirname "${HOMEBOY_ROOT}")" && pwd -P)/$(basename "${HOMEBOY_ROOT}")/agent-runtimes/${target}"
    [ "$resolved" = "$expected" ] || fail "Expected installed resolution of ${target} to ${expected}, got: ${resolved}"
done

# Resolving a filename is insufficient: execute the copied runtime-selection
# module so its dependency on the copied WordPress extension is proven after
# installation in the sibling layout that failed before test inventory started
# in #2690 and must remain the runtime path after the setup bootstrap fix in
# #2700. Runtime selection is WordPress-owned, so it loads from the installed
# extension payload rather than the shared agent-runtime tree.
selection_module="${EXTENSION_DIR}/lib/wp-codebox-runtime-selection.js"
selection_output="$(fixture_node - "${selection_module}" <<'NODE' 2>&1
const selection = require(process.argv[2]);
if (typeof selection.preflightWpCodeboxCommand !== 'function') process.exit(1);
process.stdout.write(selection.REQUIRED_WP_CODEBOX_VERSION);
NODE
)"
expected_version="$(fixture_node -p "require(process.argv[1]).wp_codebox.minimum_version" "${EXTENSION_DIR}/wordpress.json" 2>&1)"
[ "${selection_output}" = "${expected_version}" ] || fail "Installed WP Codebox runtime-selection shim failed to load: ${selection_output}"

# The opencode wrapper's only job is to resolve and require the shared runtime,
# so assert that it gets past resolution. What the runtime then does with an
# empty request is out of scope here.
executor_output="$(fixture_node "${EXTENSION_DIR}/scripts/agent/homeboy-opencode-agent-task-executor.cjs" </dev/null 2>&1)"
case "$executor_output" in
    *"Cannot find module"*|*"could not resolve shared agent runtime file"*)
        fail "Installed opencode executor wrapper failed to load the shared runtime: ${executor_output}"
        ;;
esac

# The PHPUnit adapter resolves its runtime-selection module before it validates
# its own environment, so reaching the HOMEBOY_COMPONENT_PATH error is proof the
# require succeeded.
adapter_output="$(HOMEBOY_SETTINGS_JSON='{}' fixture_node "${EXTENSION_DIR}/scripts/test/wp-codebox-phpunit-adapter.mjs" 2>&1)"
case "$adapter_output" in
    *"Cannot find module"*) fail "Installed PHPUnit adapter failed to resolve a module: ${adapter_output}" ;;
esac
case "$adapter_output" in
    *HOMEBOY_COMPONENT_PATH*) ;;
    *) fail "Expected installed PHPUnit adapter to reach its environment validation, got: ${adapter_output}" ;;
esac

# The parser source is a copied hashed snapshot, outside the active config tree.
# Core binds its separately materialized shared assets into that source runtime.
# Without those bindings, wp-codebox-json silently degrades and reports no counts.
mkdir -p "${FIXTURE_ROOT}/artifacts/files"
cat > "${FIXTURE_ROOT}/artifacts/files/test-results.json" <<'JSON'
{"schema":"wp-codebox/test-results/v1","summary":{"total":3,"passed":2,"failed":1,"skipped":0}}
JSON
cat > "${FIXTURE_ROOT}/write-test-results.sh" <<'EOF'
homeboy_write_test_results() {
    printf 'total=%s passed=%s failed=%s skipped=%s\n' "$1" "$2" "$3" "$4"
}
EOF
parse_output="$(HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${FIXTURE_ROOT}/write-test-results.sh" \
    HOMEBOY_EXTENSION_PATH="${HASHED_SOURCE_DIR}" \
    HOMEBOY_SHARED_LIB_DIR="${SHARED_LIB_DIR}" \
    HOMEBOY_AGENT_RUNTIMES_DIR="${AGENT_RUNTIMES_DIR}" \
    bash "${HASHED_SOURCE_DIR}/scripts/test/parse-test-results.sh" \
        "${FIXTURE_ROOT}/artifacts" wp-codebox-json 2>&1)"
case "$parse_output" in
    *"total=3 passed=2 failed=1 skipped=0"*) ;;
    *) fail "Expected hashed-source result parser to report WP Codebox counts, got: ${parse_output}" ;;
esac

# An explicit core-supplied generic adapter is sufficient by itself. In
# particular, the parser must not try to discover a checkout-adjacent shared
# library before honoring this binding.
printf '%s\n' 'OK (2 tests, 2 assertions)' > "${FIXTURE_ROOT}/phpunit-output.txt"
explicit_adapter_output="$(HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${FIXTURE_ROOT}/write-test-results.sh" \
    HOMEBOY_RUNTIME_TEST_RESULT_ADAPTERS="${SHARED_LIB_DIR}/test-result-adapters.sh" \
    HOMEBOY_SHARED_LIB_DIR="${FIXTURE_ROOT}/missing-shared-lib" \
    bash "${HASHED_SOURCE_DIR}/scripts/test/parse-test-results.sh" \
        "${FIXTURE_ROOT}/phpunit-output.txt" phpunit 2>&1)"
case "$explicit_adapter_output" in
    *"total=2 passed=2 failed=0 skipped=0"*) ;;
    *) fail "Expected explicit generic result adapter to bypass shared library discovery, got: ${explicit_adapter_output}" ;;
esac

# A core binding can include unrelated agent runtimes without making generic
# PHPUnit parsing depend on the optional WP Codebox runtime.
UNRELATED_RUNTIMES_DIR="${FIXTURE_ROOT}/unrelated-agent-runtimes"
mkdir -p "${UNRELATED_RUNTIMES_DIR}/opencode"
generic_bound_output="$(HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${FIXTURE_ROOT}/write-test-results.sh" \
    HOMEBOY_SHARED_LIB_DIR="${SHARED_LIB_DIR}" \
    HOMEBOY_AGENT_RUNTIMES_DIR="${UNRELATED_RUNTIMES_DIR}" \
    bash "${HASHED_SOURCE_DIR}/scripts/test/parse-test-results.sh" \
        "${FIXTURE_ROOT}/phpunit-output.txt" phpunit 2>&1)"
case "$generic_bound_output" in
    *"total=2 passed=2 failed=0 skipped=0"*) ;;
    *) fail "Expected generic parser to ignore a bound runtime root without WP Codebox, got: ${generic_bound_output}" ;;
esac
missing_wp_codebox_output="$(HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${FIXTURE_ROOT}/write-test-results.sh" \
    HOMEBOY_SHARED_LIB_DIR="${SHARED_LIB_DIR}" \
    HOMEBOY_AGENT_RUNTIMES_DIR="${UNRELATED_RUNTIMES_DIR}" \
    bash "${HASHED_SOURCE_DIR}/scripts/test/parse-test-results.sh" \
        "${FIXTURE_ROOT}/artifacts" wp-codebox-json 2>&1)"
missing_wp_codebox_status=$?
[ "$missing_wp_codebox_status" -ne 0 ] || fail "Expected requested WP Codebox adapter to reject a runtime root without WP Codebox"
case "$missing_wp_codebox_output" in
    *"HOMEBOY_AGENT_RUNTIMES_DIR is explicitly bound"*"wp-codebox/scripts/lib/test-result-adapters.sh"*) ;;
    *) fail "Expected a clear missing WP Codebox adapter diagnostic, got: ${missing_wp_codebox_output}" ;;
esac

# The copied failure parser receives the same artifact directory and must retain
# the WP Codebox failure identity instead of treating the installed snapshot as
# an unparseable zero-result run.
python3 - "${FIXTURE_ROOT}/artifacts/files/test-results.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
payload["suites"] = [{
    "name": "phpunit",
    "failures": [{
        "test_id": "Example\\ParserTest::test_failure_identity",
        "message": "Expected parser identity",
        "failure_type": "AssertionFailedError",
        "file": "/tmp/component/tests/ParserTest.php",
        "line": 42,
    }],
}]
with open(sys.argv[1], "w", encoding="utf-8") as handle:
    json.dump(payload, handle)
PY
FAILURES_FILE="${FIXTURE_ROOT}/failures.json"
HOMEBOY_TEST_FAILURES_FILE="${FAILURES_FILE}" \
    bash "${HASHED_SOURCE_DIR}/scripts/test/parse-test-failures.sh" "${FIXTURE_ROOT}/artifacts" "/tmp/component"
python3 - "${FAILURES_FILE}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    payload = json.load(handle)
assert payload["total"] == 3 and payload["passed"] == 2, payload
assert payload["failures"][0]["test_id"] == "Example\\ParserTest::test_failure_identity", payload
PY

# WordPress-owned runtime selection remains available without the experimental
# shared runtime. Wrappers that still consume shared runtimes must name what is
# missing instead of emitting a bare MODULE_NOT_FOUND stack.
rm -rf "${HOMEBOY_ROOT}/agent-runtimes"
adapter_output="$(HOMEBOY_SETTINGS_JSON='{}' fixture_node "${EXTENSION_DIR}/scripts/test/wp-codebox-phpunit-adapter.mjs" 2>&1)"
case "$adapter_output" in
    *HOMEBOY_COMPONENT_PATH*) ;;
    *) fail "Expected installed PHPUnit adapter to remain independent of shared runtimes, got: ${adapter_output}" ;;
esac

missing_output="$(fixture_node "${EXTENSION_DIR}/scripts/agent/homeboy-opencode-agent-task-executor.cjs" </dev/null 2>&1)"
missing_status=$?
[ "$missing_status" -ne 0 ] || fail "Expected missing shared runtime to fail the opencode wrapper"
case "$missing_output" in
    *"could not resolve shared agent runtime file"*"${HOMEBOY_ROOT}/agent-runtimes"*"homeboy extension install wordpress"*) ;;
    *) fail "Expected an actionable missing-runtime diagnostic, got: ${missing_output}" ;;
esac

printf '%s\n' 'installed agent runtime resolution smoke passed'
