#!/usr/bin/env bash
# Smoke-test the unsharded nextest counts path against fixture
# libtest-json-plus output.
#
# Fixtures rather than a live nextest run: what is under test is the projection
# from an event stream to a Homeboy test result, and a real run cannot produce
# the cases that matter most here -- a compile failure that emits no events, a
# retry that must not be double-counted, a child process leaking its own libtest
# JSON into the captured stream.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="${HOMEBOY_TESTED_EXTENSION_DIR:-$(cd "${SCRIPT_DIR}/.." && pwd)}"
RUNNER="${EXTENSION_DIR}/scripts/test-runner.sh"
WORK_DIR="$(mktemp -d -t homeboy-nextest-unsharded.XXXXXX)"
trap 'rm -rf "$WORK_DIR"' EXIT

FAILURES=0

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    FAILURES=$((FAILURES + 1))
}

# ── Part 1: rust_nextest_unsharded_counts in isolation ──
#
# Sourcing the runner would execute it, so the two functions under test are
# extracted with their own dependencies stubbed. EXTENSION_PATH is what the
# heredoc uses to find nextest_events_lib.
cat > "$WORK_DIR/counts-harness.sh" <<EOF
set -euo pipefail
EXTENSION_PATH="${EXTENSION_DIR}"
EOF
python3 - "$RUNNER" "$WORK_DIR/counts-harness.sh" <<'PY'
import re
import sys

source = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r"^rust_nextest_unsharded_counts\(\) \{\n.*?^\}\n", source, re.S | re.M)
assert match, "rust_nextest_unsharded_counts not found in test-runner.sh"
with open(sys.argv[2], "a", encoding="utf-8") as handle:
    handle.write(match.group(0))
PY

counts_case() {
    local label="$1" stream="$2" expected_counts="$3" expected_failed="$4"
    local names="$WORK_DIR/failed-names.txt" contexts="$WORK_DIR/failure-context.json" actual status=0
    : > "$names"
    actual="$(bash -c 'source "$1"; rust_nextest_unsharded_counts "$2" "$3" "$4"' _ \
        "$WORK_DIR/counts-harness.sh" "$stream" "$names" "$contexts")" || status=$?

    if [ "$expected_counts" = "UNMEASURED" ]; then
        if [ "$status" -eq 0 ]; then
            fail "${label}: expected unmeasured (non-zero exit), got exit 0 with '${actual}'"
        fi
        if [ -n "$actual" ]; then
            fail "${label}: unmeasured runs must print nothing, got '${actual}'"
        fi
        return 0
    fi

    if [ "$status" -ne 0 ]; then
        fail "${label}: expected measured counts, exited ${status}"
        return 0
    fi
    if [ "$actual" != "$(printf '%b' "$expected_counts")" ]; then
        fail "${label}: counts '${actual}' != expected '${expected_counts}'"
    fi
    local actual_failed
    actual_failed="$(tr '\n' ' ' < "$names" | sed 's/ $//')"
    if [ "$actual_failed" != "$expected_failed" ]; then
        fail "${label}: failed names '${actual_failed}' != expected '${expected_failed}'"
    fi
}

# All passing.
cat > "$WORK_DIR/pass.jsonl" <<'EOF'
{"type":"suite","event":"started","test_count":3}
{"type":"test","event":"started","name":"alpha::alpha_lib$tests::one"}
{"type":"test","event":"ok","name":"alpha::alpha_lib$tests::one"}
{"type":"test","event":"ok","name":"alpha::alpha_lib$tests::two"}
{"type":"test","event":"ok","name":"beta::beta_integration$covers_three"}
{"type":"suite","event":"ok","passed":3}
EOF
counts_case "passing run" "$WORK_DIR/pass.jsonl" '3\t3\t0\t0' ""

