#!/usr/bin/env bash
set -euo pipefail

# Rust test runner for homeboy test.
#
# Runs cargo test with standard homeboy extension env vars:
#   HOMEBOY_EXTENSION_PATH  — path to this extension
#   HOMEBOY_COMPONENT_PATH  — path to the Rust project
#   HOMEBOY_SKIP_LINT       — if "1", skip the pre-test lint step
#   HOMEBOY_FIX_ONLY        — if "1", propagates to the nested lint-runner so
#                             the pre-test lint step runs in fix mode. Sent by
#                             `homeboy refactor --from test --write`.
#   HOMEBOY_STEP            — comma-separated steps to run (lint, test)
#   HOMEBOY_SKIP            — comma-separated steps to skip
#   HOMEBOY_DEBUG           — if "1", show debug output
#
# Passthrough args after -- are forwarded to cargo test.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHARED_LIB_DIR="${HOMEBOY_SHARED_LIB_DIR:-}"
if [ -z "$SHARED_LIB_DIR" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
    SHARED_LIB_DIR="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
fi
SHARED_LIB_DIR="${SHARED_LIB_DIR:-$(cd "${SCRIPT_DIR}/../../scripts/lib" && pwd)}"
SETTINGS_HELPER="${HOMEBOY_RUNTIME_SETTINGS_HELPER:-${SHARED_LIB_DIR}/settings.sh}"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/runner-harness.sh"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/test-failures-adapter.sh"
homeboy_runner_harness_init --steps --failure-trap --sidecar-writer
# shellcheck source=/dev/null
homeboy_runner_harness_source_command_capture
# shellcheck source=../../scripts/lib/settings.sh
source "${SETTINGS_HELPER}"

rust_test_runner() {
    case "${HOMEBOY_RUST_TEST_RUNNER:-$(homeboy_setting rust_test_runner '.rust_test_runner // .test_runner' cargo)}" in
        nextest|cargo-nextest|cargo_nextest)
            printf 'nextest'
            ;;
        *)
            printf 'cargo'
            ;;
    esac
}

rust_nextest_fallback_enabled() {
    if [ -n "${HOMEBOY_RUST_NEXTEST_FALLBACK:-}" ]; then
        case "${HOMEBOY_RUST_NEXTEST_FALLBACK}" in
            1|true|TRUE|yes|YES|on|ON) return 0 ;;
            *) return 1 ;;
        esac
    fi

    [ "$(homeboy_setting_bool rust_nextest_fallback true '.rust_nextest_fallback // .nextest_fallback')" = "true" ]
}

rust_cargo_test_threads() {
    local value
    value="${HOMEBOY_RUST_CARGO_TEST_THREADS:-$(homeboy_setting rust_cargo_test_threads '.rust_cargo_test_threads // .cargo_test_threads' '')}"
    case "$value" in
        ''|0) return 1 ;;
        *[!0-9]*) return 1 ;;
        *) printf '%s' "$value" ;;
    esac
}

rust_test_scope_json() {
    python3 - "${HOMEBOY_TEST_SCOPE_KIND:-workspace}" "${HOMEBOY_TEST_SCOPE_MESSAGE:-}" "${HOMEBOY_TEST_RUNNER_ARGS:-}" <<'PY'
import json
import sys

kind, message, runner_args_raw = sys.argv[1:]
args = [line for line in runner_args_raw.splitlines() if line]
if not kind or kind == "full":
    kind = "workspace"

print(json.dumps({
    "kind": kind,
    "reason": message,
    "args": args,
}))
PY
}

rust_append_scope_args() {
    local scope_json="$1"
    while IFS= read -r scope_arg; do
        [ -n "$scope_arg" ] || continue
        TEST_ARGS+=("$scope_arg")
    done < <(printf '%s' "$scope_json" | jq -r '.args[]?')
}

rust_nextest_args_from_cargo_args() {
    local saw_separator=0
    for scope_arg in "${TEST_ARGS[@]:3}"; do
        if [ "$scope_arg" = "--" ]; then
            saw_separator=1
            continue
        fi
        NEXTEST_ARGS+=("$scope_arg")
    done
    [ "$saw_separator" -eq 0 ] || return 0
}

