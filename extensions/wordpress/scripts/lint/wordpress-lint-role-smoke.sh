#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component"
PHPCS_ARGS_FILE="${TMPDIR}/phpcs-args.txt"
PHPSTAN_CALLS_FILE="${TMPDIR}/phpstan-calls.txt"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context.sh"
DETECT_COMPONENT_HELPER="${TMPDIR}/detect-component.sh"
DEPENDENCY_HELPER="${TMPDIR}/validation-dependencies.sh"

mkdir -p \
    "${EXTENSION_DIR}/vendor/bin" \
    "${EXTENSION_DIR}/scripts/lint" \
    "${EXTENSION_DIR}/scripts/lib" \
    "${COMPONENT_DIR}/tools" \
    "${COMPONENT_DIR}/tests"

touch "${EXTENSION_DIR}/phpcs.xml.dist" "${EXTENSION_DIR}/phpstan.neon.dist"
mkdir -p "${EXTENSION_DIR}/rulesets"
touch "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"
ln -s "${SCRIPT_DIR}/phpstan-runner.sh" "${EXTENSION_DIR}/scripts/lint/phpstan-runner.sh"

cat > "${COMPONENT_DIR}/scoper.inc.php" <<'PHP'
<?php
return [
    'prefix' => 'Example\\Vendor',
];
PHP

cat > "${COMPONENT_DIR}/tools/build-autoloader.php" <<'PHP'
<?php
fwrite(STDOUT, "building\n");
PHP

cat > "${COMPONENT_DIR}/tests/BFBConversionUnitTest.php" <<'PHP'
<?php
class BFBConversionUnitTest extends WP_UnitTestCase {}
PHP

cat > "${COMPONENT_DIR}/tests/smoke-content-normalization.php" <<'PHP'
<?php
require_once __DIR__ . '/../vendor/autoload.php';
PHP

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-role-smoke}"
}
SH

cat > "$DETECT_COMPONENT_HELPER" <<'SH'
homeboy_detect_component() {
    HOMEBOY_COMPONENT_TYPE="plugin"
    HOMEBOY_COMPONENT_MAIN_FILE="role-smoke.php"
    HOMEBOY_COMPONENT_TEXT_DOMAIN="role-smoke"
    return 0
}
SH

cat > "$DEPENDENCY_HELPER" <<'SH'
homeboy_resolve_validation_dependency_paths() {
    return 0
}
SH

cat > "${EXTENSION_DIR}/scripts/lib/runner-steps.sh" <<'SH'
should_run_step() {
    return 0
}
SH

cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$PHPCS_ARGS_FILE"
if [[ "$*" == *"--report=json"* ]]; then
    printf '%s\n' '{"totals":{"errors":0,"warnings":0,"fixable":0},"files":{}}'
fi
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcs"

cat > "${EXTENSION_DIR}/vendor/bin/phpstan" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
count=0
[ -f "$PHPSTAN_CALLS_FILE" ] && count=$(cat "$PHPSTAN_CALLS_FILE")
count=$((count + 1))
printf '%s\n' "$count" > "$PHPSTAN_CALLS_FILE"
printf '%s\n' '{"totals":{"errors":0,"file_errors":0},"files":{},"errors":[]}'
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpstan"

assert_contains() {
    local needle="$1"
    local haystack_file="$2"
    local message="$3"

    if ! grep -F -- "$needle" "$haystack_file" >/dev/null; then
        echo "FAIL: $message" >&2
        echo "Contents of $haystack_file:" >&2
        cat "$haystack_file" >&2
        exit 1
    fi
}

assert_equals() {
    local expected="$1"
    local actual="$2"
    local message="$3"

    if [ "$expected" != "$actual" ]; then
        echo "FAIL: $message" >&2
        echo "Expected: $expected" >&2
        echo "Actual:   $actual" >&2
        exit 1
    fi
}

run_lint_file() {
    local rel_file="$1"

    : > "$PHPCS_ARGS_FILE"
    printf '0\n' > "$PHPSTAN_CALLS_FILE"
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="role-smoke" \
    HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
    HOMEBOY_RUNTIME_DETECT_COMPONENT="$DETECT_COMPONENT_HELPER" \
    HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
    PHPCS_ARGS_FILE="$PHPCS_ARGS_FILE" \
    PHPSTAN_CALLS_FILE="$PHPSTAN_CALLS_FILE" \
    HOMEBOY_SUMMARY_MODE=1 \
    HOMEBOY_LINT_FILE="$rel_file" \
    bash "$RUNNER" >/dev/null
}

run_lint_file 'scoper.inc.php'
assert_equals '' "$(cat "$PHPCS_ARGS_FILE")" 'scoper config skips production PHPCS profile'
assert_equals '0' "$(cat "$PHPSTAN_CALLS_FILE")" 'scoper config skips PHPStan runtime autoload analysis'

run_lint_file 'tools/build-autoloader.php'
assert_contains '--exclude=WordPress.WP.AlternativeFunctions,WordPress.PHP.DevelopmentFunctions,WordPress.Security.EscapeOutput' "$PHPCS_ARGS_FILE" 'tooling role excludes WordPress runtime-only PHPCS sniffs'
assert_equals '0' "$(cat "$PHPSTAN_CALLS_FILE")" 'tooling role skips PHPStan runtime static analysis'

run_lint_file 'tests/BFBConversionUnitTest.php'
assert_equals '0' "$(cat "$PHPSTAN_CALLS_FILE")" 'WordPress PHPUnit test role skips unsupported host PHPStan context'

run_lint_file 'tests/smoke-content-normalization.php'
assert_equals '0' "$(cat "$PHPSTAN_CALLS_FILE")" 'standalone smoke harness role skips unsupported dynamic bootstrap PHPStan context'

echo "WordPress lint role smoke passed"