# Failures, and their identities must survive.
cat > "$WORK_DIR/fail.jsonl" <<'EOF'
{"type":"test","event":"ok","name":"alpha::alpha_lib$tests::one"}
{"type":"test","event":"failed","name":"alpha::alpha_lib$tests::two","stdout":"assertion failed"}
{"type":"test","event":"failed","name":"beta::beta_integration$covers_three"}
{"type":"test","event":"ignored","name":"beta::beta_integration$covers_four"}
EOF
counts_case "run with failures" "$WORK_DIR/fail.jsonl" '4\t1\t2\t1' \
    'alpha::alpha_lib$tests::two beta::beta_integration$covers_three'

# Failure context is retained before later passing output can push the failure
# out of a bounded command log. It remains private until the shared publication
# boundary applies redaction and then its UTF-8 byte bound.
python3 - > "$WORK_DIR/early-failure.jsonl" <<'PY'
import json
import sys

print(json.dumps({"type": "test", "event": "failed", "name": "alpha::alpha_lib$tests::early", "stdout": "x" * 5000 + " api_key=very-secret assertion failed: expected 2, got 1"}))
for index in range(600):
    print(json.dumps({"type": "test", "event": "ok", "name": f"alpha::alpha_lib$tests::late_{index}"}))
PY
counts_case "early bounded failure context" "$WORK_DIR/early-failure.jsonl" '601\t600\t1\t0' 'alpha::alpha_lib$tests::early'
python3 - "$WORK_DIR/failure-context.json" <<'PY'
import json
import sys

record = json.load(open(sys.argv[1], encoding="utf-8"))[0]
assert "assertion failed: expected 2, got 1" in record["stdout_excerpt"], record
assert "very-secret" in record["stdout_excerpt"], record
PY

# The Rust adapter sends excerpts through the shared test.failures writer. Test
# the published sidecar, not the private event/context files that feed it.
python3 - "$RUNNER" "$WORK_DIR/counts-harness.sh" <<'PY'
import re
import sys

source = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r"^rust_merge_failure_evidence\(\) \{\n.*?^\}\n", source, re.S | re.M)
assert match, "rust_merge_failure_evidence not found in test-runner.sh"
with open(sys.argv[2], "a", encoding="utf-8") as handle:
    handle.write(match.group(0))
PY
cat > "$WORK_DIR/published-failed-names.txt" <<'EOF'
alpha::alpha_lib$tests::early
EOF
python3 - "$WORK_DIR/published-failure-context.json" <<'PY'
import json
import sys

long_secret = "long-secret-value-" + "x" * 10000
many_short = " ".join(f"refresh_token=short-secret-{index}" for index in range(700))
json.dump([{
    "name": "alpha::alpha_lib$tests::early",
    # The key is far outside the retained tail. Redaction must happen before
    # bounding so no unlabelled suffix can be published.
    "stdout_excerpt": '{\\"password\\":\\"fixture-password-value\\",\\"access_token\\":\\"fixture-access-token\\",\\"refresh_token\\":\\"fixture-refresh-token\\"} Authorization: Bearer fixture-bearer-value https://example.test/?token=fixture-query-token api_key=' + long_secret + " " + "☃" * 2500,
    # Replacement expansion is intentionally larger than these short values.
    "stderr_excerpt": many_short + " secret=fixture-secret-value",
}], open(sys.argv[1], "w", encoding="utf-8"))
PY
HOMEBOY_TEST_FAILURES_FILE="$WORK_DIR/published-test-failures.json" \
HOMEBOY_PUBLISHED_FAILURES="$WORK_DIR/published-test-failures.json" \
bash -c '
    source "$1"
    source "$2"
    homeboy_merge_test_failures() { cp "$1" "$HOMEBOY_PUBLISHED_FAILURES"; }
    rust_merge_failure_evidence "$3" "" "$4"
' _ "$EXTENSION_DIR/../scripts/lib/test-failures-adapter.sh" "$WORK_DIR/counts-harness.sh" "$WORK_DIR/published-failed-names.txt" "$WORK_DIR/published-failure-context.json"
python3 - "$WORK_DIR/published-test-failures.json" <<'PY'
import json
import sys

