#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component fixture"
PRELUDE_HELPER="${TMPDIR}/runner-prelude.sh"
DETECT_COMPONENT_HELPER="${TMPDIR}/detect-component.sh"
PHPCS_ARGS_FILE="${TMPDIR}/phpcs-args.txt"
PHPCBF_ARGS_FILE="${TMPDIR}/phpcbf-args.txt"
PHPCS_TOOL_FILE="${TMPDIR}/phpcs-tool.txt"
PHPCBF_TOOL_FILE="${TMPDIR}/phpcbf-tool.txt"
UNUSED_ARGS_FILE="${TMPDIR}/unused-args.txt"
IGNORE_ARGS_FILE="${TMPDIR}/ignore-args.txt"
OUTPUT_FILE="${TMPDIR}/output.txt"

mkdir -p \
    "${EXTENSION_DIR}/vendor/bin" \
    "${EXTENSION_DIR}/vendor/wp-coding-standards/wpcs" \
    "${EXTENSION_DIR}/scripts/lint/php-fixers" \
    "${EXTENSION_DIR}/rulesets" \
    "${COMPONENT_DIR}/vendor/bin"

touch "${EXTENSION_DIR}/phpcs.xml.dist"
touch "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: PHPCS Config Selection Fixture
 * Text Domain: phpcs-config-selection-fixture
 */
PHP

cat > "$PRELUDE_HELPER" <<'SH'
homeboy_runner_init() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="phpcs-config-selection-fixture"
}

should_run_step() {
    [ "$1" = "phpcs" ]
}
SH

cat > "$DETECT_COMPONENT_HELPER" <<'SH'
homeboy_detect_component() {
    HOMEBOY_COMPONENT_TYPE="plugin"
    HOMEBOY_COMPONENT_MAIN_FILE="plugin.php"
    HOMEBOY_COMPONENT_TEXT_DOMAIN="phpcs-config-selection-fixture"
    return 0
}
SH

cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' extension >> "$PHPCS_TOOL_FILE"
printf '%s\n' "$@" >> "$PHPCS_ARGS_FILE"
for arg in "$@"; do
    if [ "$arg" = "--report=json" ]; then
        printf '%s\n' '{"totals":{"errors":0,"warnings":0,"fixable":0},"files":{}}'
    fi
done
SH

cat > "${COMPONENT_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' component >> "$PHPCS_TOOL_FILE"
printf '%s\n' "$@" >> "$PHPCS_ARGS_FILE"
for arg in "$@"; do
    if [ "$arg" = "--report=json" ]; then
        printf '%s\n' '{"totals":{"errors":0,"warnings":0,"fixable":0},"files":{}}'
    fi
done
SH

cat > "${EXTENSION_DIR}/vendor/bin/phpcbf" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' extension >> "$PHPCBF_TOOL_FILE"
printf '%s\n' "$@" >> "$PHPCBF_ARGS_FILE"
SH

cat > "${COMPONENT_DIR}/vendor/bin/phpcbf" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' component >> "$PHPCBF_TOOL_FILE"
printf '%s\n' "$@" >> "$PHPCBF_ARGS_FILE"
SH

cat > "${EXTENSION_DIR}/scripts/lint/php-fixers/unused-param-fixer.php" <<'PHP'
<?php
file_put_contents(getenv('UNUSED_ARGS_FILE'), implode("\n", array_slice($argv, 1)) . "\n", FILE_APPEND);
PHP

cat > "${EXTENSION_DIR}/scripts/lint/php-fixers/phpcs-ignore-fixer.php" <<'PHP'
<?php
file_put_contents(getenv('IGNORE_ARGS_FILE'), implode("\n", array_slice($argv, 1)) . "\n", FILE_APPEND);
PHP

chmod +x \
    "${EXTENSION_DIR}/vendor/bin/phpcs" \
    "${EXTENSION_DIR}/vendor/bin/phpcbf" \
    "${COMPONENT_DIR}/vendor/bin/phpcs" \
    "${COMPONENT_DIR}/vendor/bin/phpcbf"

run_lint() {
    local mode="$1"
    : > "$PHPCS_ARGS_FILE"
    : > "$PHPCBF_ARGS_FILE"
    : > "$PHPCS_TOOL_FILE"
    : > "$PHPCBF_TOOL_FILE"
    : > "$UNUSED_ARGS_FILE"
    : > "$IGNORE_ARGS_FILE"

    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_DETECT_COMPONENT="$DETECT_COMPONENT_HELPER" \
    HOMEBOY_COMPONENT_SHAPE="plugin" \
    HOMEBOY_STEP="phpcs" \
    HOMEBOY_FIX_ONLY="$mode" \
    PHPCS_ARGS_FILE="$PHPCS_ARGS_FILE" \
    PHPCBF_ARGS_FILE="$PHPCBF_ARGS_FILE" \
    PHPCS_TOOL_FILE="$PHPCS_TOOL_FILE" \
    PHPCBF_TOOL_FILE="$PHPCBF_TOOL_FILE" \
    UNUSED_ARGS_FILE="$UNUSED_ARGS_FILE" \
    IGNORE_ARGS_FILE="$IGNORE_ARGS_FILE" \
        bash "$RUNNER" > "$OUTPUT_FILE" 2>&1
}

