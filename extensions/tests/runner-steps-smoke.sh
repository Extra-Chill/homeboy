#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# shellcheck source=/dev/null
source "$ROOT/wordpress/scripts/lib/runner-steps.sh"

assert_runs() {
    local label="$1"
    local step="$2"

    if ! should_run_step "$step"; then
        echo "FAIL: expected $label to run" >&2
        exit 1
    fi
}

assert_skips() {
    local label="$1"
    local step="$2"

    if should_run_step "$step"; then
        echo "FAIL: expected $label to skip" >&2
        exit 1
    fi
}

unset HOMEBOY_STEP HOMEBOY_SKIP
assert_runs "unfiltered step" "phpcs"
assert_runs "empty step" ""
assert_runs "whitespace-padded step name" " phpcs "

HOMEBOY_STEP="phpcs,eslint"
unset HOMEBOY_SKIP
assert_runs "allowlisted step" "phpcs"
assert_skips "non-allowlisted step" "phpstan"
assert_runs "empty step with allowlist" ""

HOMEBOY_SKIP="phpcs"
assert_skips "skiplisted step also present in allowlist" "phpcs"
assert_runs "allowlisted step not skiplisted" "eslint"
assert_skips "non-allowlisted step with skiplist" "phpstan"

unset HOMEBOY_STEP
HOMEBOY_SKIP="eslint,phpstan"
assert_runs "step not in skiplist" "phpcs"
assert_skips "skiplisted step without allowlist" "phpstan"

echo "runner-steps smoke ok"
