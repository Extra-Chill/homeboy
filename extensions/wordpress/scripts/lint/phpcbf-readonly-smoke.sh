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
PHPCS_ARGS_FILE="${TMPDIR}/phpcs-args.txt"
PHPCBF_ARGS_FILE="${TMPDIR}/phpcbf-args.txt"

mkdir -p \
    "${EXTENSION_DIR}/vendor/bin" \
    "${EXTENSION_DIR}/scripts/lib" \
    "${EXTENSION_DIR}/rulesets" \
    "${COMPONENT_DIR}/includes"

touch "${EXTENSION_DIR}/phpcs.xml.dist"
touch "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: PHPCBF Readonly Fixture
 * Text Domain: phpcbf-readonly-fixture
 */
PHP

cat > "${COMPONENT_DIR}/includes/example.php" <<'PHP'
<?php
function phpcbf_readonly_fixture( $value ) {
    return $value ?: 'fallback';
}
PHP

cat > "$RESOLVE_CONTEXT_HELPER" <<'SH'
homeboy_resolve_context() {
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
    COMPONENT_PATH="$HOMEBOY_COMPONENT_PATH"
    PLUGIN_PATH="$HOMEBOY_COMPONENT_PATH"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-phpcbf-readonly-fixture}"
}
SH

cat > "$DETECT_COMPONENT_HELPER" <<'SH'
homeboy_detect_component() {
    HOMEBOY_COMPONENT_TYPE="plugin"
    HOMEBOY_COMPONENT_MAIN_FILE="plugin.php"
    HOMEBOY_COMPONENT_TEXT_DOMAIN="phpcbf-readonly-fixture"
    return 0
}
SH

cat > "${EXTENSION_DIR}/scripts/lib/runner-steps.sh" <<'SH'
should_run_step() {
    [ "${HOMEBOY_STEP:-}" = "phpcs" ] || [ -z "${HOMEBOY_STEP:-}" ]
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

cat > "${EXTENSION_DIR}/vendor/bin/phpcbf" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$PHPCBF_ARGS_FILE"
printf '%s\n' 'PHPCBF fake ran'
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcbf"

: > "$PHPCS_ARGS_FILE"
: > "$PHPCBF_ARGS_FILE"

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpcbf-readonly-fixture" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_DETECT_COMPONENT="$DETECT_COMPONENT_HELPER" \
PHPCS_ARGS_FILE="$PHPCS_ARGS_FILE" \
PHPCBF_ARGS_FILE="$PHPCBF_ARGS_FILE" \
HOMEBOY_STEP=phpcs \
HOMEBOY_SUMMARY_MODE=1 \
    bash "$RUNNER" > "${TMPDIR}/readonly.out" 2>&1

if [ -s "$PHPCBF_ARGS_FILE" ]; then
    echo "FAIL: read-only lint invoked phpcbf" >&2
    sed 's/^/  /' "${TMPDIR}/readonly.out" >&2
    exit 1
fi

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpcbf-readonly-fixture" \
HOMEBOY_RUNTIME_RESOLVE_CONTEXT="$RESOLVE_CONTEXT_HELPER" \
HOMEBOY_RUNTIME_DETECT_COMPONENT="$DETECT_COMPONENT_HELPER" \
PHPCS_ARGS_FILE="$PHPCS_ARGS_FILE" \
PHPCBF_ARGS_FILE="$PHPCBF_ARGS_FILE" \
HOMEBOY_STEP=phpcs \
HOMEBOY_FIX_ONLY=1 \
    bash "$RUNNER" > "${TMPDIR}/fix.out" 2>&1

if [ ! -s "$PHPCBF_ARGS_FILE" ]; then
    echo "FAIL: fix-only lint did not invoke phpcbf" >&2
    sed 's/^/  /' "${TMPDIR}/fix.out" >&2
    exit 1
fi

echo "PHPCBF read-only lint smoke passed"