published = json.load(open(sys.argv[1], encoding="utf-8"))[0]
evidence = published["stdout_excerpt"] + published["stderr_excerpt"]
for fixture in ("fixture-password-value", "fixture-access-token", "fixture-refresh-token", "fixture-bearer-value", "fixture-query-token", "fixture-secret-value", "long-secret-value-", "short-secret-"):
    assert fixture not in evidence, published
for excerpt in (published["stdout_excerpt"], published["stderr_excerpt"]):
    encoded = excerpt.encode("utf-8")
    assert len(encoded) <= 4096, len(encoded)
    assert "�" not in excerpt, excerpt
assert evidence.count("[REDACTED]") >= 5, published
PY
REDACTION_UNDER_TEST="$EXTENSION_DIR/../scripts/lib/redaction.mjs" \
node --input-type=module - "$WORK_DIR/published-test-failures.json" "$WORK_DIR/published-failure-context.json" <<'JS'
import { readFile } from 'node:fs/promises';
const { redactText } = await import(process.env.REDACTION_UNDER_TEST);

const [publishedFile, rawFile] = process.argv.slice(2);
const published = JSON.parse(await readFile(publishedFile, 'utf8'))[0];
const raw = JSON.parse(await readFile(rawFile, 'utf8'))[0];
for (const field of ['stdout_excerpt', 'stderr_excerpt']) {
    const excerpt = published[field];
    const match = excerpt.match(/^\[truncated (\d+) UTF-8 bytes; retained tail\]\n/);
    if (!match) throw new Error(`${field} was expected to be truncated`);
    const retained = excerpt.slice(match[0].length);
    const expected = Buffer.byteLength(redactText(raw[field]), 'utf8');
    if (Number(match[1]) + Buffer.byteLength(retained, 'utf8') !== expected) {
        throw new Error(`${field} truncation omission count is not truthful`);
    }
}
JS

# The capture helper reports tee failure without hiding a real child failure.
node --test "$EXTENSION_DIR/../scripts/lib/test-failure-record.test.mjs"

python3 - "$RUNNER" "$WORK_DIR/counts-harness.sh" <<'PY'
import re
import sys

source = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r"^rust_capture_nextest_events\(\) \{\n.*?^\}\n", source, re.S | re.M)
assert match, "rust_capture_nextest_events not found in test-runner.sh"
with open(sys.argv[2], "a", encoding="utf-8") as handle:
    handle.write(match.group(0))
PY
CAPTURE_EVENTS="$WORK_DIR/capture-events.jsonl"
set +e
CAPTURE_MARKER="${CAPTURE_EVENTS}.complete" bash -c 'source "$1"; tee() { cat > "$1"; mkdir "$CAPTURE_MARKER"; }; child() { printf "event\n"; }; rust_capture_nextest_events "$2" child' _ "$WORK_DIR/counts-harness.sh" "$CAPTURE_EVENTS" >/dev/null 2>&1
MARKER_STATUS=$?
set -e
if [ "$MARKER_STATUS" -eq 0 ]; then
    fail "a missing completeness marker must not report successful capture"
fi
rmdir "${CAPTURE_EVENTS}.complete"
set +e
bash -c 'source "$1"; tee() { cat >/dev/null; return 1; }; child() { return 37; }; rust_capture_nextest_events "$2" child' _ "$WORK_DIR/counts-harness.sh" "$CAPTURE_EVENTS" >/dev/null 2>&1
CAPTURE_STATUS=$?
set -e
if [ "$CAPTURE_STATUS" -ne 37 ] || [ -f "${CAPTURE_EVENTS}.complete" ]; then
    fail "capture failure must preserve child exit status and omit completeness marker"
fi

