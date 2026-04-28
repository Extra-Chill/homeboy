#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER="${SCRIPT_DIR}/../lib/playground-cleanup-noise.sh"

# shellcheck source=../lib/playground-cleanup-noise.sh
source "$HELPER"

assertions=0
assert_equals() {
    local expected="$1"
    local actual="$2"
    local message="$3"
    assertions=$((assertions + 1))

    if [ "$expected" != "$actual" ]; then
        echo "FAIL: ${message}" >&2
        echo "Expected: ${expected}" >&2
        echo "Actual:   ${actual}" >&2
        exit 1
    fi
}

benign_line="Failed to find stale Playground temp dirs: Error: ENOENT: no such file or directory, lstat '/private/var/folders/example/T/phpcs-child123'"
real_find_failure="Failed to find stale Playground temp dirs: Error: EACCES: permission denied, scandir '/private/var/folders/example/T'"
delete_failure="Failed to delete stale Playground temp dir: /private/var/folders/example/T/phpcs-child123 Error: EBUSY: resource busy or locked"
normal_line="Running PHPUnit tests via WordPress Playground..."

filtered=$(printf '%s\n%s\n%s\n%s\n' "$normal_line" "$benign_line" "$real_find_failure" "$delete_failure" | homeboy_filter_playground_cleanup_noise)
expected=$(printf '%s\n%s\n%s' "$normal_line" "$real_find_failure" "$delete_failure")
assert_equals "$expected" "$filtered" "filters only the ENOENT lstat stale-temp race"

debug_filtered=$(printf '%s\n' "$benign_line" | HOMEBOY_DEBUG=1 homeboy_filter_playground_cleanup_noise)
assert_equals "$benign_line" "$debug_filtered" "debug mode preserves the benign cleanup race line"

echo "Playground cleanup noise smoke passed (${assertions} assertions)"
