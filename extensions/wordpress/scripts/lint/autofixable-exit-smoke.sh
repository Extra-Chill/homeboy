#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
RUNNER="${SCRIPT_DIR}/lint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

if [ ! -f "$SIDECAR_WRITER_HELPER" ]; then
    echo "Missing sidecar writer helper: $SIDECAR_WRITER_HELPER" >&2
    exit 1
fi

EXTENSION_DIR="${TMPDIR}/extension"
COMPONENT_DIR="${TMPDIR}/component"
OUTPUT_FILE="${TMPDIR}/lint-output.txt"
FINDINGS_FILE="${TMPDIR}/lint-findings.json"

mkdir -p "${EXTENSION_DIR}/vendor/bin" "${EXTENSION_DIR}/rulesets" "${COMPONENT_DIR}"
touch "${EXTENSION_DIR}/phpcs.xml.dist"
touch "${EXTENSION_DIR}/rulesets/homeboy-wordpress-project.xml"

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
        printf '{"totals":{"errors":0,"warnings":1,"fixable":1},"files":{"%s":{"errors":0,"warnings":1,"messages":[{"message":"Equals sign not aligned with surrounding assignments","source":"Generic.Formatting.MultipleStatementAlignment.NotSameWarning","severity":5,"fixable":true,"type":"WARNING","line":8,"column":7}]}}}\n' "${COMPONENT_PATH}/plugin.php"
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
    local changed_since="${2:-}"
    : > "$OUTPUT_FILE"
    rm -f "$FINDINGS_FILE"

    local -a env_args=(
        "HOMEBOY_EXTENSION_PATH=$EXTENSION_DIR"
        "HOMEBOY_COMPONENT_PATH=$COMPONENT_DIR"
        "HOMEBOY_COMPONENT_ID=autofixable-fixture"
        "HOMEBOY_RUNTIME_SIDECAR_WRITER=$SIDECAR_WRITER_HELPER"
        "HOMEBOY_LINT_FINDINGS_FILE=$FINDINGS_FILE"
        "HOMEBOY_RUNTIME_SIDECAR_WRITER=$SIDECAR_WRITER_HELPER"
        "HOMEBOY_STEP=phpcs"
        "HOMEBOY_SUMMARY_MODE=$mode"
    )
    if [ -n "$changed_since" ]; then
        env_args+=("HOMEBOY_CHANGED_SINCE=$changed_since")
    fi

    set +e
    env "${env_args[@]}" "$RUNNER" >"$OUTPUT_FILE" 2>&1
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

    expected_command="Run:  homeboy refactor --from lint --write"
    if [ -n "$changed_since" ]; then
        expected_command="${expected_command} --changed-since ${changed_since}"
    fi
    expected_command="${expected_command} autofixable-fixture"

    if ! grep -Fq "$expected_command" "$OUTPUT_FILE"; then
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

    python3 - "$FINDINGS_FILE" <<'PY'
import json
import sys

findings = json.load(open(sys.argv[1], encoding="utf-8"))
assert len(findings) == 1, findings
finding = findings[0]
expected = {
    "tool": "phpcs",
    "file": "plugin.php",
    "line": 8,
    "column": 7,
    "severity": "warning",
    "code": "Generic.Formatting.MultipleStatementAlignment.NotSameWarning",
    "rule": "Generic.Formatting.MultipleStatementAlignment.NotSameWarning",
    "category": "formatting",
    "fixable": True,
}
for key, value in expected.items():
    assert finding.get(key) == value, (key, finding)
assert "source" not in finding, finding
assert finding.get("fingerprint"), finding
assert finding.get("excerpt") == "$beta = 2;", finding
PY
}

run_lint 1
run_lint 0
run_lint 1 origin/main

echo "autofixable lint exit smoke passed"
