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
SIDECAR_WRITER_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_SIDECAR_WRITER sidecar-writer.sh)" || exit 1
RUNNER_PRELUDE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_RUNNER_PRELUDE runner-prelude.sh)" || exit 1
COMMAND_CAPTURE_HELPER="$(homeboy_runtime_helper "$REPOSITORY_ROOT" HOMEBOY_RUNTIME_COMMAND_CAPTURE command-capture.sh)" || exit 1

TMPDIR="$(mktemp -d "${TMPDIR:-/tmp}/homeboy-node-nx-vitest.XXXXXX")"
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
HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
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
assert.equal(failures.length, 1);
assert.equal(
  failures[0].test_name,
  'Test WP version detection > detects WP trunk at runtime'
);
assert.equal(failures[0].test_file, 'src/test/version-detect.spec.ts');
assert.equal(failures[0].error_type, 'vitest_timeout');
assert.equal(failures[0].test_id, failures[0].test_name);
assert.equal(failures[0].file, failures[0].test_file);
assert.equal(failures[0].failure_type, failures[0].error_type);
assert.equal(failures[0].line, 0);
assert.equal(failures[0].stderr_excerpt, '');
assert.match(failures[0].fingerprint, /^[a-f0-9]{64}$/);
assert.match(failures[0].stdout_excerpt, /FAIL src\/test\/version-detect\.spec\.ts/);
assert.match(failures[0].message, /Test timed out in 30000ms/);
assert.match(failures[0].message, /Nx task: playground-wordpress:test:vite/);
JS

if grep -q 'unknown-runner' "${TMPDIR}/test-results.json"; then
    echo "Nx/Vitest timeout should not be labelled unknown-runner" >&2
    cat "${TMPDIR}/test-results.json" >&2
    exit 1
fi

echo "nodejs Nx/Vitest timeout smoke passed"
