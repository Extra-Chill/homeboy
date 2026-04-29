#!/usr/bin/env bash
# Smoke-test the Rust test-result parser without GNU grep assumptions.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/rust-parse-results-output.XXXXXX")
RESULTS_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/rust-parse-results-json.XXXXXX")
HELPER_TMPFILE=$(mktemp "${TMPDIR:-/tmp}/rust-write-test-results.XXXXXX")

# shellcheck disable=SC2064
trap "rm -f '$OUTPUT_TMPFILE' '$RESULTS_TMPFILE' '$HELPER_TMPFILE'" EXIT

cat > "$OUTPUT_TMPFILE" <<'EOF'
running 3 tests
test result: ok. 2 passed; 1 failed; 3 ignored; 0 measured; 0 filtered out;
test result: ok. 5 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out;
EOF

cat > "$HELPER_TMPFILE" <<'EOF'
homeboy_write_test_results() {
    cat > "$HOMEBOY_TEST_RESULTS_FILE" <<JSON
{"total":$1,"passed":$2,"failed":$3,"skipped":$4}
JSON
}
EOF

HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$HELPER_TMPFILE" \
HOMEBOY_TEST_RESULTS_FILE="$RESULTS_TMPFILE" \
    bash "$SCRIPT_DIR/parse-test-results.sh" "$OUTPUT_TMPFILE"

python3 - "$RESULTS_TMPFILE" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)

expected = {"total": 12, "passed": 7, "failed": 1, "skipped": 4}
if data != expected:
    raise SystemExit(f"unexpected parser output: {data!r} != {expected!r}")
PY

echo "Rust test-result parser smoke passed"
