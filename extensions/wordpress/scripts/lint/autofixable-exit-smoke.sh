#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component"
OUTPUT_FILE="${TMPDIR}/lint-output.txt"
FINDINGS_FILE="${TMPDIR}/lint-findings.json"

mkdir -p "${EXTENSION_DIR}/vendor/bin" "${COMPONENT_DIR}"
touch "${EXTENSION_DIR}/phpcs.xml.dist"

cat > "${COMPONENT_DIR}/plugin.php" <<'PHP'
<?php
/**
 * Plugin Name: Autofixable Lint Fixture
 * Text Domain: autofixable-lint-fixture
 */

$alpha   = 1;
$beta = 2;
PHP

cat > "${EXTENSION_DIR}/vendor/bin/phpcs" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

for arg in "$@"; do
    if [ "$arg" = "--config-set" ]; then
        exit 0
    fi
done

for arg in "$@"; do
    if [ "$arg" = "--report=json" ]; then
        cat <<'JSON'
{"totals":{"errors":0,"warnings":1,"fixable":1},"files":{"/tmp/component/plugin.php":{"errors":0,"warnings":1,"messages":[{"message":"Equals sign not aligned with surrounding assignments","source":"Generic.Formatting.MultipleStatementAlignment.NotSameWarning","severity":5,"fixable":true,"type":"WARNING","line":8,"column":7}]}}}
JSON
        exit 1
    fi
done

cat <<'TXT'
FILE: plugin.php
----------------------------------------------------------------------
FOUND 0 ERRORS AND 1 WARNING AFFECTING 1 LINE
----------------------------------------------------------------------
 8 | WARNING | Equals sign not aligned with surrounding assignments
----------------------------------------------------------------------
TXT
exit 1
SH
chmod +x "${EXTENSION_DIR}/vendor/bin/phpcs"

run_lint() {
    local mode="$1"
    : > "$OUTPUT_FILE"
    rm -f "$FINDINGS_FILE"

    set +e
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
    HOMEBOY_COMPONENT_ID="autofixable-fixture" \
    HOMEBOY_LINT_FINDINGS_FILE="$FINDINGS_FILE" \
    HOMEBOY_STEP="phpcs" \
    HOMEBOY_SUMMARY_MODE="$mode" \
        "$RUNNER" >"$OUTPUT_FILE" 2>&1
    local exit_code=$?
    set -e

    if [ "$exit_code" -eq 0 ]; then
        echo "FAIL: lint runner should fail when PHPCS reports an auto-fixable warning (summary=${mode})" >&2
        sed 's/^/  /' "$OUTPUT_FILE" >&2
        exit 1
    fi

    if ! grep -Fq "AUTO-FIXABLE: 1 lint finding(s) can be fixed automatically." "$OUTPUT_FILE"; then
        echo "FAIL: auto-fixable CTA missing (summary=${mode})" >&2
        sed 's/^/  /' "$OUTPUT_FILE" >&2
        exit 1
    fi

    if ! grep -Fq "Run:  homeboy refactor --from lint --write autofixable-fixture" "$OUTPUT_FILE"; then
        echo "FAIL: refactor command missing (summary=${mode})" >&2
        sed 's/^/  /' "$OUTPUT_FILE" >&2
        exit 1
    fi

    if ! grep -Fq "Auto-fixable findings remain; run the refactor command above before pushing." "$OUTPUT_FILE"; then
        echo "FAIL: aggregate failure message missing (summary=${mode})" >&2
        sed 's/^/  /' "$OUTPUT_FILE" >&2
        exit 1
    fi

    if ! grep -Fq '"fixable":true' "$FINDINGS_FILE"; then
        echo "FAIL: lint findings sidecar should preserve fixable=true (summary=${mode})" >&2
        [ -f "$FINDINGS_FILE" ] && sed 's/^/  /' "$FINDINGS_FILE" >&2
        exit 1
    fi
}

run_lint 1
run_lint 0

echo "autofixable lint exit smoke passed"
