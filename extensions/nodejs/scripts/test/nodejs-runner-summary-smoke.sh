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

# Run test-runner.sh against a fake project whose test script prints $2 and
# exits $3. Results land in ${TMPDIR}/<case>/.
run_case() {
    local name="$1" output="$2" exit_code="$3" case_dir="${TMPDIR}/$1"
    mkdir -p "${case_dir}/project"
    printf '{"scripts":{"test":"node fake-runner.cjs"}}\n' > "${case_dir}/project/package.json"
    printf '%s' "$output" > "${case_dir}/project/output.txt"
    printf "process.stdout.write(require('node:fs').readFileSync('output.txt'), () => process.exit(%s));\n" "$exit_code" \
        > "${case_dir}/project/fake-runner.cjs"

    set +e
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="${case_dir}/project" \
    HOMEBOY_COMPONENT_ID="node-runner-summary-smoke" \
    HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="${TMPDIR}/write-results.sh" \
    HOMEBOY_RUNTIME_SIDECAR_WRITER="$SIDECAR_WRITER_HELPER" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$RUNNER_PRELUDE_HELPER" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$COMMAND_CAPTURE_HELPER" \
    HOMEBOY_TEST_RESULTS_FILE="${case_dir}/test-results.json" \
    HOMEBOY_TEST_FAILURES_FILE="${case_dir}/test-failures.json" \
    bash "${SCRIPT_DIR}/test-runner.sh" > "${case_dir}/runner.out" 2>&1
    local runner_exit=$?
    set -e

    if [ "$runner_exit" -ne "$exit_code" ]; then
        echo "${name}: expected runner exit ${exit_code}, got ${runner_exit}" >&2
        cat "${case_dir}/runner.out" >&2
        exit 1
    fi
}

# An Nx-wrapped Vitest timeout that never printed a summary keeps the failed
# test instead of becoming an opaque `unknown-runner` failure.
run_case nx-vitest-timeout '
 NX   Running target test:vite for project playground-wordpress

 FAIL src/test/version-detect.spec.ts > Test WP version detection > detects WP trunk at runtime
Error: Test timed out in 30000ms.

Failed tasks:
- playground-wordpress:test:vite
' 1

node - "${TMPDIR}/nx-vitest-timeout/test-results.json" "${TMPDIR}/nx-vitest-timeout/test-failures.json" <<'JS'
const fs = require('node:fs');
const assert = require('node:assert/strict');

const results = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
assert.deepEqual(results, { total: 1, passed: 0, failed: 1, skipped: 0, partial: 'vitest-timeout' });

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

# A coloured Vitest 4 summary (FORCE_COLOR or a TTY), byte for byte as a
# passing suite printed it, yields its real counts. Unstripped, no parser
# matched and the run was reported as zero executed tests (Extra-Chill/homeboy#16955).
run_case coloured-vitest-summary $'\e[32m\xe2\x9c\x93\e[39m src/a.test.ts \e[2m(\e[22m\e[2m3 tests\e[22m\e[2m)\e[22m\n\n\e[2m Test Files \e[22m \e[1m\e[32m274 passed\e[39m\e[22m\e[2m | \e[22m\e[33m1 skipped\e[39m\e[90m (275)\e[39m\n\e[2m      Tests \e[22m \e[1m\e[32m2636 passed\e[39m\e[22m\e[2m | \e[22m\e[33m4 skipped\e[39m\e[90m (2640)\e[39m\n' 0

node - "${TMPDIR}/coloured-vitest-summary/test-results.json" <<'JS'
const assert = require('node:assert/strict');
assert.deepEqual(
  JSON.parse(require('node:fs').readFileSync(process.argv[2], 'utf8')),
  { total: 2640, passed: 2636, failed: 0, skipped: 4 }
);
JS

echo "nodejs runner summary smoke passed"
