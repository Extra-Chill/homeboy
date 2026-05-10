#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTRACTOR="$SCRIPT_DIR/extract-engine-data.sh"

if [ ! -f "$EXTRACTOR" ]; then
    echo "ERROR: extractor not found at $EXTRACTOR" >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required" >&2
    exit 1
fi

RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/extract-engine-data-results.XXXXXX.json")
GITHUB_OUTPUT_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/extract-engine-data-output.XXXXXX")
cleanup() {
    rm -f "$RESULTS_TMPFILE" "$GITHUB_OUTPUT_TMPFILE"
}
trap cleanup EXIT

jq -n '{
    component_id: "fixture",
    iterations: 1,
    scenarios: [
        {
            id: "fixture-agent",
            metadata: {
                job_status: "completed",
                engine_data: {
                    foo: { bar: "baz" },
                    deep: { nested: 42 }
                }
            }
        },
        {
            id: "other-agent",
            metadata: {
                job_status: "failed",
                engine_data: {}
            }
        }
    ]
}' > "$RESULTS_TMPFILE"

pass_count=0
fail_count=0

pass() {
    pass_count=$((pass_count + 1))
    echo "PASS: $*"
}

fail() {
    fail_count=$((fail_count + 1))
    echo "FAIL: $*" >&2
}

stdout=$(GITHUB_OUTPUT="$GITHUB_OUTPUT_TMPFILE" bash "$EXTRACTOR" \
    --results "$RESULTS_TMPFILE" \
    --scenario fixture-agent \
    --field foo_bar=metadata.engine_data.foo.bar \
    --field nested=metadata.engine_data.deep.nested \
    --field missing=metadata.engine_data.missing \
    --required-field foo_bar \
    --required-field nested)

if grep -F "foo_bar:" <<<"$stdout" | grep -F "baz" >/dev/null \
    && grep -F "nested:" <<<"$stdout" | grep -F "42" >/dev/null; then
    pass "stdout includes projected key/value pairs"
else
    fail "stdout missing projected key/value pairs"
    printf '%s\n' "$stdout" >&2
fi

if grep -Fx "foo_bar=baz" "$GITHUB_OUTPUT_TMPFILE" >/dev/null \
    && grep -Fx "nested=42" "$GITHUB_OUTPUT_TMPFILE" >/dev/null \
    && grep -Fx "missing=" "$GITHUB_OUTPUT_TMPFILE" >/dev/null; then
    pass "GITHUB_OUTPUT includes projected key=value lines"
else
    fail "GITHUB_OUTPUT missing projected key=value lines"
    cat "$GITHUB_OUTPUT_TMPFILE" >&2
fi

if ! bash "$EXTRACTOR" \
    --results "$RESULTS_TMPFILE" \
    --scenario missing-agent \
    --field foo_bar=metadata.engine_data.foo.bar \
    --quiet >/dev/null 2>&1; then
    pass "missing scenario fails closed"
else
    fail "missing scenario unexpectedly succeeded"
fi

if ! bash "$EXTRACTOR" \
    --results "$RESULTS_TMPFILE" \
    --scenario fixture-agent \
    --field missing=metadata.engine_data.missing \
    --required-field missing \
    --quiet >/dev/null 2>&1; then
    pass "missing required field fails closed"
else
    fail "missing required field unexpectedly succeeded"
fi

if ! bash "$EXTRACTOR" \
    --results "$RESULTS_TMPFILE" \
    --scenario fixture-agent \
    --field foo_bar=metadata.engine_data.foo.bar \
    --required-status failed \
    --quiet >/dev/null 2>&1; then
    pass "required status mismatch fails closed"
else
    fail "required status mismatch unexpectedly succeeded"
fi

if [ "$fail_count" -gt 0 ]; then
    echo "✗ engine_data extractor smoke test FAILED (${fail_count} failed, ${pass_count} passed)" >&2
    exit 1
fi

echo "✓ engine_data extractor smoke test PASSED (${pass_count} checks)"
