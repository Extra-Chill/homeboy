#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_DIR}/../.." && pwd)/homeboy}"
REPOSITORY_ROOT="$(cd "${EXTENSION_DIR}/.." && pwd)"
# Helpers resolve through the shared resolver. The literal path used
# here previously pointed at src/core/extension/runtime, a Homeboy layout
# that has not existed since the move to crates/.
# shellcheck source=/dev/null
source "${REPOSITORY_ROOT}/scripts/lib/runtime-helper-resolver.sh"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)" || exit 1
RUNNER_PRELUDE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_RUNNER_PRELUDE runner-prelude.sh)" || exit 1
COMMAND_CAPTURE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_COMMAND_CAPTURE command-capture.sh)" || exit 1
RUNNER="$SCRIPT_DIR/lint-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

if [ ! -f "$SIDECAR_WRITER_HELPER" ]; then
    echo "Missing sidecar writer helper: $SIDECAR_WRITER_HELPER" >&2
    exit 1
fi
if [ ! -f "$RUNNER_PRELUDE_HELPER" ]; then
    echo "Missing runner prelude helper: $RUNNER_PRELUDE_HELPER" >&2
    exit 1
fi
if [ ! -f "$COMMAND_CAPTURE_HELPER" ]; then
    echo "Missing command capture helper: $COMMAND_CAPTURE_HELPER" >&2
    exit 1
fi

PROJECT_PATH="$TMPDIR/project"
FINDINGS_FILE="$TMPDIR/lint-findings.json"
OUTPUT_FILE="$TMPDIR/lint.out"
mkdir -p "$PROJECT_PATH/node_modules/.bin" "$PROJECT_PATH/src"
printf '{"name":"node-lint-sidecar-smoke","scripts":{}}\n' > "$PROJECT_PATH/package.json"
printf 'export const value = 1\n' > "$PROJECT_PATH/src/bad.js"
printf 'export default []\n' > "$PROJECT_PATH/eslint.config.js"

cat > "$PROJECT_PATH/node_modules/.bin/eslint" <<'SH'
#!/usr/bin/env bash
project_path="$(pwd)"
printf '[{"filePath":"%s/src/bad.js","messages":[{"ruleId":"semi","message":"Missing semicolon.","line":1,"column":23,"severity":2,"fix":{"range":[22,22],"text":";"}}]}]\n' "$project_path"
exit 1
SH
chmod +x "$PROJECT_PATH/node_modules/.bin/eslint"

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_PATH" \
HOMEBOY_COMPONENT_ID="node-lint-sidecar-smoke" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_LINT_FINDINGS_FILE="$FINDINGS_FILE" \
    bash "$RUNNER" >"$OUTPUT_FILE" 2>&1
status=$?
set -e

if [ "$status" -eq 0 ]; then
    echo "Expected lint runner to fail on fake ESLint finding" >&2
    cat "$OUTPUT_FILE" >&2
    exit 1
fi

node - "$FINDINGS_FILE" <<'NODE'
const fs = require('node:fs')
const findings = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'))
if (findings.length !== 1) throw new Error(`expected one finding, got ${findings.length}`)
const finding = findings[0]
for (const key of ['id', 'file', 'line', 'column', 'severity', 'source', 'code', 'message', 'category', 'fixable', 'fingerprint', 'excerpt']) {
  if (!(key in finding)) throw new Error(`missing ${key}: ${JSON.stringify(finding)}`)
}
if (finding.file !== 'src/bad.js') throw new Error(`unexpected file: ${finding.file}`)
if (finding.line !== 1 || finding.column !== 23) throw new Error(`unexpected location: ${finding.line}:${finding.column}`)
if (finding.severity !== 'error' || finding.category !== 'error') throw new Error(`unexpected severity: ${finding.severity}/${finding.category}`)
if (finding.source !== 'eslint' || finding.code !== 'semi') throw new Error(`unexpected source/code: ${finding.source}/${finding.code}`)
if (finding.fixable !== true) throw new Error('expected fixable=true')
if (!/^[a-f0-9]{40}$/.test(finding.fingerprint)) throw new Error(`bad fingerprint: ${finding.fingerprint}`)
if (finding.excerpt !== 'export const value = 1') throw new Error(`unexpected excerpt: ${finding.excerpt}`)
NODE

echo "nodejs lint findings sidecar smoke passed"
