#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOMEBOY_CORE_DIR="${HOMEBOY_CORE_DIR:-$(cd "${EXTENSION_DIR}/../.." && pwd)/homeboy}"
REPOSITORY_ROOT="$(cd "${EXTENSION_DIR}/.." && pwd)"
# Helpers resolve through the shared resolver. The literal path used
# here previously pointed at src/core/extension/runtime, a Homeboy layout
# that has not existed since the move to crates/.
# shellcheck source=/dev/null
source "${REPOSITORY_ROOT}/scripts/lib/runtime-helper-resolver.sh"
RUNNER_PRELUDE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_RUNNER_PRELUDE runner-prelude.sh)" || exit 1
COMMAND_CAPTURE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_COMMAND_CAPTURE command-capture.sh)" || exit 1

if [ ! -f "$RUNNER_PRELUDE_HELPER" ]; then
    echo "Missing runner prelude helper: $RUNNER_PRELUDE_HELPER" >&2
    exit 1
fi
if [ ! -f "$COMMAND_CAPTURE_HELPER" ]; then
    echo "Missing command capture helper: $COMMAND_CAPTURE_HELPER" >&2
    exit 1
fi

TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-scope.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

PROJECT_DIR="${TMPDIR}/project"
mkdir -p "$PROJECT_DIR/tests"

cat > "${PROJECT_DIR}/package.json" <<'JSON'
{
  "scripts": {
    "test": "node test-recorder.mjs"
  }
}
JSON

cat > "${PROJECT_DIR}/test-recorder.mjs" <<'JS'
import { writeFileSync } from 'node:fs';

const args = process.argv.slice(2);
writeFileSync('received-args.json', JSON.stringify(args));

if (args.join('\n') !== 'tests/mcp-config.test.ts\ntests/mcp.test.ts') {
  console.error(`Unexpected test args: ${JSON.stringify(args)}`);
  process.exit(1);
}

console.log('# tests 2');
console.log('# pass 2');
console.log('# fail 0');
JS

touch "${PROJECT_DIR}/tests/mcp-config.test.ts" "${PROJECT_DIR}/tests/mcp.test.ts"

HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_COMPONENT_ID="node-scope-smoke" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
HOMEBOY_CHANGED_TEST_FILES=$'tests/mcp-config.test.ts\ntests/mcp.test.ts' \
bash "${SCRIPT_DIR}/test-runner.sh" > "${TMPDIR}/runner.out"

if ! grep -q 'Scope:     2 selected test file(s)' "${TMPDIR}/runner.out"; then
    echo "Expected selected-scope line in runner output" >&2
    cat "${TMPDIR}/runner.out" >&2
    exit 1
fi

if [ "$(cat "${PROJECT_DIR}/received-args.json")" != '["tests/mcp-config.test.ts","tests/mcp.test.ts"]' ]; then
    echo "Runner did not receive selected test files" >&2
    cat "${PROJECT_DIR}/received-args.json" >&2
    exit 1
fi

echo "nodejs changed-scope smoke passed"
