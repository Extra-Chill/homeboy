#!/usr/bin/env bash
# Shared test-failure sidecar adapter helpers.

homeboy_test_failures_enabled() {
    [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ]
}

homeboy_test_failures_require_writer() {
    if type homeboy_merge_test_failures >/dev/null 2>&1; then
        return 0
    fi

    echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write test failures" >&2
    return 1
}

homeboy_test_failures_merge_file() {
    local failures_file="$1"
    homeboy_test_failures_enabled || return 0
    [ -f "$failures_file" ] || return 0
    homeboy_test_failures_require_writer || return 1
    homeboy_merge_test_failures "$failures_file"
}

homeboy_test_failure_record_json() {
    local namespace="$1"
    local test_id="$2"
    local suite="$3"
    local file="$4"
    local line="$5"
    local message="$6"
    local failure_type="$7"
    local stdout_excerpt="${8:-}"
    local stderr_excerpt="${9:-}"

    node - "$namespace" "$test_id" "$suite" "$file" "$line" "$message" "$failure_type" "$stdout_excerpt" "$stderr_excerpt" <<'NODE'
const crypto = require('node:crypto');
const [namespace, testId, suite, file, line, message, failureType, stdoutExcerpt, stderrExcerpt] = process.argv.slice(2);
const fingerprintInput = [namespace, testId, suite, file, line, message, failureType].join('\0');
const sourceLine = line === '' ? null : Number(line || 0);
const record = {
  test_id: testId,
  test_name: testId,
  suite: suite || null,
  file: file || null,
  test_file: file || null,
  line: sourceLine,
  message,
  failure_type: failureType || 'test_failure',
  error_type: failureType || 'test_failure',
  fingerprint: crypto.createHash('sha256').update(fingerprintInput).digest('hex'),
  stdout_excerpt: stdoutExcerpt || '',
  stderr_excerpt: stderrExcerpt || '',
  source_file: file || null,
  source_line: sourceLine,
};
console.log(JSON.stringify(record));
NODE
}

homeboy_test_failure_emit_record_json() {
    local failure_json="$1"
    homeboy_test_failures_enabled || return 0
    if ! type homeboy_sidecar_emit >/dev/null 2>&1; then
        echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write test failures" >&2
        return 1
    fi
    homeboy_sidecar_emit test.failure "$failure_json"
}
