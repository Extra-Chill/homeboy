#!/usr/bin/env bash
set -euo pipefail

# Regression smoke for homeboy-extensions#2234.
#
# The wordpress lint gate used to decide the PHPCS step pass/fail from the
# phpcs exit code, which is non-zero for warnings even when there are zero
# errors. That blocked `homeboy release` preflight on error-clean repos and
# pushed operators to `--skip-checks=lint`, which threw away the error gate.
#
# Fix: pass `--runtime-set ignore_warnings_on_exit 1` (default), so phpcs exits
# 0 on warnings-only and the exit code becomes an errors-only gate. Warnings are
# still reported in the summary and findings. HOMEBOY_LINT_FAIL_ON=warnings
# restores the legacy block-on-warning behavior.
#
# This smoke asserts across both lint modes (summary + full) and both fixtures:
#   - warnings-only, default    => exit 0 AND "PHPCS SUMMARY: 0 errors, 3 warnings" reported
#   - warnings-only, fail-on=warnings => exit non-zero (legacy)
#   - errors present (any knob) => exit non-zero (gate still blocks on errors)
#
# Self-contained: stubs the runner-prelude/steps/resolve-context/sidecar-writer
# helpers so it runs without a homeboy core checkout alongside.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component"
PRELUDE_HELPER="${TMPDIR}/runner-prelude.sh"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context.sh"
DETECT_COMPONENT_HELPER="${TMPDIR}/detect-component.sh"
RUNNER_STEPS_HELPER="${TMPDIR}/runner-steps.sh"
SIDECAR_WRITER_HELPER="${TMPDIR}/sidecar-writer.sh"
OUTPUT_FILE="${TMPDIR}/lint-output.txt"

mkdir -p \
    "${EXTENSION_DIR}/vendor/bin" \
    "${EXTENSION_DIR}/scripts/lint" \
    "${COMPONENT_DIR}"

touch "${EXTENSION_DIR}/phpcs.xml.dist" "${EXTENSION_DIR}/phpstan.neon.dist"
mkdir -p "${EXTENSION_DIR}/rulesets"
touch "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: PHPCS Warnings Gate Fixture
 * Text Domain: phpcs-warnings-gate-fixture
 */

$alpha = 1;
PHP

# Minimal stand-in for homeboy core's runner-prelude. The harness sources this
# then calls homeboy_runner_init, which must source the steps/resolve-context/
# sidecar-writer helpers and resolve the component context.
cat > "$PRELUDE_HELPER" <<'SH'
homeboy_runner_init() {
    local _h
    for _h in "${HOMEBOY_RUNTIME_RUNNER_STEPS:-}" \
              "${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-}" \
              "${HOMEBOY_RUNTIME_SIDECAR_WRITER:-}"; do
        [ -n "$_h" ] && [ -f "$_h" ] && source "$_h"
    done
    if declare -F homeboy_resolve_context >/dev/null 2>&1; then
        homeboy_resolve_context --component-alias PLUGIN_PATH
    fi
    return 0
}
SH

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-phpcs-warnings-gate}"
}
SH

cat > "$DETECT_COMPONENT_HELPER" <<'SH'
homeboy_detect_component() {
    HOMEBOY_COMPONENT_TYPE="plugin"
    HOMEBOY_COMPONENT_MAIN_FILE="plugin.php"
    HOMEBOY_COMPONENT_TEXT_DOMAIN="phpcs-warnings-gate-fixture"
    return 0
}
SH

cat > "$RUNNER_STEPS_HELPER" <<'SH'
should_run_step() {
    return 0
}
SH

cat > "$SIDECAR_WRITER_HELPER" <<'SH'
homeboy_sidecar_merge() {
    return 0
}
SH

# Stubbed phpcs that models real PHPCS exit-code semantics.
# PHPCS_FIXTURE_MODE controls the report shape:
#   warnings — 0 errors, 3 warnings (the #2234 scenario)
#   error    — 2 errors, 0 warnings (gate must always block)
# Exit code follows real phpcs: errors => 1; warnings-only => 1 unless
# `ignore_warnings_on_exit 1` was passed => 0; clean => 0.
cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

# `--config-set installed_paths` (runner bootstrap) is a no-op for the stub.
for arg in "$@"; do
    if [ "$arg" = "--config-set" ]; then
        exit 0
    fi