rust_emit_test_plan() {
    local runner="$1"
    local runner_command="$2"
    local scope_json="$3"
    local status="$4"
    local exit_code="${5:-0}"
    local plan_tmp

    if ! type homeboy_merge_annotations >/dev/null 2>&1; then
        return 0
    fi

    plan_tmp="$(mktemp)"
    python3 - "$runner" "$runner_command" "$scope_json" "$status" "$exit_code" "$plan_tmp" <<'PY'
import json
import sys

runner, command, scope_raw, status, exit_code, target = sys.argv[1:]
try:
    scope = json.loads(scope_raw)
except json.JSONDecodeError:
    scope = {"kind": "workspace", "reason": "Scope metadata was not available.", "args": []}

record = {
    "type": "rust-test-command",
    "runner": runner,
    "command": command,
    "scope": scope.get("kind") or "workspace",
    "scope_reason": scope.get("reason") or "",
    "args": scope.get("args") or [],
    "status": status,
    "exit_code": int(exit_code or 0),
}
with open(target, "w", encoding="utf-8") as handle:
    json.dump([record], handle, indent=2)
    handle.write("\n")
PY
    homeboy_merge_annotations rust-test-plan "$plan_tmp" || true
    rm -f "$plan_tmp"
}

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Rust Test Environment:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_SKIP_LINT=${HOMEBOY_SKIP_LINT:-NOT_SET}"
    echo "HOMEBOY_FIX_ONLY=${HOMEBOY_FIX_ONLY:-NOT_SET}"
    echo "PROJECT_PATH=${PROJECT_PATH}"
    echo "Passthrough args: $*"
fi

# Verify this is a Rust project
if [ ! -f "${PROJECT_PATH}/Cargo.toml" ]; then
    echo "Error: No Cargo.toml found at ${PROJECT_PATH}"
    echo "Not a Rust project — cannot run tests."
    exit 1
fi

echo "Running Rust tests..."

# ── Step 1: Pre-test lint (unless skipped) ──
if should_run_step "lint" && [ "${HOMEBOY_SKIP_LINT:-}" != "1" ]; then
    LINT_RUNNER="${EXTENSION_PATH}/scripts/lint-runner.sh"
    if [ -f "$LINT_RUNNER" ]; then
        echo ""
        echo "Running pre-test lint checks..."
        HOMEBOY_SUMMARY_MODE=1 bash "$LINT_RUNNER"
        echo ""
    fi
elif ! should_run_step "lint"; then
    echo "Skipping lint (step filter)"
else
    echo "Skipping lint (--skip-lint)"
fi

# ── Step 2: cargo test (with optional coverage) ──
if ! should_run_step "test"; then
    echo "Skipping tests (step filter)"
    exit 0
fi

