#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT="$(mktemp "${TMPDIR:-/tmp}/rust-test-failures-output.XXXXXX")"
RESULTS="$(mktemp "${TMPDIR:-/tmp}/rust-test-failures-json.XXXXXX")"
IDENTITIES="$(mktemp "${TMPDIR:-/tmp}/rust-test-failure-identities.XXXXXX")"
trap 'rm -f "$OUTPUT" "$RESULTS" "$IDENTITIES"' EXIT

cat > "$OUTPUT" <<'EOF'
running 2 tests
test crate::tests::failed_only ... FAILED

---- crate::tests::panic_before_summary stdout ----
thread 'crate::tests::panic_before_summary' panicked at src/lib.rs:42:9:
assertion failed: expected provider execution count 1, observed 0
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    crate::tests::panic_before_summary
    crate::tests::failed_only

test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
EOF

python3 "$SCRIPT_DIR/parse-test-failures.py" /project "$OUTPUT" "$RESULTS"
python3 - "$RESULTS" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    records = json.load(handle)

assert len(records) == 2, records
panic = next(record for record in records if record["test_id"] == "crate::tests::panic_before_summary")
assert panic["file"] == "src/lib.rs", panic
assert panic["line"] == 42, panic
assert "expected provider execution count 1" in panic["message"], panic
assert panic["stdout_excerpt"]
# Generic gate diagnostics require a Homeboy-owned evidence ref. Until Homeboy
# provides that handoff, this remains its established test-failures sidecar.
assert "schema" not in panic, panic
PY

printf 'thread malformed panic output\n' > "$OUTPUT"
python3 "$SCRIPT_DIR/parse-test-failures.py" /project "$OUTPUT" "$RESULTS"
python3 - "$RESULTS" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    records = json.load(handle)

assert len(records) == 1, records
assert records[0]["test_id"] == "cargo test", records
assert records[0]["failure_type"] == "infrastructure", records
PY

{
    printf "thread 'crate::tests::bounded' panicked at src/lib.rs:7:3:\n"
    printf 'x%.0s' {1..2048}
    printf '\n'
} > "$OUTPUT"
python3 "$SCRIPT_DIR/parse-test-failures.py" /project "$OUTPUT" "$RESULTS"
python3 - "$RESULTS" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    record = json.load(handle)[0]

assert record["test_id"] == "crate::tests::bounded", record
assert len(record["message"].encode()) == 1024, record
assert record["file"] == "src/lib.rs", record
assert record["line"] == 7, record
PY

cat > "$OUTPUT" <<'EOF'
---- crate::tests::unicode_☃ stdout ----
thread 'tokio-runtime-worker' panicked at /project/src/lib.rs:9:5:
assertion failed: worker panic maps to the enclosing test

failures:
    crate::tests::unicode_☃

test result: FAILED. 0 passed; 1 failed
EOF
python3 "$SCRIPT_DIR/parse-test-failures.py" /project "$OUTPUT" "$RESULTS"
python3 - "$RESULTS" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    records = json.load(handle)

assert len(records) == 1, records
record = records[0]
assert record["test_id"] == "crate::tests::unicode_☃", record
assert record["file"] == "src/lib.rs", record
assert record["line"] == 9, record
assert "worker panic" in record["message"], record
PY

printf '\xff\xfe' > "$OUTPUT"
python3 "$SCRIPT_DIR/parse-test-failures.py" /project "$OUTPUT" "$RESULTS"
python3 - "$RESULTS" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    record = json.load(handle)[0]

assert record["test_id"] == "cargo test", record
assert record["failure_type"] == "infrastructure", record
PY

# Cargo's compiler-artifact map disambiguates identical names across packages
# and targets. Sidecar IDs must be byte-for-byte inventory IDs.
cat > "$IDENTITIES" <<'EOF'
{"tests":[
  {"id":"alpha::lib::alpha::tests::duplicate","package":"alpha","target":"alpha","target_kind":"lib","name":"tests::duplicate","executable":"/target/debug/deps/alpha-a1"},
  {"id":"alpha::test::api::tests::duplicate","package":"alpha","target":"api","target_kind":"test","name":"tests::duplicate","executable":"/target/debug/deps/api-a2"},
  {"id":"beta::test::api::tests::duplicate","package":"beta","target":"api","target_kind":"test","name":"tests::duplicate","executable":"/target/debug/deps/api-b1"}
]}
EOF
cat > "$OUTPUT" <<'EOF'
     Running unittests src/lib.rs (/target/debug/deps/alpha-a1)
test tests::duplicate ... FAILED
failures:
    tests::duplicate
test result: FAILED. 0 passed; 1 failed
     Running tests/api.rs (/target/debug/deps/api-a2)
test tests::duplicate ... FAILED
failures:
    tests::duplicate
test result: FAILED. 0 passed; 1 failed
     Running tests/api.rs (/target/debug/deps/api-b1)
test tests::duplicate ... FAILED
failures:
    tests::duplicate
test result: FAILED. 0 passed; 1 failed
EOF
python3 "$SCRIPT_DIR/parse-test-failures.py" /project "$OUTPUT" "$RESULTS" "$IDENTITIES"
python3 - "$RESULTS" "$IDENTITIES" <<'PY'
import json
import sys

records = json.load(open(sys.argv[1], encoding="utf-8"))
inventory_ids = {item["id"] for item in json.load(open(sys.argv[2], encoding="utf-8"))["tests"]}
assert {record["test_id"] for record in records} == inventory_ids, records
assert all(record["test_id"] == record["test_name"] for record in records), records
PY

printf 'Rust test-failure parser smoke passed\n'
