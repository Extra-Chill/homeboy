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
    local names="$WORK_DIR/failed-names.txt" actual status=0
    : > "$names"
    actual="$(bash -c 'source "$1"; rust_nextest_unsharded_counts "$2" "$3"' _ \
        "$WORK_DIR/counts-harness.sh" "$stream" "$names")" || status=$?

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
    local output status=0
    output="$(mktemp)"
    "$@" >"$output" 2>&1 || status=$?
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

run_runner() {
    local results_file="$1" stream="$2" cargo_exit="$3" measured="$4"
    shift 4
    PATH="$BIN_DIR:$PATH" \
    HOMEBOY_EXTENSION_PATH="$EXTENSION_DIR" \
    HOMEBOY_COMPONENT_PATH="$PROJECT_DIR" \
    HOMEBOY_SKIP_LINT=1 \
    HOMEBOY_RUST_TEST_RUNNER=nextest \
    HOMEBOY_RUST_NEXTEST_MEASURED_COUNTS="$measured" \
    HOMEBOY_RUNTIME_RUNNER_PRELUDE="$HELPER_DIR/runner-prelude.sh" \
    HOMEBOY_RUNTIME_COMMAND_CAPTURE="$HELPER_DIR/command-capture.sh" \
    HOMEBOY_RUNTIME_WRITE_TEST_RESULTS="$HELPER_DIR/write-test-results.sh" \
    HOMEBOY_TEST_RESULTS_FILE="$results_file" \
    HOMEBOY_FAKE_NEXTEST_STREAM="$stream" \
    HOMEBOY_FAKE_CARGO_EXIT="$cargo_exit" \
    HOMEBOY_FAKE_CARGO_ARGS="$WORK_DIR/cargo-args.txt" \
    HOMEBOY_FAKE_CARGO_ENV="$WORK_DIR/cargo-env.txt" \
    "$@" \
    bash "$RUNNER" 2>&1 || true
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
