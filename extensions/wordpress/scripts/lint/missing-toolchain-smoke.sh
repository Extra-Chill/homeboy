#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
EXTENSION_DIR="${TMP_DIR}/extension"
COMPONENT_DIR="${TMP_DIR}/component"
mkdir -p "${EXTENSION_DIR}/vendor/bin" "${EXTENSION_DIR}/scripts/lint" "${EXTENSION_DIR}/rulesets" "$COMPONENT_DIR"
printf '%s\n' '<?php // Toolchain fixture.' > "${COMPONENT_DIR}/main.php"
touch "${EXTENSION_DIR}/phpstan.neon.dist" "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"
# Keep the real runner's helper resolution while exercising the aggregate callers.
cat > "${EXTENSION_DIR}/scripts/lint/phpstan-runner.sh" <<SH
#!/usr/bin/env bash
exec bash "${SCRIPT_DIR}/phpstan-runner.sh" "\$@"
SH

export HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR"
export HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR"
export HOMEBOY_COMPONENT_ID="missing-toolchain-smoke"
export HOMEBOY_COMPONENT_SHAPE="plugin"
export HOMEBOY_LINT_FILE="main.php"
export HOMEBOY_SKIP="eslint"
export HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="${TMP_DIR}/dependencies.sh"
printf '%s\n' 'homeboy_resolve_validation_dependency_paths() { return 0; }' > "$HOMEBOY_WORDPRESS_DEPENDENCY_HELPER"
# Minimal runtime helpers, like the sibling PHPCS gate smoke: no core checkout required.
export HOMEBOY_RUNTIME_SIDECAR_WRITER="${TMP_DIR}/sidecar-writer.sh"
: > "$HOMEBOY_RUNTIME_SIDECAR_WRITER"
export HOMEBOY_RUNTIME_RESOLVE_CONTEXT="${TMP_DIR}/resolve-context.sh"
cat > "$HOMEBOY_RUNTIME_RESOLVE_CONTEXT" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$COMPONENT_PATH"
}
SH
export HOMEBOY_RUNTIME_RUNNER_PRELUDE="${TMP_DIR}/prelude.sh"
cat > "$HOMEBOY_RUNTIME_RUNNER_PRELUDE" <<'SH'
homeboy_runner_init() {
    source "$HOMEBOY_RUNTIME_RESOLVE_CONTEXT"
    homeboy_resolve_context
}
should_run_step() { [ "$1" != 'eslint' ]; }
SH
export TOOLCHAIN_CALLS="${TMP_DIR}/calls"
OUTPUT="${TMP_DIR}/output"
failures=0

expect_missing() {
    local label="$1" runner="$2"
    if bash "$runner" > "$OUTPUT" 2>&1; then
        echo "FAIL: $label unexpectedly succeeded" >&2
        failures=$((failures + 1))
    fi
    if ! grep -Fq 'homeboy extension setup wordpress' "$OUTPUT"; then
        echo "FAIL: $label did not report the setup command" >&2
        failures=$((failures + 1))
    fi
}

expect_missing 'missing PHPCS' "${SCRIPT_DIR}/lint-runner.sh"
expect_missing 'missing PHPStan' "${SCRIPT_DIR}/phpstan-runner.sh"

cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
printf '%s\n' phpcs >> "$TOOLCHAIN_CALLS"
printf '%s\n' '{"totals":{"errors":0,"warnings":0,"fixable":0},"files":{}}'
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcs"
for mode in 0 1; do
    export HOMEBOY_SUMMARY_MODE="$mode"
    expect_missing "aggregate missing PHPStan (summary=$mode)" "${SCRIPT_DIR}/lint-runner.sh"
done

cat > "${EXTENSION_DIR}/vendor/bin/phpstan" <<'SH'
#!/usr/bin/env bash
if [ "${1:-}" = '--version' ]; then
    echo 'PHPStan 2.0.0'
    exit 0
fi
printf '%s\n' phpstan >> "$TOOLCHAIN_CALLS"
printf '%s\n' '{"totals":{"errors":0,"file_errors":0},"files":{},"errors":[]}'
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpstan"
for mode in 0 1; do
    export HOMEBOY_SUMMARY_MODE="$mode"
    : > "$TOOLCHAIN_CALLS"
    if ! bash "${SCRIPT_DIR}/lint-runner.sh" > "$OUTPUT" 2>&1; then
        echo "FAIL: present toolchain failed (summary=$mode)" >&2
        tail -30 "$OUTPUT" >&2
        failures=$((failures + 1))
    fi
    for tool in phpcs phpstan; do
        if ! grep -Fxq "$tool" "$TOOLCHAIN_CALLS"; then
            echo "FAIL: present $tool was not run (summary=$mode)" >&2
            failures=$((failures + 1))
        fi
    done
done

[ "$failures" -eq 0 ] || exit 1
echo 'WordPress missing toolchain smoke passed'
