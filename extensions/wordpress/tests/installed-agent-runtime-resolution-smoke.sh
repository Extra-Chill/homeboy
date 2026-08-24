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
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

HOMEBOY_ROOT="${FIXTURE_ROOT}/config/homeboy"
EXTENSION_DIR="${HOMEBOY_ROOT}/extensions/wordpress"
mkdir -p "${EXTENSION_DIR}" "${HOMEBOY_ROOT}/extensions/scripts"
cp -R "${WORDPRESS_ROOT}/scripts" "${EXTENSION_DIR}/scripts"
cp -R "${WORDPRESS_ROOT}/lib" "${EXTENSION_DIR}/lib"
cp "${WORDPRESS_ROOT}/wordpress.json" "${EXTENSION_DIR}/wordpress.json"
cp -R "${REPOSITORY_ROOT}/agent-runtimes" "${HOMEBOY_ROOT}/agent-runtimes"
cp -R "${REPOSITORY_ROOT}/agent-task-contracts" "${HOMEBOY_ROOT}/agent-task-contracts"
cp -R "${REPOSITORY_ROOT}/dependency-adapters" "${HOMEBOY_ROOT}/dependency-adapters"
cp -R "${REPOSITORY_ROOT}/runtime-agent-ci" "${HOMEBOY_ROOT}/runtime-agent-ci"
ln -s "${REPOSITORY_ROOT}/scripts/lib" "${HOMEBOY_ROOT}/extensions/scripts/lib"

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
    env -u HOMEBOY_EXTENSION_PATH node "$@"
}

# Resolution is the invariant under test, so assert the resolved paths directly
# instead of inferring them from a runtime's exit code. The shared runtimes
# themselves depend on ambient tooling (the opencode executor spawns `homeboy`
# for contract constants), which is not what an installed-layout packaging test
# should be gated on.
for target in \
    "wp-codebox/lib/wp-codebox-runtime-selection.js" \
    "opencode/scripts/agent/homeboy-opencode-agent-task-executor.cjs"; do
    resolved="$(fixture_node "${EXTENSION_DIR}/scripts/lib/agent-runtime-paths.cjs" "${target}" 2>&1)"
    expected="$(cd "$(dirname "${HOMEBOY_ROOT}")" && pwd -P)/$(basename "${HOMEBOY_ROOT}")/agent-runtimes/${target}"
    [ "$resolved" = "$expected" ] || fail "Expected installed resolution of ${target} to ${expected}, got: ${resolved}"
done

# Resolving a shim filename is insufficient: execute the copied runtime shim so
# its dependency on the copied WordPress extension is proven in the installed
# sibling layout that failed before test inventory started in #2690.
selection_module="$(fixture_node "${EXTENSION_DIR}/scripts/lib/agent-runtime-paths.cjs" "wp-codebox/lib/wp-codebox-runtime-selection.js" 2>&1)"
selection_output="$(fixture_node - "${selection_module}" <<'NODE' 2>&1
const selection = require(process.argv[2]);
if (typeof selection.preflightWpCodeboxCommand !== 'function') process.exit(1);
process.stdout.write(selection.REQUIRED_WP_CODEBOX_VERSION);
NODE
)"
expected_version="$(fixture_node -p "require(process.argv[1]).minimum_version" "${HOMEBOY_ROOT}/agent-runtimes/wp-codebox/wp-codebox.json" 2>&1)"
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

# The result parser sources the WP Codebox adapters from the shared tree; when
# it cannot, wp-codebox-json silently degrades and a shard reports no counts.
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
    HOMEBOY_EXTENSION_PATH="${EXTENSION_DIR}" \
    bash "${EXTENSION_DIR}/scripts/test/parse-test-results.sh" \
        "${FIXTURE_ROOT}/artifacts" wp-codebox-json 2>&1)"
case "$parse_output" in
    *"total=3 passed=2 failed=1 skipped=0"*) ;;
    *) fail "Expected installed result parser to report WP Codebox counts, got: ${parse_output}" ;;
esac

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
