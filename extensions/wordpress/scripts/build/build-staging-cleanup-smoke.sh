#!/usr/bin/env bash
# The build must not leave its staging tree in the component directory.
#
# Deployment does not use the production ZIP's exclude list — homeboy rsyncs the
# component directory to the server — so anything left beside the built output
# is copied verbatim. `.homeboy-build/` therefore reached production on five
# components, where Imagify's media scanner descended into it and fatalled:
#
#   PHP Fatal error: Uncaught UnexpectedValueException:
#     RecursiveDirectoryIterator::__construct(.../.homeboy-build/<project>/.homeboy-build):
#     Failed to open directory: Permission denied
#
# That was every PHP fatal on the network. Excluding the path from the ZIP was
# never enough, because the ZIP is only one of two routes to a server (#2861).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_SH="${SCRIPT_DIR}/build.sh"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-staging-cleanup.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

fail=0
pass() { echo "PASS: $1"; }
bad() { echo "FAIL: $1" >&2; fail=1; }

# ---------------------------------------------------------------------------
# Behaviour: the helper removes the staging root, not just the project inside.
# Exercised directly, because a full build needs a WordPress toolchain.
# ---------------------------------------------------------------------------
STAGING_ROOT=".homeboy-build"
PROJECT_NAME="demo-plugin"

remove_staging_tree() {
    [ -n "${STAGING_ROOT:-}" ] || return 0
    rm -rf "${STAGING_ROOT:?}/${PROJECT_NAME:-}" 2>/dev/null || true
    rmdir "${STAGING_ROOT}" 2>/dev/null || true
}

cd "${TMP_DIR}"

# A completed build: staging root holding a populated project copy.
mkdir -p "${STAGING_ROOT}/${PROJECT_NAME}/inc"
printf 'x' > "${STAGING_ROOT}/${PROJECT_NAME}/demo-plugin.php"
printf 'x' > "${STAGING_ROOT}/${PROJECT_NAME}/inc/thing.php"

remove_staging_tree

if [ ! -e "${STAGING_ROOT}" ]; then
    pass "a completed build leaves no staging root behind"
else
    bad "staging root survived: $(find "${STAGING_ROOT}" | head -5 | tr '\n' ' ')"
fi

# Idempotent: cleanup also runs from the EXIT trap, so it must tolerate a
# staging root that is already gone.
remove_staging_tree
pass "removing an absent staging tree is not an error"

# A staging root containing something unexpected must not be blindly deleted.
mkdir -p "${STAGING_ROOT}"
printf 'unexpected' > "${STAGING_ROOT}/not-ours.txt"
remove_staging_tree
if [ -f "${STAGING_ROOT}/not-ours.txt" ]; then
    pass "a non-empty staging root is left alone rather than force-removed"
else
    bad "unrelated content under the staging root was destroyed"
fi
rm -rf "${STAGING_ROOT}"

# ---------------------------------------------------------------------------
# Source guards: both exit paths must call the helper.
# ---------------------------------------------------------------------------
if grep -q 'remove_staging_tree()' "${BUILD_SH}"; then
    pass "build.sh defines a single staging-removal helper"
else
    bad "build.sh has no remove_staging_tree helper"
fi

# The success path previously removed nothing at all, which is how a populated
# staging tree survived into the deployed directory.
if awk '/^    create_production_zip/,/print_success "Build process completed/' "${BUILD_SH}" \
    | grep -q 'remove_staging_tree'; then
    pass "the success path removes the staging tree after packaging"
else
    bad "the success path does not remove the staging tree"
fi

# The trap previously only fired on a non-zero exit.
if awk '/^cleanup\(\) \{/,/^\}/' "${BUILD_SH}" | grep -q 'remove_staging_tree'; then
    pass "the exit trap removes the staging tree"
else
    bad "the exit trap does not remove the staging tree"
fi

if awk '/^cleanup\(\) \{/,/^\}/' "${BUILD_SH}" | grep -q 'exit_code -ne 0'; then
    bad "staging removal is still conditional on a failing exit code"
else
    pass "staging removal is unconditional, not failure-only"
fi

if [ "${fail}" -ne 0 ]; then
    echo "staging cleanup checks FAILED" >&2
    exit 1
fi
echo "All staging cleanup checks passed."
