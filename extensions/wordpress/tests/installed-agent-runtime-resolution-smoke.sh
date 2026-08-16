#!/usr/bin/env bash
# Prove the installed extension layout can load shared agent-runtime files.
#
# `agent-runtimes` is a shared asset: Homeboy installs it at
# <homeboy>/agent-runtimes, a sibling of <homeboy>/extensions, while a monorepo
# checkout keeps it one level closer to the extension. A linked dev install
# hides the difference because Node and `pwd -P` resolve a symlinked extension
# back to the checkout, so this fixture COPIES the extension scripts — that is
# the shape a fresh CI runner has, and the shape that produced four identical
# MODULE_NOT_FOUND shard bootstrap failures in #12585.
set -euo pipefail

WORDPRESS_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPOSITORY_ROOT="$(cd "${WORDPRESS_ROOT}/.." && pwd)"
FIXTURE_ROOT="$(mktemp -d)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

HOMEBOY_ROOT="${FIXTURE_ROOT}/config/homeboy"
EXTENSION_DIR="${HOMEBOY_ROOT}/extensions/wordpress"
mkdir -p "${EXTENSION_DIR}" "${HOMEBOY_ROOT}/extensions/scripts"
cp -R "${WORDPRESS_ROOT}/scripts" "${EXTENSION_DIR}/scripts"
ln -s "${REPOSITORY_ROOT}/agent-runtimes" "${HOMEBOY_ROOT}/agent-runtimes"
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
    expected="${HOMEBOY_ROOT}/agent-runtimes/${target}"
    [ "$resolved" = "$expected" ] || fail "Expected installed resolution of ${target} to ${expected}, got: ${resolved}"
done

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

# An incomplete install must name what is missing instead of emitting a bare
# MODULE_NOT_FOUND stack that says nothing about shared-asset packaging.
rm "${HOMEBOY_ROOT}/agent-runtimes"
missing_output="$(HOMEBOY_SETTINGS_JSON='{}' fixture_node "${EXTENSION_DIR}/scripts/test/wp-codebox-phpunit-adapter.mjs" 2>&1)"
missing_status=$?
[ "$missing_status" -ne 0 ] || fail "Expected missing shared runtime to fail the adapter"
case "$missing_output" in
    *"could not resolve shared agent runtime file"*"${HOMEBOY_ROOT}/agent-runtimes"*"homeboy extension install wordpress"*) ;;
    *) fail "Expected an actionable missing-runtime diagnostic, got: ${missing_output}" ;;
esac

printf '%s\n' 'installed agent runtime resolution smoke passed'