# Coverage mode: use cargo-tarpaulin if HOMEBOY_COVERAGE=1
if [ "${HOMEBOY_COVERAGE:-}" = "1" ]; then
    if command -v cargo-tarpaulin &>/dev/null; then
        echo "Running cargo tarpaulin (test + coverage)..."

        COVERAGE_JSON_FILE=$(mktemp --suffix=.json)
        TARPAULIN_ARGS=(
            tarpaulin
            --manifest-path "${PROJECT_PATH}/Cargo.toml"
            --out Json
            --output-dir "$(dirname "$COVERAGE_JSON_FILE")"
        )

        if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
            echo "DEBUG: cargo ${TARPAULIN_ARGS[*]} $*"
        fi

        homeboy_run_step_capture TEST_TMPFILE TEST_EXIT "cargo tarpaulin" -- cargo "${TARPAULIN_ARGS[@]}" "$@" || true

        # Parse test results for homeboy core (best-effort, non-blocking)
        PARSE_RESULTS="${EXTENSION_PATH}/scripts/parse-test-results.sh"
        if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
            bash "$PARSE_RESULTS" "$TEST_TMPFILE" || true
        fi

        TEST_OUTPUT=$(cat "$TEST_TMPFILE")
        homeboy_cleanup_step_capture "$TEST_TMPFILE"


        if [ $TEST_EXIT -ne 0 ]; then
            SUMMARY=$(echo "$TEST_OUTPUT" | grep -E "^test result:" | tail -1 || true)
            FAILURES=$(echo "$TEST_OUTPUT" | grep -E "^---- .* ----$|^test .* FAILED$" || true)
            if [ -n "$SUMMARY" ]; then echo ""; echo "$SUMMARY"; fi
            FAILED_STEP="cargo tarpaulin"
            FAILURE_REPLAY_MODE="none"
            rm -f "$COVERAGE_JSON_FILE"
            exit $TEST_EXIT
        fi

        # Parse tarpaulin JSON output for coverage summary
        # Tarpaulin writes to tarpaulin-report.json in output-dir
        TARPAULIN_REPORT="$(dirname "$COVERAGE_JSON_FILE")/tarpaulin-report.json"
        if [ -f "$TARPAULIN_REPORT" ]; then
            # Extract coverage from tarpaulin JSON
            COVERAGE_DATA=$(python3 -c "
import json, sys, os
with open('$TARPAULIN_REPORT') as f:
    data = json.load(f)
files = []
total_lines = 0
covered_lines = 0
source_dir = '${PROJECT_PATH}/'
for fpath, traces in data.get('files', {}).items():
    rel = fpath.replace(source_dir, '') if fpath.startswith(source_dir) else fpath
    flines = len(traces)
    fcovered = sum(1 for t in traces if t.get('hits', 0) > 0)
    total_lines += flines
    covered_lines += fcovered
    pct = round((fcovered / flines) * 100, 2) if flines > 0 else 100
    files.append({'file': rel, 'lines': flines, 'covered': fcovered, 'line_pct': pct})
files.sort(key=lambda x: x['line_pct'])
line_pct = round((covered_lines / total_lines) * 100, 2) if total_lines > 0 else 0
result = {
    'totals': {
        'lines': {'total': total_lines, 'covered': covered_lines, 'pct': line_pct},
        'methods': {'total': 0, 'covered': 0, 'pct': 0},
        'classes': {'total': 0, 'covered': 0, 'pct': 0}
    },
    'files': files
}
print(json.dumps(result, indent=2))
" 2>/dev/null || true)

            if [ -n "$COVERAGE_DATA" ]; then
                LINE_PCT=$(echo "$COVERAGE_DATA" | jq -r '.totals.lines.pct')
                LINE_TOTAL=$(echo "$COVERAGE_DATA" | jq -r '.totals.lines.total')
                LINE_COVERED=$(echo "$COVERAGE_DATA" | jq -r '.totals.lines.covered')
                echo ""
                echo "============================================"
                echo "COVERAGE SUMMARY"
                echo "============================================"
                echo "  Lines: ${LINE_PCT}% (${LINE_COVERED}/${LINE_TOTAL})"
                echo ""

                if [ -n "${HOMEBOY_COVERAGE_FILE:-}" ]; then
                    echo "$COVERAGE_DATA" > "$HOMEBOY_COVERAGE_FILE"
                fi

                if [ -n "${HOMEBOY_COVERAGE_MIN:-}" ]; then
                    BELOW=$(echo "$LINE_PCT < ${HOMEBOY_COVERAGE_MIN}" | bc -l 2>/dev/null || echo "0")
                    if [ "$BELOW" = "1" ]; then
                        echo "COVERAGE FAILED: ${LINE_PCT}% is below minimum ${HOMEBOY_COVERAGE_MIN}%"
                        FAILED_STEP="Coverage threshold (${LINE_PCT}% < ${HOMEBOY_COVERAGE_MIN}%)"
                        rm -f "$TARPAULIN_REPORT" "$COVERAGE_JSON_FILE"
                        exit 1
                    fi
                fi
            fi

            rm -f "$TARPAULIN_REPORT" "$COVERAGE_JSON_FILE"
        fi

        echo "Rust tests passed (with coverage)"
        exit 0
    else
        echo ""
        echo "WARNING: Coverage requested but cargo-tarpaulin not found."
        echo "  Install: cargo install cargo-tarpaulin"
        echo "  Falling back to cargo test without coverage."
        echo ""
    fi
fi

SELECTED_RUNNER="$(rust_test_runner)"
SCOPE_JSON="$(rust_test_scope_json)"
SCOPE_KIND="$(printf '%s' "$SCOPE_JSON" | jq -r '.kind // "workspace"')"
SCOPE_REASON="$(printf '%s' "$SCOPE_JSON" | jq -r '.reason // empty')"

if [ "$SCOPE_KIND" = "fallback" ]; then
    echo "Rust test scope fallback: ${SCOPE_REASON}"
elif [ -n "$SCOPE_REASON" ]; then
    echo "Rust test scope: ${SCOPE_REASON}"
fi

if [ "$SELECTED_RUNNER" = "nextest" ]; then
    if cargo nextest --version >/dev/null 2>&1; then
        echo "Running cargo nextest..."
    elif rust_nextest_fallback_enabled; then
        echo "WARNING: cargo-nextest requested but not available; falling back to cargo test."
        echo "  Install: cargo install cargo-nextest"
        SELECTED_RUNNER="cargo"
    else
        echo "Error: cargo-nextest requested but not available."
        echo "Install it with: cargo install cargo-nextest"
        rust_emit_test_plan "nextest" "cargo nextest run" "$SCOPE_JSON" "missing-runner" 127
        exit 127
    fi
else
    echo "Running cargo test..."
fi

TEST_ARGS=(
    test
    --manifest-path "${PROJECT_PATH}/Cargo.toml"
)

if [ -n "${HOMEBOY_TEST_SCOPE_MESSAGE:-}" ]; then
    echo "$HOMEBOY_TEST_SCOPE_MESSAGE"
fi

rust_append_scope_args "$SCOPE_JSON"

COMMAND_LABEL="cargo test"
COMMAND_BINARY=(cargo "${TEST_ARGS[@]}")

if [ "$SELECTED_RUNNER" = "cargo" ]; then
    CARGO_TEST_THREADS="$(rust_cargo_test_threads || true)"
    if [ -n "$CARGO_TEST_THREADS" ]; then
        COMMAND_BINARY+=(-- --test-threads="$CARGO_TEST_THREADS")
    fi
fi

if [ "$SELECTED_RUNNER" = "nextest" ]; then
    NEXTEST_ARGS=(
        run
        --manifest-path "${PROJECT_PATH}/Cargo.toml"
    )
    rust_nextest_args_from_cargo_args
    COMMAND_LABEL="cargo nextest run"
    COMMAND_BINARY=(cargo nextest "${NEXTEST_ARGS[@]}")
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: ${COMMAND_BINARY[*]} $*"
fi

rust_emit_test_plan "$SELECTED_RUNNER" "$COMMAND_LABEL" "$SCOPE_JSON" "started" 0
homeboy_run_step_capture TEST_TMPFILE TEST_EXIT "$COMMAND_LABEL" -- "${COMMAND_BINARY[@]}" "$@" || true
rust_emit_test_plan "$SELECTED_RUNNER" "$COMMAND_LABEL" "$SCOPE_JSON" "completed" "$TEST_EXIT"

# Parse test results for homeboy core (best-effort, non-blocking)
PARSE_RESULTS="${EXTENSION_PATH}/scripts/parse-test-results.sh"
if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
    bash "$PARSE_RESULTS" "$TEST_TMPFILE" || true
fi

TEST_OUTPUT=$(cat "$TEST_TMPFILE")


if [ $TEST_EXIT -eq 0 ]; then
    # Extract test summary line
    SUMMARY=$(echo "$TEST_OUTPUT" | grep -E "^test result:" | tail -1 || true)
    if [ -n "$SUMMARY" ]; then
        echo ""
        echo "$SUMMARY"
    fi
    echo ""
    echo "Rust tests passed"
    rm -f "$TEST_TMPFILE"
else
    # Extract failure details
    SUMMARY=$(echo "$TEST_OUTPUT" | grep -E "^test result:" | tail -1 || true)
    FAILURES=$(echo "$TEST_OUTPUT" | grep -E "^---- .* ----$|^test .* FAILED$" || true)

    if [ -n "$SUMMARY" ]; then
        echo ""
        echo "$SUMMARY"
    fi

    if homeboy_test_failures_enabled; then
        homeboy_runner_harness_temp TEST_FAILURES_TMP "homeboy-rust-test-failures.XXXXXX"
        python3 - "$PROJECT_PATH" "$TEST_TMPFILE" "$TEST_FAILURES_TMP" <<'PY'
import hashlib
import json
import os
import re
import sys

project, output_file, target = sys.argv[1:]
with open(output_file, encoding="utf-8") as handle:
    lines = handle.read().splitlines()

failed = []
for raw in lines:
    match = re.match(r"^test (?P<name>.+) \.\.\. FAILED$", raw)
    if match:
        failed.append(match.group("name"))

if not failed:
    for raw in lines:
        match = re.match(r"^---- (?P<name>.+) stdout ----$", raw)
        if match:
            failed.append(match.group("name"))

failures = []
for name in dict.fromkeys(failed):
    message = f"Rust test failed: {name}"
    identity = f"rust:test:{name}"
    failures.append({
        "test_id": name,
        "suite": None,
        "file": None,
        "line": None,
        "message": message,
        "failure_type": "test_failure",
        "fingerprint": hashlib.sha256(identity.encode()).hexdigest(),
        "stdout_excerpt": "\n".join(lines)[-4000:],
        "stderr_excerpt": "",
    })

if not failures:
    identity = "rust:cargo-test:failed"
    failures.append({
        "test_id": "cargo test",
        "suite": None,
        "file": None,
        "line": None,
        "message": "cargo test failed before individual test failures could be parsed",
        "failure_type": "infrastructure",
        "fingerprint": hashlib.sha256(identity.encode()).hexdigest(),
        "stdout_excerpt": "\n".join(lines)[-4000:],
        "stderr_excerpt": "",
    })

with open(target, "w", encoding="utf-8") as handle:
    json.dump(failures, handle, indent=2)
    handle.write("\n")
PY
        homeboy_test_failures_merge_file "$TEST_FAILURES_TMP"
    fi

    FAILED_STEP="$COMMAND_LABEL"
    FAILURE_REPLAY_MODE="none"
    rm -f "$TEST_TMPFILE"
    exit $TEST_EXIT
fi

# Detect zero-test runs — only warn if NO test result line shows passed tests.
# Cargo runs multiple test binaries (unit, integration, doc-tests); some may
# legitimately have 0 tests while others have hundreds.
TOTAL_PASSED=$( { echo "$TEST_OUTPUT" | grep -Eo '[0-9]+ passed' || true; } | awk '{s+=$1} END {print s+0}' )
if [ "$TOTAL_PASSED" -eq 0 ]; then
    TEST_FILE_COUNT=$(find "$PROJECT_PATH" -name "*test*" -name "*.rs" -not -path "*/target/*" 2>/dev/null | wc -l | tr -d ' ')
    if [ "$TEST_FILE_COUNT" -gt 0 ]; then
        echo ""
        echo "============================================"
        echo "WARNING: cargo test ran 0 tests"
        echo "============================================"
        echo ""
        echo "Found ${TEST_FILE_COUNT} test files but no tests were executed."
        echo "This may indicate a configuration issue."
        if [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ]; then
            FAILED_STEP="$COMMAND_LABEL"
            FAILURE_REPLAY_MODE="none"
            rm -f "$TEST_TMPFILE"
            exit 1
        fi
    fi
fi

rm -f "$TEST_TMPFILE"