# An unset event path must not turn its marker into relative .complete and
# delete an unrelated project file during an early shard failure.
python3 - "$RUNNER" "$WORK_DIR/counts-harness.sh" <<'PY'
import re
import sys

source = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r"^rust_nextest_cleanup\(\) \{\n.*?^\}\n", source, re.S | re.M)
assert match, "rust_nextest_cleanup not found in test-runner.sh"
with open(sys.argv[2], "a", encoding="utf-8") as handle:
    handle.write(match.group(0))
PY
touch "$WORK_DIR/.complete"
bash -c 'source "$1"; homeboy_runner_harness_cleanup_path() { rm -f "$2"; }; SHARD_EVENTS=""; rust_nextest_cleanup 1' _ "$WORK_DIR/counts-harness.sh" "$WORK_DIR/.complete" || true
if [ ! -f "$WORK_DIR/.complete" ]; then
    fail "early cleanup with no owned event path must preserve project .complete"
fi

cat > "$WORK_DIR/names-only-failure.jsonl" <<'EOF'
{"type":"test","event":"failed","name":"alpha::alpha_lib$tests::names_only"}
EOF
counts_case "names-only failure context" "$WORK_DIR/names-only-failure.jsonl" '1\t0\t1\t0' 'alpha::alpha_lib$tests::names_only'
python3 - "$WORK_DIR/failure-context.json" <<'PY'
import json
import sys

record = json.load(open(sys.argv[1], encoding="utf-8"))[0]
assert record["stdout_excerpt"] == "[nextest did not emit captured per-test output]", record
assert record["stderr_excerpt"] == "", record
PY

# All skipped is a measured outcome, not an absent one.
cat > "$WORK_DIR/skipped.jsonl" <<'EOF'
{"type":"test","event":"ignored","name":"alpha::alpha_lib$tests::one"}
{"type":"test","event":"ignored","name":"alpha::alpha_lib$tests::two"}
EOF
counts_case "all-skipped run" "$WORK_DIR/skipped.jsonl" '2\t0\t0\t2' ""

# No events at all — a compile failure never reaches the first test.
cat > "$WORK_DIR/compile-failure.jsonl" <<'EOF'
   Compiling alpha v0.1.0 (/project)
error[E0425]: cannot find value `nope` in this scope
 --> src/lib.rs:4:5
error: could not compile `alpha` (lib test) due to 1 previous error
EOF
counts_case "compile failure" "$WORK_DIR/compile-failure.jsonl" UNMEASURED ""

# Truly empty output — killed before it wrote anything.
: > "$WORK_DIR/empty.jsonl"
counts_case "empty output" "$WORK_DIR/empty.jsonl" UNMEASURED ""

# Lifecycle events only: started but never finished. Nothing terminal, so
# nothing measured — this must not read as a zero-failure pass.
cat > "$WORK_DIR/lifecycle-only.jsonl" <<'EOF'
{"type":"suite","event":"started","test_count":2}
{"type":"test","event":"started","name":"alpha::alpha_lib$tests::one"}
{"type":"test","event":"started","name":"alpha::alpha_lib$tests::two"}
EOF
counts_case "lifecycle events only" "$WORK_DIR/lifecycle-only.jsonl" UNMEASURED ""

# A retry is one test with one outcome. Flaky-then-green counts once, passed.
cat > "$WORK_DIR/retry.jsonl" <<'EOF'
{"type":"test","event":"failed","name":"alpha::alpha_lib$tests::flaky"}
{"type":"test","event":"ok","name":"alpha::alpha_lib$tests::flaky#2"}
{"type":"test","event":"ok","name":"alpha::alpha_lib$tests::stable"}
EOF
counts_case "retry folds onto its base identity" "$WORK_DIR/retry.jsonl" '2\t2\t0\t0' ""

