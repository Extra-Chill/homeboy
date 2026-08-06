#!/usr/bin/env bash
set -euo pipefail

# Pins the WP Codebox CLI resolution contract shared by the managed WordPress
# PHPUnit runner.
#
# Regression: a wrapper left on PATH by an earlier install pointed at a managed
# source cache whose `packages/cli/dist/index.js` had never been built. The
# PHPUnit adapter resolved the CLI on its own — bare env vars falling back to
# the literal string `wp-codebox` — so it exec'd that wrapper and the operator
# got a raw `Cannot find module` Node stack instead of a Homeboy diagnosis.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
PATHS_LIB="${EXTENSION_ROOT}/scripts/lib/wp-codebox-paths.sh"
ADAPTER="${EXTENSION_ROOT}/scripts/test/wp-codebox-phpunit-adapter.mjs"
RUNNER="${EXTENSION_ROOT}/scripts/test/test-runner-wp-codebox.sh"
SETUP="${EXTENSION_ROOT}/scripts/build/setup.sh"

TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "${TMP_ROOT}"' EXIT

failures=0

fail() {
    echo "FAIL: $1" >&2
    failures=$((failures + 1))
}

pass() {
    echo "ok: $1"
}

# --- fixtures ---------------------------------------------------------------

# A managed cache that was cloned but never built: the source tree exists and
# the CLI entrypoint does not.
INCOMPLETE_CACHE="${TMP_ROOT}/incomplete-cache"
mkdir -p "${INCOMPLETE_CACHE}/source/packages/cli"

# A managed cache with a built CLI entrypoint.
COMPLETE_CACHE="${TMP_ROOT}/complete-cache"
mkdir -p "${COMPLETE_CACHE}/source/packages/cli/dist"
cat > "${COMPLETE_CACHE}/source/packages/cli/dist/index.js" <<'NODE'
#!/usr/bin/env node
process.stdout.write('commands\n');
NODE

# A wrapper of the exact shape an earlier install wrote, pointing at the
# unbuilt entrypoint.
STALE_BIN_DIR="${TMP_ROOT}/stale-bin"
mkdir -p "${STALE_BIN_DIR}"
cat > "${STALE_BIN_DIR}/wp-codebox" <<EOF
#!/usr/bin/env bash
exec node "${INCOMPLETE_CACHE}/source/packages/cli/dist/index.js" "\$@"
EOF
chmod +x "${STALE_BIN_DIR}/wp-codebox"

# --- 1. a stale PATH wrapper is never handed back as a usable CLI -----------

resolution_stderr="${TMP_ROOT}/resolution.err"
resolution_stdout="${TMP_ROOT}/resolution.out"
set +e
env -i \
    PATH="${STALE_BIN_DIR}:/usr/bin:/bin" \
    HOME="${TMP_ROOT}/home" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${INCOMPLETE_CACHE}" \
    bash -c 'source "$1" && homeboy_wp_codebox_resolve_bin ""' \
    wp-codebox-resolution "${PATHS_LIB}" \
    > "${resolution_stdout}" 2> "${resolution_stderr}"
resolution_status=$?
set -e

if [ "${resolution_status}" -eq 0 ]; then
    fail "resolver accepted a stale wrapper whose target is missing (returned: $(cat "${resolution_stdout}"))"
else
    pass "resolver rejects a stale wrapper whose target is missing"
fi

if grep -q "${INCOMPLETE_CACHE}/source/packages/cli/dist/index.js" "${resolution_stderr}"; then
    pass "resolver diagnostic names the missing CLI entrypoint"
else
    fail "resolver diagnostic does not name the missing CLI entrypoint: $(cat "${resolution_stderr}")"
fi

if grep -qi 'Cannot find module' "${resolution_stderr}"; then
    fail "resolver leaked a raw Node module error instead of a Homeboy diagnostic"
else
    pass "resolver does not leak a raw Node module error"
fi

# --- 2. an incomplete managed cache is reported as incomplete ---------------