done

want_json=0
ignore_warnings=0
for arg in "$@"; do
    [ "$arg" = "--report=json" ] && want_json=1
    # `--runtime-set ignore_warnings_on_exit 1` arrives as the next token after
    # `--runtime-set`; detect the config key itself.
    [ "$arg" = "ignore_warnings_on_exit" ] && ignore_warnings=1
done

mode="${PHPCS_FIXTURE_MODE:-warnings}"
errors=0
warnings=0
case "$mode" in
    warnings) errors=0; warnings=3 ;;
    error)    errors=2; warnings=0 ;;
    *) echo "unknown PHPCS_FIXTURE_MODE: $mode" >&2; exit 2 ;;
esac

component="${COMPONENT_PATH:-/tmp}"
if [ "$want_json" -eq 1 ]; then
    if [ "$errors" -gt 0 ]; then
        printf '{"totals":{"errors":%d,"warnings":%d,"fixable":0},"files":{"%s/plugin.php":{"errors":%d,"warnings":%d,"messages":[{"type":"ERROR","source":"WordPress.Security.EscapeOutput","message":"Esc output","line":7,"column":1,"fixable":false}]}}}\n' \
            "$errors" "$warnings" "$component" "$errors" "$warnings"
    else
        printf '{"totals":{"errors":0,"warnings":%d,"fixable":0},"files":{"%s/plugin.php":{"errors":0,"warnings":%d,"messages":[{"type":"WARNING","source":"WordPress.WhiteSpace.ControlStructureSpacing","message":"Spacing nit","line":7,"column":1,"fixable":false}]}}}\n' \
            "$warnings" "$component" "$warnings"
    fi
else
    echo "FOUND ${errors} ERROR(S), ${warnings} WARNING(S)"
fi

# Real phpcs exit code semantics.
if [ "$errors" -gt 0 ]; then
    exit 1
elif [ "$warnings" -gt 0 ]; then
    if [ "$ignore_warnings" -eq 1 ]; then
        exit 0
    fi
    exit 1
fi
exit 0
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcs"

# ESLint + PHPStan stubs pass by default, so the six PHPCS cases remain isolated.
cat > "${EXTENSION_DIR}/scripts/lint/eslint-runner.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${ESLINT_FIXTURE_MODE:-pass}" = "error" ]; then
    echo "ESLINT SUMMARY: 1 errors, 0 warnings"
    exit 1
fi
echo "ESLint linting passed"
exit 0
SH
chmod +x "${EXTENSION_DIR}/scripts/lint/eslint-runner.sh"

cat > "${EXTENSION_DIR}/scripts/lint/phpstan-runner.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${PHPSTAN_FIXTURE_MODE:-pass}" = "error" ]; then
    echo "PHPSTAN SUMMARY: 2 errors at level 7"
    exit 1
fi
echo "PHPStan linting passed"
exit 0
SH
chmod +x "${EXTENSION_DIR}/scripts/lint/phpstan-runner.sh"

fail=0

run_lint() {
    local mode="$1"
    local fail_on="${2:-}"
    local summary="${3:-0}"
    : > "$OUTPUT_FILE"

    local -a env_args=(
        "HOMEBOY_EXTENSION_PATH=$EXTENSION_DIR"
        "HOMEBOY_COMPONENT_PATH=$COMPONENT_DIR"
        "HOMEBOY_COMPONENT_ID=phpcs-warnings-gate"
        "HOMEBOY_RUNTIME_RUNNER_PRELUDE=$PRELUDE_HELPER"
        "HOMEBOY_RUNTIME_RESOLVE_CONTEXT=$RESOLVE_CONTEXT_HELPER"
        "HOMEBOY_RUNTIME_DETECT_COMPONENT=$DETECT_COMPONENT_HELPER"
        "HOMEBOY_RUNTIME_RUNNER_STEPS=$RUNNER_STEPS_HELPER"
        "HOMEBOY_RUNTIME_SIDECAR_WRITER=$SIDECAR_WRITER_HELPER"
        "HOMEBOY_STEP=phpcs"
        "PHPCS_FIXTURE_MODE=$mode"
    )
    [ -n "$fail_on" ] && env_args+=("HOMEBOY_LINT_FAIL_ON=$fail_on")
    [ "$summary" = "1" ] && env_args+=("HOMEBOY_SUMMARY_MODE=1")

    set +e
    env "${env_args[@]}" bash "$RUNNER" >"$OUTPUT_FILE" 2>&1
    LINT_EXIT=$?
    set -e
}

