#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component"
RESOLVE_CONTEXT_HELPER="${TMPDIR}/resolve-context.sh"
DETECT_COMPONENT_HELPER="${TMPDIR}/detect-component.sh"
DEPENDENCY_HELPER="${TMPDIR}/validation-dependencies.sh"
RUNNER_STEPS_HELPER="${TMPDIR}/runner-steps.sh"
SIDECAR_WRITER_HELPER="${TMPDIR}/sidecar-writer.sh"
OUTPUT_FILE="${TMPDIR}/lint-output.txt"

mkdir -p \
    "${EXTENSION_DIR}/vendor/bin" \
    "${EXTENSION_DIR}/scripts/lint" \
    "${COMPONENT_DIR}"

touch "${EXTENSION_DIR}/phpcs.xml.dist" "${EXTENSION_DIR}/phpstan.neon.dist"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
echo $undefined;
PHP

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-lint-summary-smoke}"
}
SH

cat > "$DETECT_COMPONENT_HELPER" <<'SH'
homeboy_detect_component() {
    HOMEBOY_COMPONENT_TYPE="plugin"
    HOMEBOY_COMPONENT_MAIN_FILE="plugin.php"
    HOMEBOY_COMPONENT_TEXT_DOMAIN="lint-summary-smoke"
    return 0
}
SH

cat > "$DEPENDENCY_HELPER" <<'SH'
homeboy_resolve_validation_dependency_paths() {
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

cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == *"--report=json"* ]]; then
    printf '%s\n' '{"totals":{"errors":0,"warnings":1,"fixable":0},"files":{"/tmp/plugin.php":{"errors":0,"warnings":1,"messages":[{"type":"WARNING","source":"WordPress.WhiteSpace.ControlStructureSpacing","message":"Spacing issue","line":1,"column":1,"fixable":false}]}}}'
fi
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcs"

cat > "${EXTENSION_DIR}/scripts/lint/eslint-runner.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "ESLINT SUMMARY: 1 errors, 0 warnings"
exit 1
SH
chmod +x "${EXTENSION_DIR}/scripts/lint/eslint-runner.sh"

cat > "${EXTENSION_DIR}/scripts/lint/phpstan-runner.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "PHPSTAN SUMMARY: 2 errors at level 7"
exit 1
SH
chmod +x "${EXTENSION_DIR}/scripts/lint/phpstan-runner.sh"

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="lint-summary-smoke" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_DETECT_COMPONENT="$DETECT_COMPONENT_HELPER" \
HOMEBOY_WORDPRESS_DEPENDENCY_HELPER="$DEPENDENCY_HELPER" \
HOMEBOY_RUNTIME_RUNNER_STEPS="$RUNNER_STEPS_HELPER" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_SUMMARY_MODE=1 \
bash "$RUNNER" >"$OUTPUT_FILE" 2>&1
status=$?
set -e

if [ "$status" -eq 0 ]; then
    echo "FAIL: lint runner should fail when ESLint and PHPStan fail" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

if grep -E '^LINT SUMMARY:' "$OUTPUT_FILE" >/dev/null; then
    echo "FAIL: PHPCS-only output must not use the generic LINT SUMMARY label" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

for expected in \
    'PHPCS SUMMARY: 0 errors, 1 warnings' \
    'ESLINT SUMMARY: 1 errors, 0 warnings' \
    'PHPSTAN SUMMARY: 2 errors at level 7'; do
    if ! grep -F -- "$expected" "$OUTPUT_FILE" >/dev/null; then
        echo "FAIL: missing expected summary: $expected" >&2
        cat "$OUTPUT_FILE" >&2
        exit 1
    fi
done

echo "WordPress lint summary label smoke passed"
