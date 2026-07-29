#!/usr/bin/env bash
set -euo pipefail

# Regressions for validation dependency resolution.
#
#   1. Every documented `validation_dependencies` value shape normalizes to the
#      same token list. The comma-separated form previously spliced a literal
#      `n` between entries and dropped the trailing dependency.
#   2. A bare dependency slug is only satisfied by a plugin-shaped directory.
#      Components legitimately ship subdirectories named after a dependency
#      (bbPress theme-compat template overrides being the canonical case), and
#      those must not shadow the real plugin.
#   3. An explicitly relative or absolute path is still honoured verbatim, so
#      operators can point validation at a non-plugin directory on purpose.

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

expect_tokens() {
    local label="$1"
    local raw="$2"
    local expected="$3"
    local actual

    actual=$(homeboy_normalize_validation_dependencies "$raw" | tr '\n' '|')
    [ "$actual" = "$expected" ] || fail "${label}: expected '${expected}', got '${actual}'"
}

# 1. Documented value shapes.
expect_tokens 'comma-separated with spaces' 'data-machine, agents-api' 'data-machine|agents-api|'
expect_tokens 'comma-separated without spaces' 'a,b,c' 'a|b|c|'
expect_tokens 'single slug' 'solo-dependency' 'solo-dependency|'
expect_tokens 'JSON array' '["data-machine", "agents-api"]' 'data-machine|agents-api|'
expect_tokens 'newline separated' 'first
second' 'first|second|'

# A literal `n` must never appear as a separator artifact.
if homeboy_normalize_validation_dependencies 'data-machine, agents-api' | grep -q 'machinen'; then
    fail 'comma separator was spliced into a literal n'
fi

# 2. Slug resolution must skip non-plugin directories.
COMPONENT_DIR="${TMPDIR}/component"
mkdir -p "${COMPONENT_DIR}/bbpress" "${COMPONENT_DIR}/legit-dep"

# bbPress theme-compat override: real convention, not a plugin.
cat > "${COMPONENT_DIR}/bbpress/bbpress.php" <<'PHP'
<?php
/*
 * Template Name: bbPress Template
 */
get_header();
PHP

cat > "${COMPONENT_DIR}/legit-dep/legit-dep.php" <<'PHP'
<?php
/**
 * Plugin Name: Legit Dep
 */
PHP

export _HOMEBOY_DEP_PLUGIN_PATH="$COMPONENT_DIR"

shadow_output=$(homeboy_resolve_validation_dependency_path bbpress 2>&1 || true)
if grep -F "via direct path: ${COMPONENT_DIR}/bbpress" <<< "$shadow_output" >/dev/null; then
    fail "non-plugin template directory satisfied slug 'bbpress'"
fi
if ! grep -F 'not plugin-shaped' <<< "$shadow_output" >/dev/null; then
    fail "skipping a non-plugin directory should explain why: ${shadow_output}"
fi

# A plugin-shaped sibling must still resolve with zero configuration.
legit_output=$(homeboy_resolve_validation_dependency_path legit-dep 2>&1 || true)
if ! grep -F "via direct path: ${COMPONENT_DIR}/legit-dep" <<< "$legit_output" >/dev/null; then
    fail "plugin-shaped sibling directory should resolve: ${legit_output}"
fi

# 3. Explicit paths remain verbatim even when not plugin-shaped.
explicit_output=$(homeboy_resolve_validation_dependency_path ./bbpress 2>&1 || true)
if ! grep -F "via direct path: ${COMPONENT_DIR}/bbpress" <<< "$explicit_output" >/dev/null; then
    fail "explicit relative path should resolve verbatim: ${explicit_output}"
fi

echo "Validation dependency resolution regression smoke passed"