assert_exit() {
    local label="$1"
    local expected="$2"
    if [ "$LINT_EXIT" -ne "$expected" ]; then
        echo "FAIL: ${label}: expected exit ${expected}, got ${LINT_EXIT}" >&2
        sed 's/^/  /' "$OUTPUT_FILE" >&2
        fail=1
    else
        echo "ok: ${label} (exit ${LINT_EXIT})"
    fi
}

assert_contains() {
    local label="$1"
    local needle="$2"
    if ! grep -Fq -- "$needle" "$OUTPUT_FILE"; then
        echo "FAIL: ${label}: expected output to contain: ${needle}" >&2
        sed 's/^/  /' "$OUTPUT_FILE" >&2
        fail=1
    fi
}

assert_no_generic_summary() {
    local label="$1"
    if grep -Eq '^LINT SUMMARY:' "$OUTPUT_FILE"; then
        echo "FAIL: ${label}: generic LINT SUMMARY label found" >&2
        sed 's/^/  /' "$OUTPUT_FILE" >&2
        fail=1
    fi
}

# 1. warnings-only, default knob, SUMMARY mode => exit 0, warning still reported.
run_lint warnings "" 1
assert_exit "warnings-only / default / summary mode" 0
assert_contains "warnings-only / default / summary mode" "PHPCS SUMMARY: 0 errors, 3 warnings"

# 2. warnings-only, default knob, FULL mode => exit 0, warning still reported.
run_lint warnings "" 0
assert_exit "warnings-only / default / full mode" 0
assert_contains "warnings-only / default / full mode" "PHPCS SUMMARY: 0 errors, 3 warnings"

# 3. warnings-only, HOMEBOY_LINT_FAIL_ON=warnings, SUMMARY mode => exit 1 (legacy).
run_lint warnings warnings 1
assert_exit "warnings-only / fail_on=warnings / summary mode" 1

# 4. warnings-only, HOMEBOY_LINT_FAIL_ON=warnings, FULL mode => exit 1 (legacy).
run_lint warnings warnings 0
assert_exit "warnings-only / fail_on=warnings / full mode" 1

# 5. errors present, default knob, SUMMARY mode => exit 1 (gate still blocks).
run_lint error "" 1
assert_exit "errors / default / summary mode" 1
assert_contains "errors / default / summary mode" "PHPCS SUMMARY: 2 errors, 0 warnings"

# 6. errors present, default knob, FULL mode => exit 1 (gate still blocks).
run_lint error "" 0
assert_exit "errors / default / full mode" 1

# 7. PHPCS passes its warnings-only gate while ESLint and PHPStan fail aggregate lint.
: > "$OUTPUT_FILE"
set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpcs-warnings-gate" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$PRELUDE_HELPER" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_DETECT_COMPONENT="$DETECT_COMPONENT_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="$RUNNER_STEPS_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
PHPCS_FIXTURE_MODE=warnings \
ESLINT_FIXTURE_MODE=error \
PHPSTAN_FIXTURE_MODE=error \
HOMEBOY_SUMMARY_MODE=1 \
bash "$RUNNER" >"$OUTPUT_FILE" 2>&1
LINT_EXIT=$?
set -e

assert_exit "producer labels / aggregate summary mode" 1
assert_contains "producer labels / aggregate summary mode" "PHPCS SUMMARY: 0 errors, 3 warnings"
assert_contains "producer labels / aggregate summary mode" "ESLINT SUMMARY: 1 errors, 0 warnings"
assert_contains "producer labels / aggregate summary mode" "PHPSTAN SUMMARY: 2 errors at level 7"
assert_no_generic_summary "producer labels / aggregate summary mode"

if [ "$fail" -ne 0 ]; then
    echo "phpcs-warnings-gate lint smoke FAILED" >&2
    exit 1
fi

echo "phpcs-warnings-gate lint smoke passed"
