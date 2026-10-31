#!/usr/bin/env bash
# ESLint's "File ignored because of a matching ignore pattern" notice for an
# explicitly passed path is not a lint finding (Extra-Chill/homeboy#16123).
# Real findings and "no matching configuration" notices stay.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REAL_EXTENSION_PATH="$(cd "${SCRIPT_DIR}/../.." && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
# shellcheck source=../../../scripts/lib/runtime-helper-resolver.sh
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT
EXTENSION_PATH="${TMPDIR}/extension"

component_dir="${TMPDIR}/component"
findings_file="${TMPDIR}/eslint-findings.json"
mkdir -p "${component_dir}/tests/js" "${component_dir}/src" "${EXTENSION_PATH}/node_modules/.bin"
touch "${EXTENSION_PATH}/eslint.runner.config.mjs"
printf '%s\n' 'test( "x", () => {} );' > "${component_dir}/tests/js/render-order.test.js"
printf '%s\n' 'const answer = 42' > "${component_dir}/src/app.js"
printf '%s\n' 'export const View = () => null;' > "${component_dir}/src/view.jsx"

# Stands in for ESLint 9: the JSON report it writes for an explicitly passed
# ignored file, a real rule violation, and an unconfigured file.
cat > "${EXTENSION_PATH}/node_modules/.bin/eslint" <<SH
#!/usr/bin/env bash
cat <<'JSON'
[{"filePath":"${component_dir}/tests/js/render-order.test.js","messages":[{"ruleId":null,"fatal":false,"severity":1,"message":"File ignored because of a matching ignore pattern. Use \"--no-ignore\" to disable file ignore settings or use \"--no-warn-ignored\" to suppress this warning."}],"errorCount":0,"warningCount":1,"fixableErrorCount":0,"fixableWarningCount":0},
{"filePath":"${component_dir}/src/app.js","messages":[{"ruleId":"semi","severity":2,"message":"Missing semicolon.","line":1,"column":18}],"errorCount":1,"warningCount":0,"fixableErrorCount":0,"fixableWarningCount":0},
{"filePath":"${component_dir}/src/view.jsx","messages":[{"ruleId":null,"fatal":false,"severity":1,"message":"File ignored because no matching configuration was supplied."}],"errorCount":0,"warningCount":1,"fixableErrorCount":0,"fixableWarningCount":0}]
JSON
SH
chmod +x "${EXTENSION_PATH}/node_modules/.bin/eslint"

HOMEBOY_EXTENSION_PATH="$EXTENSION_PATH" \
HOMEBOY_COMPONENT_ID="component" \
HOMEBOY_COMPONENT_PATH="$component_dir" \
HOMEBOY_COMPONENT_TEXT_DOMAIN="component" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_LINT_GLOB="{${component_dir}/tests/js/render-order.test.js,${component_dir}/src/app.js,${component_dir}/src/view.jsx}" \
HOMEBOY_LINT_FINDINGS_FILE="$findings_file" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
    bash "${REAL_EXTENSION_PATH}/scripts/lint/eslint-runner.sh" > "${TMPDIR}/run.out" 2>&1 || true

python3 - "$findings_file" "${TMPDIR}/run.out" <<'PY'
import json
import sys

findings = json.load(open(sys.argv[1], encoding="utf-8"))
files = sorted(finding["file"] for finding in findings)
assert files == ["src/app.js", "src/view.jsx"], (files, open(sys.argv[2]).read())
assert not any("matching ignore pattern" in finding["message"] for finding in findings), findings
PY

echo "ESLint ignored-file notice smoke passed"
