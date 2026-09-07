#!/usr/bin/env bash
set -euo pipefail

# A declared validation dependency that cannot be resolved must fail the
# resolver, and the export used by the test/trace/autoload runners must
# propagate that failure.
#
# Before this contract, an unresolved dependency was a `Warning:` and the run
# continued with a partial plugin graph. Every test touching the missing
# plugin then failed for reasons unrelated to the code under test — on
# extrachill-users this produced 109 phantom failures and Homeboy stamped the
# evidence `invalid_evidence` while still reporting a fake pass/fail count.
#
# The same file also covers the composer-install failure path: the full
# composer error must be surfaced, not a one-line warning, because the
# composer output is the only thing that says *why* (e.g. `requires php
# >=8.4` on a runner that got PHP 8.3 because setup-php was skipped).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER="${SCRIPT_DIR}/../scripts/lib/validation-dependencies.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

# shellcheck source=/dev/null
source "$HELPER"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

COMPONENT_DIR="${TMPDIR}/component"
mkdir -p "${COMPONENT_DIR}/present-dep"
cat > "${COMPONENT_DIR}/present-dep/present-dep.php" <<'PHP'
<?php
/**
 * Plugin Name: Present Dep
 */
PHP

export _HOMEBOY_DEP_PLUGIN_PATH="$COMPONENT_DIR"

# Stub the settings source so the resolver sees two declared dependencies:
# one that exists as a plugin-shaped sibling and one that does not exist
# anywhere. Stub the entry resolver so the missing slug never reaches the
# GitHub clone fallback (keeps the smoke offline and deterministic).
homeboy_get_validation_dependencies_raw() {
    printf '%s\n' '["present-dep", "does-not-exist-anywhere"]'
}

_homeboy_resolve_validation_dependency_entry_path() {
    local dependency="${1:-}"
    case "$dependency" in
        present-dep) printf '%s\n' "${COMPONENT_DIR}/present-dep" ;;
        *) return 1 ;;
    esac
}

homeboy_get_requires_plugins_from_header() {
    return 0
}

# 1. Resolver returns non-zero and names the dependency.
set +e
resolver_output=$(homeboy_resolve_validation_dependency_paths "$COMPONENT_DIR" 2>&1)
resolver_exit=$?
set -e

[ "$resolver_exit" -ne 0 ] || fail "resolver must fail when a declared dependency is unresolved (exit ${resolver_exit})"
grep -F "Could not resolve WordPress validation dependency 'does-not-exist-anywhere'" <<< "$resolver_output" >/dev/null \
    || fail "resolver should name the unresolved dependency: ${resolver_output}"
grep -F "Refusing to continue with a partial dependency graph" <<< "$resolver_output" >/dev/null \
    || fail "resolver should explain why it refused: ${resolver_output}"
if grep -F 'Warning: Could not resolve' <<< "$resolver_output" >/dev/null; then
    fail "unresolved dependency must be an Error, not a Warning: ${resolver_output}"
fi

# 2. The export used by the runners propagates the failure and does not
#    export a partial HOMEBOY_WORDPRESS_DEPENDENCY_PATHS.
unset HOMEBOY_WORDPRESS_DEPENDENCY_PATHS
set +e
export_output=$(homeboy_export_validation_dependency_paths "$COMPONENT_DIR" 2>&1)
export_exit=$?
set -e

[ "$export_exit" -ne 0 ] || fail "export must propagate resolver failure (exit ${export_exit})"
[ -z "${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS:-}" ] \
    || fail "export must not publish a partial dependency list: ${HOMEBOY_WORDPRESS_DEPENDENCY_PATHS}"

# 3. With every declared dependency resolvable, the resolver succeeds and the
#    strict path is not triggered.
homeboy_get_validation_dependencies_raw() {
    printf '%s\n' '["present-dep"]'
}
ok_output=$(homeboy_resolve_validation_dependency_paths "$COMPONENT_DIR" 2>&1) \
    || fail "resolver must succeed when every declared dependency resolves: ${ok_output}"
grep -F "${COMPONENT_DIR}/present-dep" <<< "$ok_output" >/dev/null \
    || fail "resolver should emit the resolved path: ${ok_output}"

# 4. A composer install failure surfaces the composer error, not a one-liner.
if command -v composer >/dev/null 2>&1 && command -v php >/dev/null 2>&1; then
    BROKEN_DEP="${TMPDIR}/broken-composer-dep"
    mkdir -p "$BROKEN_DEP"
    cat > "${BROKEN_DEP}/broken-composer-dep.php" <<'PHP'
<?php
/**
 * Plugin Name: Broken Composer Dep
 */
PHP
    # An impossible PHP constraint is the same failure class as a dependency
    # that requires a newer PHP than the runner installed.
    cat > "${BROKEN_DEP}/composer.json" <<'JSON'
{
  "name": "homeboy-smoke/broken-composer-dep",
  "require": {
    "php": ">=99.0"
  }
}
JSON

    set +e
    prepare_output=$(_homeboy_prepare_cloned_dependency "$BROKEN_DEP" 2>&1)
    prepare_exit=$?
    set -e

    [ "$prepare_exit" -ne 0 ] || fail "composer failure must fail dependency preparation"
    grep -F "Error: Could not install Composer dependencies" <<< "$prepare_output" >/dev/null \
        || fail "composer failure should be reported as an Error: ${prepare_output}"
    grep -F "Composer output (PHP" <<< "$prepare_output" >/dev/null \
        || fail "composer failure should report the PHP version composer ran under: ${prepare_output}"
    grep -Fi "requires php" <<< "$prepare_output" >/dev/null \
        || fail "composer's own problem report must be surfaced, not swallowed: ${prepare_output}"
else
    echo "composer/php unavailable; skipping composer-failure surface check" >&2
fi

echo "Validation dependency unresolved-fatal smoke passed"