assert_exact_arg() {
    local args_file="$1"
    local expected="$2"
    local consumer="$3"
    if ! grep -Fxq -- "$expected" "$args_file"; then
        printf 'FAIL: %s did not receive argument <%s>\n' "$consumer" "$expected" >&2
        sed 's/^/  /' "$OUTPUT_FILE" >&2
        exit 1
    fi
}

assert_tool() {
    local tool_file="$1"
    local expected="$2"
    local consumer="$3"
    if ! grep -Fxq "$expected" "$tool_file" || grep -Fvxq "$expected" "$tool_file"; then
        printf 'FAIL: %s did not use the %s binary\n' "$consumer" "$expected" >&2
        sed 's/^/  /' "$OUTPUT_FILE" >&2
        exit 1
    fi
}

assert_fix_consumers() {
    local expected_config="$1"
    local expected_phpcs_bin="$2"
    local expected_tool="$3"

    run_lint 1
    assert_tool "$PHPCBF_TOOL_FILE" "$expected_tool" phpcbf
    assert_exact_arg "$PHPCBF_ARGS_FILE" "--standard=${expected_config}" phpcbf
    for fixer_args in "$UNUSED_ARGS_FILE" "$IGNORE_ARGS_FILE"; do
        assert_exact_arg "$fixer_args" "--phpcs-binary=${expected_phpcs_bin}" "custom fixer"
        assert_exact_arg "$fixer_args" "--phpcs-standard=${expected_config}" "custom fixer"
    done
}

rulesets=(phpcs.xml .phpcs.xml phpcs.xml.dist .phpcs.xml.dist)
for ruleset in "${rulesets[@]}"; do
    touch "${COMPONENT_DIR}/${ruleset}"
done

for ruleset in "${rulesets[@]}"; do
    expected_config="${COMPONENT_DIR}/${ruleset}"
    expected_phpcs_bin="${COMPONENT_DIR}/vendor/bin/phpcs"

    run_lint 0
    assert_tool "$PHPCS_TOOL_FILE" component phpcs
    assert_exact_arg "$PHPCS_ARGS_FILE" "--standard=${expected_config}" phpcs
    if grep -Fxq -- "--config-set" "$PHPCS_ARGS_FILE"; then
        echo "FAIL: component phpcs installed_paths were overwritten" >&2
        exit 1
    fi

    assert_fix_consumers "$expected_config" "$expected_phpcs_bin" component
    rm "${COMPONENT_DIR}/${ruleset}"
done

# A component ruleset does not require component Composer binaries. When either
# executable is unavailable, that tool independently falls back to the extension.
touch "${COMPONENT_DIR}/phpcs.xml"
chmod -x "${COMPONENT_DIR}/vendor/bin/phpcs" "${COMPONENT_DIR}/vendor/bin/phpcbf"
expected_config="${COMPONENT_DIR}/phpcs.xml"
expected_phpcs_bin="${EXTENSION_DIR}/vendor/bin/phpcs"
run_lint 0
assert_tool "$PHPCS_TOOL_FILE" extension "phpcs executable fallback"
assert_exact_arg "$PHPCS_ARGS_FILE" "--config-set" "extension phpcs self-healing"
assert_exact_arg "$PHPCS_ARGS_FILE" "--standard=${expected_config}" "phpcs executable fallback"
assert_fix_consumers "$expected_config" "$expected_phpcs_bin" extension
rm "${COMPONENT_DIR}/phpcs.xml"

# No component ruleset at all: fall back to the shipped consumer ruleset
# (rulesets/homeboy-wordpress-project.xml), not this extension's own
# phpcs.xml.dist — see lint-runner.sh for why (homeboy-extensions#2797).
expected_config="${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"
expected_phpcs_bin="${EXTENSION_DIR}/vendor/bin/phpcs"
run_lint 0
assert_tool "$PHPCS_TOOL_FILE" extension phpcs
assert_exact_arg "$PHPCS_ARGS_FILE" "--config-set" "extension phpcs self-healing"
assert_exact_arg "$PHPCS_ARGS_FILE" "--standard=${expected_config}" "phpcs fallback"
assert_fix_consumers "$expected_config" "$expected_phpcs_bin" extension

echo "PHPCS config selection smoke passed"