# A retry that stays red is still one test, and still reports its base name.
cat > "$WORK_DIR/retry-red.jsonl" <<'EOF'
{"type":"test","event":"failed","name":"alpha::alpha_lib$tests::flaky"}
{"type":"test","event":"failed","name":"alpha::alpha_lib$tests::flaky#2"}
EOF
counts_case "exhausted retry stays failed" "$WORK_DIR/retry-red.jsonl" '1\t0\t1\t0' \
    'alpha::alpha_lib$tests::flaky'

# Interleaved noise: nextest's human progress output shares the captured stream,
# and a child process can emit its own libtest JSON with an unparseable name.
cat > "$WORK_DIR/noisy.jsonl" <<'EOF'
    Starting 2 tests across 1 binary
{"type":"test","event":"ok","name":"alpha::alpha_lib$tests::one"}
{"type":"test","event":"ok","name":"bare_name_without_separators"}
{"type":"test","event":"ok","name":null}
not json at all
{"type":"test","event":"ok","name":"alpha::alpha_lib$tests::two"}
        PASS [   0.011s] alpha::alpha_lib tests::two
------------
     Summary [   0.021s] 2 tests run: 2 passed, 0 skipped
EOF
counts_case "noise and unparseable identities are ignored" "$WORK_DIR/noisy.jsonl" '2\t2\t0\t0' ""

# ── Part 2: the runner end to end, with a fake cargo ──

PROJECT_DIR="$WORK_DIR/project"
HELPER_DIR="$WORK_DIR/helpers"
BIN_DIR="$WORK_DIR/bin"
mkdir -p "$PROJECT_DIR/tests" "$HELPER_DIR" "$BIN_DIR"

cat > "$PROJECT_DIR/Cargo.toml" <<'EOF'
[package]
name = "rust-nextest-unsharded-smoke"
version = "0.1.0"
edition = "2021"
EOF

cat > "$PROJECT_DIR/tests/integration_scope.rs" <<'EOF'
#[test]
fn integration_scope_runs() {
    assert_eq!(1, 1);
}
EOF

cat > "$HELPER_DIR/runner-prelude.sh" <<'EOF'
homeboy_runner_init() {
    PROJECT_PATH="$HOMEBOY_COMPONENT_PATH"
    EXTENSION_PATH="$HOMEBOY_EXTENSION_PATH"
}
should_run_step() { return 0; }
EOF

cat > "$HELPER_DIR/command-capture.sh" <<'EOF'
homeboy_run_step_capture() {
    local output_var="$1" exit_var="$2"
    shift 3
    [ "${1:-}" != -- ] || shift
    local output full status=0
    output="$(mktemp)"
    full="$(mktemp)"
    "$@" >"$full" 2>&1 || status=$?
    tail -c "${HOMEBOY_COMMAND_CAPTURE_MAX_BYTES:-1048576}" "$full" > "$output"
    rm -f "$full"
    printf -v "$output_var" '%s' "$output"
    printf -v "$exit_var" '%s' "$status"
    return "$status"
}
homeboy_cleanup_step_capture() { rm -f "$1"; }
EOF

cat > "$HELPER_DIR/write-test-results.sh" <<'EOF'
homeboy_write_test_results() {
    python3 - "$HOMEBOY_TEST_RESULTS_FILE" "$@" <<'PY'
import json
import sys

path, total, passed, failed, skipped, partial = sys.argv[1:]
json.dump({"total": int(total), "passed": int(passed), "failed": int(failed), "skipped": int(skipped), "partial": partial}, open(path, "w"))
PY
}
EOF

cat > "$HELPER_DIR/settings.sh" <<'EOF'
homeboy_setting() { printf '%s' "${3:-}"; }
homeboy_setting_bool() { printf '%s' "${2:-}"; }
EOF

# Fake cargo: replays whatever stream the case put in HOMEBOY_FAKE_NEXTEST_STREAM
# and records the argv it was called with.
cat > "$BIN_DIR/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" = "nextest" ] && [ "${2:-}" = "--version" ]; then
    echo "cargo-nextest 0.9.140"
    exit 0
