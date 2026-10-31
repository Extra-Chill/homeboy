#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHPSTAN_RUNNER="${ROOT_DIR}/scripts/lint/phpstan-runner.sh"

assert() {
    local condition="$1"
    local message="$2"
    if ! eval "$condition"; then
        echo "FAIL: ${message}" >&2
        exit 1
    fi
}

assert_path_for_template() {
    local template="$1"
    local expected_suffix="$2"
    local path

    path=$(homeboy_mktemp "$template")
    assert "[ -f \"\$path\" ]" "${template} should create a temp file"
    assert "[[ \"\$path\" == *\"${expected_suffix}\" ]]" "${template} should preserve ${expected_suffix} suffix: ${path}"
    assert "[[ \"\$path\" != *XXXXXX* ]]" "${template} should not leave literal XXXXXX: ${path}"
    rm -f "$path"
}

function_body=$(awk '/^homeboy_mktemp\(\)/,/^}$/' "$PHPSTAN_RUNNER")
eval "$function_body"

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT
export TMPDIR="$tmpdir"
unset HOMEBOY_CACHE_DIR

assert_path_for_template 'phpstan.XXXXXX.neon' '.neon'
assert_path_for_template 'phpstan-dependencies.XXXXXX.neon' '.neon'

grep -q "phpstan\.XXXXXX\.neon" "$PHPSTAN_RUNNER"
grep -q "phpstan-dependencies\.XXXXXX\.neon" "$PHPSTAN_RUNNER"

echo "phpstan temp config suffix smoke passed"