if env -i PATH="/usr/bin:/bin" HOME="${TMP_ROOT}/home" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${INCOMPLETE_CACHE}" \
    bash -c 'source "$1" && homeboy_wp_codebox_managed_cache_is_incomplete' \
    wp-codebox-cache "${PATHS_LIB}"; then
    pass "an unbuilt managed source cache is classified incomplete"
else
    fail "an unbuilt managed source cache was not classified incomplete"
fi

if env -i PATH="/usr/bin:/bin" HOME="${TMP_ROOT}/home" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${COMPLETE_CACHE}" \
    bash -c 'source "$1" && homeboy_wp_codebox_managed_cache_is_incomplete' \
    wp-codebox-cache "${PATHS_LIB}"; then
    fail "a built managed source cache was misclassified as incomplete"
else
    pass "a built managed source cache is not classified incomplete"
fi

# --- 3. the exported argv prefixes node for a .js entrypoint ----------------

command_json="$(env -i \
    PATH="/usr/bin:/bin" \
    HOME="${TMP_ROOT}/home" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${COMPLETE_CACHE}" \
    bash -c 'source "$1" && homeboy_wp_codebox_export_command "" && printf "%s" "$HOMEBOY_WP_CODEBOX_COMMAND_JSON"' \
    wp-codebox-command "${PATHS_LIB}" 2>/dev/null || true)"

expected_json="[\"node\",\"${COMPLETE_CACHE}/source/packages/cli/dist/index.js\"]"
if [ "${command_json}" = "${expected_json}" ]; then
    pass "exported argv prefixes node for a .js entrypoint"
else
    fail "exported argv is ${command_json}, expected ${expected_json}"
fi

# --- 4. a validated explicit override outranks the managed cache ------------

OVERRIDE_BIN="${TMP_ROOT}/override-wp-codebox"
cat > "${OVERRIDE_BIN}" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "${OVERRIDE_BIN}"

override_json="$(env -i \
    PATH="/usr/bin:/bin" \
    HOME="${TMP_ROOT}/home" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${COMPLETE_CACHE}" \
    HOMEBOY_WP_CODEBOX_BIN="${OVERRIDE_BIN}" \
    bash -c 'source "$1" && homeboy_wp_codebox_export_command "" && printf "%s" "$HOMEBOY_WP_CODEBOX_COMMAND_JSON"' \
    wp-codebox-override "${PATHS_LIB}" 2>/dev/null || true)"

if [ "${override_json}" = "[\"${OVERRIDE_BIN}\"]" ]; then
    pass "a runnable explicit override outranks the managed cache"
else
    fail "explicit override produced ${override_json}, expected [\"${OVERRIDE_BIN}\"]"
fi

# A pin at a path that is not there is a dangling pin, not an instruction to
# hand it to the runtime.
dangling_override_json="$(env -i \
    PATH="/usr/bin:/bin" \
    HOME="${TMP_ROOT}/home" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${COMPLETE_CACHE}" \
    HOMEBOY_WP_CODEBOX_BIN="${TMP_ROOT}/does-not-exist/wp-codebox" \
    bash -c 'source "$1" && homeboy_wp_codebox_export_command "" && printf "%s" "$HOMEBOY_WP_CODEBOX_COMMAND_JSON"' \
    wp-codebox-dangling-override "${PATHS_LIB}" 2>/dev/null || true)"

if [ "${dangling_override_json}" = "${expected_json}" ]; then
    pass "a dangling explicit override falls through to full resolution"
else
    fail "dangling override produced ${dangling_override_json}, expected ${expected_json}"
fi

# An override is pinned by the caller, so it is not subjected to the ambient
# `commands` probe: a CLI that exits non-zero for unknown verbs is still used.
NONPROBING_OVERRIDE="${TMP_ROOT}/nonprobing-wp-codebox"
cat > "${NONPROBING_OVERRIDE}" <<'SH'
#!/usr/bin/env bash
exit 1
SH
chmod +x "${NONPROBING_OVERRIDE}"