fi

printf '%s\n' "$@" > "${HOMEBOY_FAKE_CARGO_ARGS}"
printf '%s\n' "NEXTEST_EXPERIMENTAL_LIBTEST_JSON=${NEXTEST_EXPERIMENTAL_LIBTEST_JSON:-unset}" \
    > "${HOMEBOY_FAKE_CARGO_ENV}"
if [ -n "${HOMEBOY_FAKE_NEXTEST_STREAM:-}" ] && [ -f "${HOMEBOY_FAKE_NEXTEST_STREAM}" ]; then
    cat "${HOMEBOY_FAKE_NEXTEST_STREAM}"
fi
exit "${HOMEBOY_FAKE_CARGO_EXIT:-0}"
EOF
chmod +x "$BIN_DIR/cargo"

cat > "$BIN_DIR/tee" <<'EOF'
#!/usr/bin/env bash
if [ "${HOMEBOY_FAKE_TEE_FAIL:-}" = 1 ]; then
    cat >/dev/null
    exit 1
fi
exec /usr/bin/tee "$@"
EOF
chmod +x "$BIN_DIR/tee"

run_runner() {
    local results_file="$1" stream="$2" cargo_exit="$3" measured="$4"
    shift 4
    env \
    PATH="$BIN_DIR:$PATH" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_RUST_TEST_RUNNER=nextest \
    HOMEBOY_RUST_NEXTEST_MEASURED_COUNTS="$measured" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$HELPER_DIR/runner-prelude.sh" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$HELPER_DIR/command-capture.sh" \
    HOMEBOY_RUNTIME_SETTINGS_HELPER="$HELPER_DIR/settings.sh" \
    HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$HELPER_DIR/write-test-results.sh" \
    HOMEBOY_TEST_RESULTS_FILE="$results_file" \
    HOMEBOY_FAKE_NEXTEST_STREAM="$stream" \
    HOMEBOY_FAKE_CARGO_EXIT="$cargo_exit" \
    HOMEBOY_FAKE_CARGO_ARGS="$WORK_DIR/cargo-args.txt" \
    HOMEBOY_FAKE_CARGO_ENV="$WORK_DIR/cargo-env.txt" \
    "$@" bash "$RUNNER" 2>&1 || true
}

# Measured green run writes the cargo-shaped result tuple.
RESULTS="$WORK_DIR/results-pass.json"
rm -f "$RESULTS"
OUTPUT="$(run_runner "$RESULTS" "$WORK_DIR/pass.jsonl" 0 1)"

if [[ "$OUTPUT" != *"Rust test runner: cargo nextest (measured counts from libtest-json-plus)"* ]]; then
    fail "runner must announce the measured nextest selection. Output:
$OUTPUT"
fi
if [[ "$OUTPUT" != *"Rust nextest result: total=3 passed=3 failed=0 skipped=0"* ]]; then
    fail "runner must report measured counts. Output:
$OUTPUT"
fi
if [[ "$OUTPUT" == *"ran 0 tests"* ]]; then
    fail "a fully green measured run must not warn about zero tests. Output:
$OUTPUT"
fi
if [ ! -f "$RESULTS" ]; then
    fail "measured run must write a Homeboy test result"
else
    python3 - "$RESULTS" <<'PY' || exit 1
import json
import sys

data = json.load(open(sys.argv[1], encoding="utf-8"))
expected = {"total": 3, "passed": 3, "failed": 0, "skipped": 0, "partial": ""}
if data != expected:
    raise SystemExit(f"FAIL: measured result {data!r} != {expected!r}")
PY
fi

# The invocation carries the flags the parser depends on.
ACTUAL_ARGS="$(cat "$WORK_DIR/cargo-args.txt")"
for required in "--no-fail-fast" "--no-tests" "warn" "libtest-json-plus" "--message-format-version" "0.1"; do
    if ! printf '%s\n' "$ACTUAL_ARGS" | grep -qx -- "$required"; then
        fail "measured nextest invocation missing '${required}'. Args:
