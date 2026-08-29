#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The settings helper is core-owned; this smoke covers the helper extensions
# actually run, not a vendored copy of it.
# shellcheck source=../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
SETTINGS_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SETTINGS_HELPER settings.sh)" || exit 1

# shellcheck source=/dev/null
source "$SETTINGS_HELPER"

assert_equals() {
    local expected="$1"
    local actual="$2"
    local message="$3"

    if [ "$actual" != "$expected" ]; then
        echo "${message}: expected '${expected}', got '${actual}'" >&2
        exit 1
    fi
}

HOMEBOY_SETTINGS_JSON='{"test_backend":"host-smoke","testing":{"backend":"wp-codebox"}}'
assert_equals "host-smoke" "$(homeboy_setting test_backend '.test_backend // .testing.backend // empty')" "scalar setting alias order"

HOMEBOY_SETTINGS_JSON='{"testing":{"backend":"host"}}'
assert_equals "host" "$(homeboy_setting test_backend '.test_backend // .testing.backend // empty')" "nested scalar setting fallback"

HOMEBOY_SETTINGS_JSON='{}'
assert_equals "wp-codebox" "$(homeboy_setting test_backend '.test_backend // .testing.backend // empty' 'wp-codebox')" "scalar setting default"

HOMEBOY_SETTINGS_JSON='{"rust":{"bench":{"cargo_timings":true}}}'
assert_equals "true" "$(homeboy_setting_bool rust_bench_cargo_timings false '.rust_bench_cargo_timings // .rust.bench.cargo_timings // false')" "bool setting nested true"

HOMEBOY_SETTINGS_JSON='{"rust_bench_cargo_timings":"no"}'
assert_equals "false" "$(homeboy_setting_bool rust_bench_cargo_timings true '.rust_bench_cargo_timings // .rust.bench.cargo_timings // false')" "bool setting explicit false beats default"

HOMEBOY_SETTINGS_JSON='{"validation_dependencies":["agents-api","example-dependency"]}'
assert_equals '["agents-api","example-dependency"]' "$(homeboy_setting_array validation_dependencies '.validation_dependencies // .depends_on // []')" "array setting"

HOMEBOY_SETTINGS_JSON='not json'
assert_equals "fallback" "$(homeboy_setting missing '.missing // empty' 'fallback')" "invalid JSON scalar default"
assert_equals "false" "$(homeboy_setting_bool flag false '.flag // false')" "invalid JSON bool default"
assert_equals "[]" "$(homeboy_setting_array dependencies '.dependencies // []')" "invalid JSON array default"

HOMEBOY_SETTINGS_JSON='{"depends_on":"agents-api, example-dependency"}'
# shellcheck source=../wordpress/scripts/lib/validation-dependencies.sh
source "${ROOT_DIR}/wordpress/scripts/lib/validation-dependencies.sh"
assert_equals '"agents-api, example-dependency"' "$(homeboy_get_validation_dependencies_raw)" "validation dependency raw JSON string"

echo "settings helper smoke passed"