nonprobing_json="$(env -i \
    PATH="/usr/bin:/bin" \
    HOME="${TMP_ROOT}/home" \
    HOMEBOY_WP_CODEBOX_INSTALL_DIR="${COMPLETE_CACHE}" \
    HOMEBOY_WP_CODEBOX_BIN="${NONPROBING_OVERRIDE}" \
    bash -c 'source "$1" && homeboy_wp_codebox_export_command "" && printf "%s" "$HOMEBOY_WP_CODEBOX_COMMAND_JSON"' \
    wp-codebox-nonprobing "${PATHS_LIB}" 2>/dev/null || true)"

if [ "${nonprobing_json}" = "[\"${NONPROBING_OVERRIDE}\"]" ]; then
    pass "an explicit override is not subjected to the ambient runtime probe"
else
    fail "non-probing override produced ${nonprobing_json}, expected [\"${NONPROBING_OVERRIDE}\"]"
fi

# --- 5. the adapter consumes the resolved argv, never a bare CLI string -----

if grep -q "HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox'" "${ADAPTER}"; then
    fail "the adapter still resolves the CLI inline instead of using the shared seam"
else
    pass "the adapter has no inline CLI fallback expression"
fi

if grep -q 'homeboy_wp_codebox_export_command' "${RUNNER}"; then
    pass "the runner resolves and exports the CLI argv before exec'ing the adapter"
else
    fail "the runner does not export a resolved CLI argv"
fi

if grep -q 'homeboy_wp_codebox_export_command' "${ADAPTER}"; then
    pass "the adapter delegates resolution to the shared shell library"
else
    fail "the adapter does not delegate resolution to the shared shell library"
fi

if grep -q 'HOMEBOY_WP_CODEBOX_COMMAND_JSON' "${ADAPTER}"; then
    pass "the adapter consumes the exported argv contract"
else
    fail "the adapter does not consume the exported argv contract"
fi

# --- 6. setup prunes a stale wrapper whose target is gone -------------------

PRUNE_BIN_DIR="${TMP_ROOT}/prune-bin"
mkdir -p "${PRUNE_BIN_DIR}"
cp "${STALE_BIN_DIR}/wp-codebox" "${PRUNE_BIN_DIR}/wp-codebox"

prune_snippet="$(sed -n '/^    prune_stale_wp_codebox_wrapper() {$/,/^    }$/p' "${SETUP}")"
if [ -z "${prune_snippet}" ]; then
    fail "setup.sh no longer defines prune_stale_wp_codebox_wrapper"
else
    bash -c "
set -euo pipefail
${prune_snippet#    }
prune_stale_wp_codebox_wrapper '${PRUNE_BIN_DIR}/wp-codebox'
" 2>/dev/null || true

    if [ -f "${PRUNE_BIN_DIR}/wp-codebox" ]; then
        fail "setup did not prune a wrapper whose target is missing"
    else
        pass "setup prunes a wrapper whose target is missing"
    fi

    cat > "${PRUNE_BIN_DIR}/wp-codebox" <<EOF
#!/usr/bin/env bash
exec node "${COMPLETE_CACHE}/source/packages/cli/dist/index.js" "\$@"
EOF
    bash -c "
set -euo pipefail
${prune_snippet#    }
prune_stale_wp_codebox_wrapper '${PRUNE_BIN_DIR}/wp-codebox'
" 2>/dev/null || true

    if [ -f "${PRUNE_BIN_DIR}/wp-codebox" ]; then
        pass "setup keeps a wrapper whose target exists"
    else
        fail "setup pruned a wrapper whose target exists"
    fi
fi

# ---------------------------------------------------------------------------

if [ "${failures}" -ne 0 ]; then
    echo "" >&2
    echo "WP Codebox CLI resolution smoke failed (${failures} assertion(s))." >&2
    exit 1
fi

echo ""
echo "WP Codebox CLI resolution smoke passed."