$ACTUAL_ARGS"
    fi
done
if ! grep -qx 'NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1' "$WORK_DIR/cargo-env.txt"; then
    fail "measured nextest run must export NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1"
fi

# The host-facing command log retains only its tail. Counts still come from the
# tee'd event file, so this early failure survives 600 later passing events.
RESULTS="$WORK_DIR/results-early-failure.json"
rm -f "$RESULTS"
HOMEBOY_COMMAND_CAPTURE_MAX_BYTES=256 run_runner "$RESULTS" "$WORK_DIR/early-failure.jsonl" 100 1 >/dev/null
python3 - "$RESULTS" <<'PY' || exit 1
import json
import sys

assert json.load(open(sys.argv[1], encoding="utf-8")) == {"total": 601, "passed": 600, "failed": 1, "skipped": 0, "partial": ""}
PY

# A tee write failure makes evidence incomplete. Even a green child must not
# produce passing totals from the partial event stream.
RESULTS="$WORK_DIR/results-capture-failure.json"
rm -f "$RESULTS"
OUTPUT="$(run_runner "$RESULTS" "$WORK_DIR/pass.jsonl" 0 1 HOMEBOY_FAKE_TEE_FAIL=1)"
if [ -f "$RESULTS" ]; then
    fail "capture failure must not publish partial passing totals: $(cat "$RESULTS")"
fi
if [[ "$OUTPUT" != *"event capture is incomplete"* ]]; then
    fail "capture failure must be explicit. Output:
$OUTPUT"
fi

# Failures: counted, named, and red.
RESULTS="$WORK_DIR/results-fail.json"
rm -f "$RESULTS"
OUTPUT="$(run_runner "$RESULTS" "$WORK_DIR/fail.jsonl" 100 1)"
if [[ "$OUTPUT" != *"Rust nextest result: total=4 passed=1 failed=2 skipped=1"* ]]; then
    fail "failing measured run must report its counts. Output:
$OUTPUT"
fi
python3 - "$RESULTS" <<'PY' || exit 1
import json
import sys

data = json.load(open(sys.argv[1], encoding="utf-8"))
expected = {"total": 4, "passed": 1, "failed": 2, "skipped": 1, "partial": ""}
if data != expected:
    raise SystemExit(f"FAIL: failing measured result {data!r} != {expected!r}")
PY

# A measured failure with a zero exit code is still a failure.
RESULTS="$WORK_DIR/results-liar.json"
rm -f "$RESULTS"
OUTPUT="$(run_runner "$RESULTS" "$WORK_DIR/fail.jsonl" 0 1)"
if [[ "$OUTPUT" != *"treating the run as failed"* ]]; then
    fail "measured failures must override a zero exit code. Output:
$OUTPUT"
fi
if [[ "$OUTPUT" == *"Rust tests passed"* ]]; then
    fail "a run with measured failures must not report success. Output:
$OUTPUT"
fi

# All skipped: measured, written, and not mistaken for a broken run.
RESULTS="$WORK_DIR/results-skipped.json"
rm -f "$RESULTS"
OUTPUT="$(run_runner "$RESULTS" "$WORK_DIR/skipped.jsonl" 0 1)"
python3 - "$RESULTS" <<'PY' || exit 1
import json
import sys

data = json.load(open(sys.argv[1], encoding="utf-8"))
expected = {"total": 2, "passed": 0, "failed": 0, "skipped": 2, "partial": ""}
if data != expected:
    raise SystemExit(f"FAIL: all-skipped result {data!r} != {expected!r}")
PY

