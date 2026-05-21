#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-nx-vitest.XXXXXX")"
trap 'rm -rf "$TMPDIR"' EXIT

PROJECT_DIR="${TMPDIR}/project"
mkdir -p "$PROJECT_DIR"

cat > "${PROJECT_DIR}/package.json" <<'JSON'
{
  "scripts": {
    "test": "node fake-nx-vitest-timeout.mjs"
  }
}
JSON

cat > "${PROJECT_DIR}/fake-nx-vitest-timeout.mjs" <<'JS'
console.log(`
 NX   Running target test:vite for project playground-wordpress

 FAIL src/test/version-detect.spec.ts > Test WP version detection > detects WP trunk at runtime
Error: Test timed out in 30000ms.

Failed tasks:
- playground-wordpress:test:vite
`);
process.exit(1);
JS

cat > "${TMPDIR}/write-results.sh" <<'SH'
homeboy_write_test_results() {
    local total="$1"
    local passed="$2"
    local failed="$3"
    local skipped="$4"
    local partial="${5:-}"

    node - "$HOMEBOY_TEST_RESULTS_FILE" "$total" "$passed" "$failed" "$skipped" "$partial" <<'JS'
const fs = require('node:fs');
const [file, total, passed, failed, skipped, partial] = process.argv.slice(2);
const payload = {
  total: Number(total),
  passed: Number(passed),
  failed: Number(failed),
  skipped: Number(skipped)
};
if (partial) {
  payload.partial = partial;
}
fs.writeFileSync(file, JSON.stringify(payload, null, 2));
JS
}
SH

set +e
HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
HOMEBOY_COMPONENT_ID="node-nx-vitest-smoke" \
HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${TMPDIR}/write-results.sh" \
HOMEBOY_TEST_RESULTS_FILE="${TMPDIR}/test-results.json" \
HOMEBOY_TEST_FAILURES_FILE="${TMPDIR}/test-failures.json" \
bash "${SCRIPT_DIR}/test-runner.sh" > "${TMPDIR}/runner.out" 2>&1
RUNNER_EXIT=$?
set -e

if [ "$RUNNER_EXIT" -eq 0 ]; then
    echo "Expected runner to fail for fake Vitest timeout" >&2
    cat "${TMPDIR}/runner.out" >&2
    exit 1
fi

node - "${TMPDIR}/test-results.json" "${TMPDIR}/test-failures.json" <<'JS'
const fs = require('node:fs');
const assert = require('node:assert/strict');

const results = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
assert.equal(results.total, 1);
assert.equal(results.passed, 0);
assert.equal(results.failed, 1);
assert.equal(results.skipped, 0);
assert.equal(results.partial, 'vitest-timeout');

const failures = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
assert.equal(failures.total, 1);
assert.equal(failures.passed, 0);
assert.equal(failures.failures.length, 1);
assert.equal(
  failures.failures[0].test_name,
  'Test WP version detection > detects WP trunk at runtime'
);
assert.equal(failures.failures[0].test_file, 'src/test/version-detect.spec.ts');
assert.equal(failures.failures[0].error_type, 'vitest_timeout');
assert.equal(failures.failures[0].test_id, failures.failures[0].test_name);
assert.equal(failures.failures[0].file, failures.failures[0].test_file);
assert.equal(failures.failures[0].failure_type, failures.failures[0].error_type);
assert.equal(failures.failures[0].line, 0);
assert.equal(failures.failures[0].stderr_excerpt, '');
assert.match(failures.failures[0].fingerprint, /^[a-f0-9]{64}$/);
assert.match(failures.failures[0].stdout_excerpt, /FAIL src\/test\/version-detect\.spec\.ts/);
assert.match(failures.failures[0].message, /Test timed out in 30000ms/);
assert.match(failures.failures[0].message, /Nx task: playground-wordpress:test:vite/);
JS

if grep -q 'unknown-runner' "${TMPDIR}/test-results.json"; then
    echo "Nx/Vitest timeout should not be labelled unknown-runner" >&2
    cat "${TMPDIR}/test-results.json" >&2
    exit 1
fi

echo "nodejs Nx/Vitest timeout smoke passed"
