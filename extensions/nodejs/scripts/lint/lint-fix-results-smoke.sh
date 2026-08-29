#!/usr/bin/env bash
# Smoke-test Node.js lint fix-results sidecar emission with a project lint:fix script.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${ROOT_DIR}/.." && pwd)/homeboy}"
# Helpers resolve through the shared resolver. The literal path used here
# previously pointed at src/core/extension/runtime, a Homeboy layout that has
# not existed since the move to crates/.
# shellcheck source=/dev/null
source "${ROOT_DIR}/scripts/lib/runtime-helper-resolver.sh"
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)" || exit 1
RUNNER_PRELUDE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_RUNNER_PRELUDE runner-prelude.sh)" || exit 1
COMMAND_CAPTURE_HELPER="$(homeboy_runtime_helper "$ROOT_DIR" HOMEBOY_RUNTIME_COMMAND_CAPTURE command-capture.sh)" || exit 1
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

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

PROJECT_DIR="${TMP_DIR}/project"
RESULTS_FILE="${TMP_DIR}/fix-results.json"
FINDINGS_FILE="${TMP_DIR}/lint-findings.json"
mkdir -p "$PROJECT_DIR"

cat > "${PROJECT_DIR}/package.json" <<'JSON'
{"name":"homeboy-node-fix-results-smoke","scripts":{"lint:fix":"node fix.js"}}
JSON
cat > "${PROJECT_DIR}/fix.js" <<'JS'
const fs = require('node:fs');
fs.writeFileSync('index.js', fs.readFileSync('index.js', 'utf8').replace('before', 'after'));
JS
cat > "${PROJECT_DIR}/index.js" <<'JS'
console.log('before');
JS

git -C "$PROJECT_DIR" init >/dev/null
git -C "$PROJECT_DIR" add package.json fix.js index.js
git -C "$PROJECT_DIR" \
    -c user.name="Homeboy Smoke" \
    -c user.email="homeboy-smoke@example.com" \
    commit -m "fixture" >/dev/null

HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_EXTENSION_PATH="${ROOT_DIR}/nodejs" \
HOMEBOY_COMPONENT_ID="node-fix-results-smoke" \
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_FIX_ONLY=1 \
HOMEBOY_FIX_RESULTS_FILE="$RESULTS_FILE" \
HOMEBOY_LINT_FINDINGS_FILE="$FINDINGS_FILE" \
    bash "${SCRIPT_DIR}/lint-runner.sh" >/dev/null

python3 - "$RESULTS_FILE" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)

expected = [{"file": "index.js", "rule": "nodejs_lint", "action": "rewrite"}]
if data != expected:
    raise SystemExit(f"unexpected fix results: {data!r} != {expected!r}")
PY

echo "Node.js lint fix-results smoke passed"