# No events: unmeasured. No result file is the signal, and it must be said out
# loud rather than left to a silent green.
RESULTS="$WORK_DIR/results-unmeasured.json"
rm -f "$RESULTS"
OUTPUT="$(run_runner "$RESULTS" "$WORK_DIR/compile-failure.jsonl" 101 1)"
if [ -f "$RESULTS" ]; then
    fail "a run with no test events must not write a test result: $(cat "$RESULTS")"
fi
if [[ "$OUTPUT" != *"no structured test counts"* ]]; then
    fail "unmeasured run must say so. Output:
$OUTPUT"
fi
if [[ "$OUTPUT" == *"Rust tests passed"* ]]; then
    fail "unmeasured run must not report success. Output:
$OUTPUT"
fi

# Same stream, but the runner exits 0. Still unmeasured, still no result.
RESULTS="$WORK_DIR/results-unmeasured-green.json"
rm -f "$RESULTS"
OUTPUT="$(run_runner "$RESULTS" "$WORK_DIR/empty.jsonl" 0 1)"
if [ -f "$RESULTS" ]; then
    fail "an empty stream must not write a test result even on exit 0: $(cat "$RESULTS")"
fi
if [[ "$OUTPUT" != *"no structured test counts"* ]]; then
    fail "empty stream must be reported as unmeasured. Output:
$OUTPUT"
fi

# ── Part 3: the gate ──

# Measurement off: legacy invocation shape, no JSON flags, no measured result.
RESULTS="$WORK_DIR/results-off.json"
rm -f "$RESULTS"
OUTPUT="$(run_runner "$RESULTS" "$WORK_DIR/pass.jsonl" 0 0)"
if [[ "$OUTPUT" != *"unmeasured; rust_nextest_measured_counts is off"* ]]; then
    fail "disabled measurement must be visible in the output. Output:
$OUTPUT"
fi
EXPECTED_LEGACY_ARGS=$'nextest\nrun\n--manifest-path\n'"$PROJECT_DIR"$'/Cargo.toml\n--workspace'
if [ "$(cat "$WORK_DIR/cargo-args.txt")" != "$EXPECTED_LEGACY_ARGS" ]; then
    fail "measurement off must preserve the legacy nextest invocation. Args:
$(cat "$WORK_DIR/cargo-args.txt")"
fi

# nextest absent: falls back to cargo exactly as before, and the fallback path
# is not a measured nextest run.
cat > "$BIN_DIR/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [ "${1:-}" = "nextest" ] && [ "${2:-}" = "--version" ]; then
    exit 1
fi

printf '%s\n' "$@" > "${HOMEBOY_FAKE_CARGO_ARGS}"
echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
EOF
chmod +x "$BIN_DIR/cargo"

RESULTS="$WORK_DIR/results-fallback.json"
rm -f "$RESULTS"
OUTPUT="$(run_runner "$RESULTS" "$WORK_DIR/pass.jsonl" 0 1)"
if [[ "$OUTPUT" != *"falling back to cargo test"* ]]; then
    fail "missing nextest must fall back to cargo test. Output:
$OUTPUT"
fi
if [[ "$OUTPUT" != *"Rust test runner: cargo test"* ]]; then
    fail "fallback must announce cargo. Output:
$OUTPUT"
fi
if [ "$(head -1 "$WORK_DIR/cargo-args.txt")" != "test" ]; then
    fail "fallback must invoke cargo test. Args:
$(cat "$WORK_DIR/cargo-args.txt")"
fi
python3 - "$RESULTS" <<'PY' || exit 1
import json
import sys

data = json.load(open(sys.argv[1], encoding="utf-8"))
expected = {"total": 1, "passed": 1, "failed": 0, "skipped": 0, "partial": ""}
if data != expected:
    raise SystemExit(f"FAIL: cargo fallback result {data!r} != {expected!r}")
PY

if [ "$FAILURES" -ne 0 ]; then
    printf '%s check(s) failed\n' "$FAILURES" >&2
    exit 1
fi

printf 'rust nextest unsharded counts smoke ok\n'
