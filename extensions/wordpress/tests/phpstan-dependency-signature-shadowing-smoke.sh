#!/usr/bin/env bash
set -euo pipefail

# Regression: a validation dependency's PHP sources must be registered as
# `scanFiles:`, not only through `scanDirectories:`.
#
# `scanDirectories:` declarations lose to project-source declarations during
# signature resolution, the same precedence trap phpstan.neon.dist documents for
# `bootstrapFiles:`. Standalone test files idiomatically declare bare stubs
# (`function bbp_get_template_part() {}`) so they can run under plain `php`.
# When such a file is inside the analysed scope, its stub outranks the
# dependency's real `bbp_get_template_part( $slug, $name = null )`, and every
# genuine two-argument call in component source is reported as `invoked with 2
# parameters, 0 required`. Those findings look like component defects but are
# artifacts of test scaffolding winning the symbol graph.
#
# This asserts the generated dependency config carries both mechanisms:
# `scanDirectories:` for whole-tree class discovery, and `scanFiles:` entries
# pinning the dependency's function signatures.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/lint/phpstan-runner.sh"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

DEPENDENCY_DIR="${TMP_ROOT}/fixture-dependency"
mkdir -p "${DEPENDENCY_DIR}/includes/core" "${DEPENDENCY_DIR}/vendor/acme" "${DEPENDENCY_DIR}/tests"

cat > "${DEPENDENCY_DIR}/fixture-dependency.php" <<'PHP'
<?php
/**
 * Plugin Name: Fixture Dependency
 */
PHP

cat > "${DEPENDENCY_DIR}/includes/core/template-functions.php" <<'PHP'
<?php
function fixture_dep_template_part( $slug, $name = null ) {
    return $slug . (string) $name;
}
PHP

# Must never be pinned: vendored code and the dependency's own tests.
cat > "${DEPENDENCY_DIR}/vendor/acme/vendored.php" <<'PHP'
<?php
function fixture_vendored_helper() {}
PHP

cat > "${DEPENDENCY_DIR}/tests/dependency-own-smoke.php" <<'PHP'
<?php
function fixture_dep_template_part() {}
PHP

# The runner is an executable script, not a sourceable library, so extract the
# helper for a focused unit check instead of running a full analysis.
eval "$(awk '/^homeboy_resolve_phpstan_dependency_signature_files\(\)/,/^}/' "$RUNNER")"

if ! declare -F homeboy_resolve_phpstan_dependency_signature_files >/dev/null 2>&1; then
    echo "FAIL: homeboy_resolve_phpstan_dependency_signature_files() is not defined" >&2
    exit 1
fi

signature_files="$(homeboy_resolve_phpstan_dependency_signature_files "$DEPENDENCY_DIR")"

if ! grep -F "${DEPENDENCY_DIR}/includes/core/template-functions.php" <<< "$signature_files" >/dev/null; then
    echo "FAIL: dependency function signatures must be pinned as scanFiles entries" >&2
    printf '%s\n' "$signature_files" >&2
    exit 1
fi

if ! grep -F "${DEPENDENCY_DIR}/fixture-dependency.php" <<< "$signature_files" >/dev/null; then
    echo "FAIL: dependency root plugin file must be pinned" >&2
    printf '%s\n' "$signature_files" >&2
    exit 1
fi

if grep -F "${DEPENDENCY_DIR}/vendor/acme/vendored.php" <<< "$signature_files" >/dev/null; then
    echo "FAIL: vendored dependency code must not be pinned" >&2
    exit 1
fi

if grep -F "${DEPENDENCY_DIR}/tests/dependency-own-smoke.php" <<< "$signature_files" >/dev/null; then
    echo "FAIL: the dependency's own tests must not be pinned" >&2
    exit 1
fi

# The generated dependency config must emit both mechanisms for the dependency.
if ! grep -qF 'homeboy_resolve_phpstan_dependency_signature_files "$dependency_path"' "$RUNNER"; then
    echo "FAIL: generate_dependency_config() must pin dependency signature files" >&2
    exit 1
fi

if ! grep -q 'scanDirectories:' "$RUNNER"; then
    echo "FAIL: generate_dependency_config() must still emit scanDirectories" >&2
    exit 1
fi

echo "PHPStan dependency signature shadowing smoke passed"
