#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

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
