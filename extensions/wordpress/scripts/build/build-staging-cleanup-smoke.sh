#!/usr/bin/env bash
# A build leaves the component tree exactly as it found it, apart from the
# declared ZIP artifact.
#
# Deployment rsyncs the component directory to the server verbatim, so anything
# a build stages inside it ships. An in-tree `.homeboy-build/` once reached
# production on five components, where Imagify's media scanner descended into
# it and fatalled (#2861). Staging now lives in a private temp directory; this
# smoke runs real builds (success and failure) against a git component and
# asserts the tree and the temp area are clean afterwards. The component root is
# read-only during the build, so in-tree staging fails loudly.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${EXTENSION_DIR}/.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
RESOLVE_CONTEXT_CORE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RESOLVE_CONTEXT resolve-context.sh)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-staging-cleanup.XXXXXX")"
trap 'chmod -R u+w "$TMP_DIR" 2>/dev/null; rm -rf "$TMP_DIR"' EXIT

fail=0
pass() { echo "PASS: $1"; }
bad() { echo "FAIL: $1" >&2; fail=1; }

make_component() {
    local dir="$1" name="$2" body="$3"
    mkdir -p "${dir}/inc"
    cat > "${dir}/${name}.php" <<PHP
<?php
/**
 * Plugin Name: ${name}
 * Version: 1.0.0
 */
PHP
    printf '%s\n' "$body" > "${dir}/inc/thing.php"
    printf '%s\n' 'build/' > "${dir}/.gitignore"
    git -C "$dir" init -q
    git -C "$dir" add -A
    git -C "$dir" -c user.name=smoke -c user.email=smoke@example.invalid commit -qm init
    # Only the declared artifact directory is writable: any attempt to stage
    # inside the component root fails the build instead of passing silently.
    mkdir -p "${dir}/build"
    chmod a-w "$dir" "${dir}/inc"
}

# Runs build.sh with an isolated TMPDIR so leftover staging is observable.
run_build() {
    local dir="$1" build_tmp="$2"
    mkdir -p "$build_tmp"
    (
        cd "$dir"
        TMPDIR="$build_tmp" \
        HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
        HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_CORE_HELPER" \
        HOMEBOY_SKIP_TESTS=1 \
            bash "${EXTENSION_DIR}/scripts/build/build.sh"
    )
}

# Everything git sees, ignored files included, other than the declared artifact.
tree_residue() {
    git -C "$1" status --porcelain --ignored | grep -vE "^!! build/$" || true
}

# Success: the ZIP is produced and nothing else is left behind.
good_dir="${TMP_DIR}/good-plugin"
good_tmp="${TMP_DIR}/good-tmp"
make_component "$good_dir" good-plugin '<?php function good_plugin_thing() { return true; }'
if run_build "$good_dir" "$good_tmp" > "${TMP_DIR}/good.out" 2>&1; then
    pass "a valid component builds"
else
    bad "a valid component failed to build"; sed 's/^/  /' "${TMP_DIR}/good.out" >&2
fi
if unzip -Z1 "${good_dir}/build/good-plugin.zip" 2>/dev/null | grep -qx 'good-plugin/inc/thing.php'; then
    pass "the ZIP holds the staged component"
else
    bad "the ZIP is missing or incomplete"
fi
residue="$(tree_residue "$good_dir")"
[ -z "$residue" ] && pass "a successful build leaves the component tree clean" \
    || bad "a successful build left: ${residue}"
[ -z "$(ls -A "$good_tmp")" ] && pass "a successful build removes its staging directory" \
    || bad "a successful build left staging: $(ls -A "$good_tmp")"

# Failure: a syntax error aborts the build and still leaves nothing behind.
bad_dir="${TMP_DIR}/bad-plugin"
bad_tmp="${TMP_DIR}/bad-tmp"
make_component "$bad_dir" bad-plugin '<?php function bad_plugin_thing( { return true; }'
if run_build "$bad_dir" "$bad_tmp" > "${TMP_DIR}/bad.out" 2>&1; then
    bad "a component with a PHP syntax error built successfully"
else
    pass "a component with a PHP syntax error fails the build"
fi
residue="$(tree_residue "$bad_dir")"
[ -z "$residue" ] && pass "a failed build leaves the component tree clean" \
    || bad "a failed build left: ${residue}"
[ -z "$(ls -A "$bad_tmp")" ] && pass "a failed build removes its staging directory" \
    || bad "a failed build left staging: $(ls -A "$bad_tmp")"

if [ "${fail}" -ne 0 ]; then
    echo "staging cleanup checks FAILED" >&2
    exit 1
fi
echo "All staging cleanup checks passed."
