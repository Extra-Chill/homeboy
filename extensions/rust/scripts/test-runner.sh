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
homeboy_runner_harness_load_adapter test-failures-adapter
homeboy_runner_harness_init --steps --failure-trap --sidecar-writer
# shellcheck source=/dev/null
homeboy_runner_harness_source_command_capture
# shellcheck source=../../scripts/lib/settings.sh
source "${SETTINGS_HELPER}"

WRITE_TEST_RESULTS_HELPER="${HOMEBOY_RUNTIME_WRITE_TEST_RESULTS:-}"
if [ -n "$WRITE_TEST_RESULTS_HELPER" ] && [ -f "$WRITE_TEST_RESULTS_HELPER" ]; then
    # shellcheck source=/dev/null
    source "$WRITE_TEST_RESULTS_HELPER"
fi

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

rust_emit_shard_result() {
    local status="$1" total="$2" executed="$3" passed="$4" failed="$5" skipped="$6" duration_ms="$7"
    local result_tmp
    if type homeboy_write_test_results >/dev/null 2>&1; then
        homeboy_write_test_results "$executed" "$passed" "$failed" "$skipped" "rust-shard"
    fi
    type homeboy_merge_annotations >/dev/null 2>&1 || return 0
    result_tmp="$(mktemp)"
    python3 - "$status" "$total" "$executed" "$passed" "$failed" "$skipped" "$duration_ms" "$result_tmp" <<'PY'
import json
import sys
status, total, executed, passed, failed, skipped, duration, target = sys.argv[1:]
with open(target, "w", encoding="utf-8") as handle:
    json.dump([{"type": "rust-test-shard", "status": status, "total": int(total), "executed": int(executed), "passed": int(passed), "failed": int(failed), "skipped": int(skipped), "duration_ms": int(duration)}], handle)
    handle.write("\n")
PY
    homeboy_merge_annotations rust-test-shard "$result_tmp" || true
    rm -f "$result_tmp"
}

rust_shard_cargo_counts() {
    python3 - "$1" <<'PY'
import re
import sys

passed = failed = skipped = 0
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    if not line.startswith("test result:"):
        continue
    for name, target in (("passed", "passed"), ("failed", "failed"), ("ignored", "skipped")):
        match = re.search(rf"(\d+) {name}", line)
        if match:
            if target == "passed": passed += int(match.group(1))
            elif target == "failed": failed += int(match.group(1))
            else: skipped += int(match.group(1))
print(f"{passed}\t{failed}\t{skipped}")
PY
}

rust_nextest_filter() {
    jq -r '[.selected[] | "package(=\(.package)) and kind(=\(.target_kind)) and binary(=\(.target)) and test(=\(.name))"] | join(" + ")' "$1"
}

rust_validate_nextest_membership() {
    local list_file="$1" shard_file="$2"
    python3 - "$list_file" "$shard_file" <<'PY'
import json
import sys

listed = json.load(open(sys.argv[1], encoding="utf-8"))
expected = {item["id"] for item in json.load(open(sys.argv[2], encoding="utf-8"))["selected"]}
actual = set()
for suite in listed.get("rust-suites", {}).values():
    for name, testcase in suite.get("testcases", {}).items():
        if testcase.get("filter-match", {}).get("status") == "matches":
            actual.add(f'{suite["package-name"]}::{suite["kind"]}::{suite["binary-name"]}::{name}')
if actual != expected:
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    raise SystemExit(f"Rust test shard error: nextest exact filter membership mismatch (missing={missing[:1]}, extra={extra[:1]})")
PY
}

rust_nextest_counts() {
    python3 - "$1" "$2" <<'PY'
import json
import sys

expected = json.load(open(sys.argv[2], encoding="utf-8"))["selected"]
names = {}
for item in expected:
    key = f'{item["package"]}::{item["target"]}${item["name"]}'
    if key in names:
        raise SystemExit(f"Rust test shard error: nextest output identity is ambiguous: {key}")
    names[key] = item["id"]
passed = failed = skipped = 0
actual = set()
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        event = json.loads(line)
    except json.JSONDecodeError:
        continue
    if event.get("type") != "test":
        continue
    status = event.get("event")
    if status not in {"ok", "passed", "failed", "fail", "ignored", "skipped"}:
        continue
    name = event.get("name")
    if name not in names or names[name] in actual:
        raise SystemExit(f"Rust test shard error: nextest emitted an unexpected or duplicate test identity: {name}")
    actual.add(names[name])
    if status in {"ok", "passed"}: passed += 1
    elif status in {"failed", "fail"}: failed += 1
    else: skipped += 1
if actual != set(names.values()):
    raise SystemExit(f"Rust test shard error: nextest executed membership does not match the shard manifest (missing={sorted(set(names.values()) - actual)[:1]})")
print(f"{passed}\t{failed}\t{skipped}")
PY
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
if [ "${HOMEBOY_TEST_INVENTORY_ONLY:-}" = "1" ]; then
    echo "Skipping lint (test inventory only)"
elif should_run_step "lint" && [ "${HOMEBOY_SKIP_LINT:-}" != "1" ]; then
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
if [ "${HOMEBOY_TEST_INVENTORY_ONLY:-}" != "1" ] && [ "${HOMEBOY_COVERAGE:-}" = "1" ]; then
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

# Shard manifests are Rust-owned because Cargo target identities and their
# resolution are runner-specific. Normal and changed-scope paths do not enter
# this branch.
if [ -n "${HOMEBOY_TEST_SHARD_MANIFEST:-}${HOMEBOY_TEST_INVENTORY_FILE:-}${HOMEBOY_TEST_INVENTORY_ONLY:-}" ]; then
    if [ "$#" -gt 0 ]; then
        echo "Error: test shard inventory and replay do not support passthrough arguments." >&2
        echo "Remove arguments after -- and encode selection in HOMEBOY_TEST_SHARD_MANIFEST." >&2
        exit 2
    fi
    SHARD_TOOL="${EXTENSION_PATH}/scripts/test-shard-inventory.py"
    SHARD_DATA="$(mktemp)"
    SHARD_ARGS=(--project "$PROJECT_PATH" --runner "$SELECTED_RUNNER" --output "$SHARD_DATA")
    if [ -n "${HOMEBOY_TEST_SHARD_MANIFEST:-}" ]; then
        SHARD_ARGS+=(--manifest "$HOMEBOY_TEST_SHARD_MANIFEST")
    fi
    if ! python3 "$SHARD_TOOL" "${SHARD_ARGS[@]}"; then
        rm -f "$SHARD_DATA"
        exit 1
    fi
    if [ -n "${HOMEBOY_TEST_INVENTORY_FILE:-}" ]; then
        cp "$SHARD_DATA" "$HOMEBOY_TEST_INVENTORY_FILE"
    fi
    if [ "${HOMEBOY_TEST_INVENTORY_ONLY:-}" = "1" ]; then
        cat "$SHARD_DATA"
        rm -f "$SHARD_DATA"
        exit 0
    fi
    if [ -z "${HOMEBOY_TEST_SHARD_MANIFEST:-}" ]; then
        rm -f "$SHARD_DATA"
        exit 0
    fi
    SHARD_STARTED="$(date +%s)"
    SHARD_TOTAL="$(jq '.selected | length' "$SHARD_DATA")"
    SHARD_PASSED=0
    SHARD_FAILED=0
    SHARD_SKIPPED=0
    if [ "$SELECTED_RUNNER" = "nextest" ]; then
        NEXTEST_FILTER="$(rust_nextest_filter "$SHARD_DATA")"
        NEXTEST_LIST="$(mktemp)"
        homeboy_run_step_capture NEXTEST_LIST_OUTPUT NEXTEST_LIST_EXIT "cargo nextest list" -- cargo nextest list --workspace --message-format json -E "$NEXTEST_FILTER" || true
        if [ "$NEXTEST_LIST_EXIT" -ne 0 ] || ! rust_validate_nextest_membership "$NEXTEST_LIST_OUTPUT" "$SHARD_DATA"; then
            SHARD_ELAPSED=$(( $(date +%s) - SHARD_STARTED ))
            rust_emit_shard_result failed "$SHARD_TOTAL" 0 0 0 0 "$((SHARD_ELAPSED*1000))"
            rm -f "$NEXTEST_LIST_OUTPUT" "$NEXTEST_LIST"
            exit 1
        fi
        rm -f "$NEXTEST_LIST_OUTPUT" "$NEXTEST_LIST"
        echo "Replaying Rust nextest shard: ${SHARD_TOTAL} exact identities"
        homeboy_run_step_capture SHARD_OUTPUT SHARD_EXIT "cargo nextest run" -- env NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --manifest-path "${PROJECT_PATH}/Cargo.toml" --test-threads 1 --no-fail-fast --no-tests fail --message-format libtest-json-plus --message-format-version 0.1 -E "$NEXTEST_FILTER" || true
        if ! NEXTEST_COUNTS="$(rust_nextest_counts "$SHARD_OUTPUT" "$SHARD_DATA")"; then
            SHARD_ELAPSED=$(( $(date +%s) - SHARD_STARTED ))
            rust_emit_shard_result failed "$SHARD_TOTAL" 0 0 0 0 "$((SHARD_ELAPSED*1000))"
            rm -f "$SHARD_OUTPUT" "$SHARD_DATA"
            exit 1
        fi
        IFS=$'\t' read -r SHARD_PASSED SHARD_FAILED SHARD_SKIPPED <<< "$NEXTEST_COUNTS"
        rm -f "$SHARD_OUTPUT"
    else
        while IFS=$'\t' read -r package target target_kind name; do
            case "$target_kind" in
                lib) TARGET_ARGS=(--lib) ;;
                bin) TARGET_ARGS=(--bin "$target") ;;
                doc) TARGET_ARGS=(--doc) ;;
                example) TARGET_ARGS=(--example "$target") ;;
                bench) TARGET_ARGS=(--bench "$target") ;;
                *) TARGET_ARGS=(--test "$target") ;;
            esac
            echo "Replaying Rust shard test: ${package}::${target}::${name}"
            homeboy_run_step_capture SHARD_OUTPUT SHARD_EXIT "cargo test" -- cargo test --manifest-path "${PROJECT_PATH}/Cargo.toml" -p "$package" "${TARGET_ARGS[@]}" -- "$name" --exact --test-threads=1 || true
            IFS=$'\t' read -r PASSED FAILED SKIPPED < <(rust_shard_cargo_counts "$SHARD_OUTPUT")
            rm -f "$SHARD_OUTPUT"
            SHARD_PASSED=$((SHARD_PASSED + PASSED))
            SHARD_FAILED=$((SHARD_FAILED + FAILED))
            SHARD_SKIPPED=$((SHARD_SKIPPED + SKIPPED))
        done < <(jq -r '.selected[] | [.package, .target, .target_kind, .name] | @tsv' "$SHARD_DATA")
    fi
    SHARD_ELAPSED=$(( $(date +%s) - SHARD_STARTED ))
    SHARD_EXECUTED=$((SHARD_PASSED + SHARD_FAILED + SHARD_SKIPPED))
    if [ "$SHARD_EXECUTED" -ne "$SHARD_TOTAL" ] || [ "$SHARD_FAILED" -ne 0 ] || [ "${SHARD_EXIT:-0}" -ne 0 ]; then
        echo "Rust shard result: total=${SHARD_TOTAL} executed=${SHARD_EXECUTED} passed=${SHARD_PASSED} failed=${SHARD_FAILED} skipped=${SHARD_SKIPPED}" >&2
        rust_emit_shard_result failed "$SHARD_TOTAL" "$SHARD_EXECUTED" "$SHARD_PASSED" "$SHARD_FAILED" "$SHARD_SKIPPED" "$((SHARD_ELAPSED*1000))"
        rm -f "$SHARD_DATA"
        exit 1
    fi
    echo "Rust shard result: total=${SHARD_TOTAL} passed=${SHARD_PASSED} failed=${SHARD_FAILED} skipped=${SHARD_SKIPPED} duration_ms=$((SHARD_ELAPSED*1000))"
    rust_emit_shard_result completed "$SHARD_TOTAL" "$SHARD_EXECUTED" "$SHARD_PASSED" "$SHARD_FAILED" "$SHARD_SKIPPED" "$((SHARD_ELAPSED*1000))"
    rm -f "$SHARD_DATA"
    exit 0
fi

TEST_ARGS=(
    test
    --manifest-path "${PROJECT_PATH}/Cargo.toml"
)

# Test every workspace member -- not just the root package.
#
# At a hybrid root (a Cargo.toml that is both [package] and [workspace]),
# `cargo test` without `--workspace` runs ONLY the root package's targets and
# silently skips every member crate. Member crates are still compiled as test
# targets, so nothing looks wrong: the binaries are built and never executed.
#
# This used to be added only for `workspace`/`full` scope kinds, which meant
# every other kind (`args`, `rust_filter`, `rust_integration`) inherited the
# root-package-only default. On this repository that hid ten genuinely failing
# member-crate tests from CI indefinitely, including a real release-timeout
# defect (#10477).
#
# `--workspace` is now the default and is withheld only when the scope args
# already choose packages themselves, so an intentionally narrow scope such as
# `-p <crate> --lib` stays narrow.
SCOPE_ARGS_FLAT=" $(printf '%s' "$SCOPE_JSON" | jq -r '.args[]?' | tr '\n' ' ') "
case "$SCOPE_ARGS_FLAT" in
    *" -p "* | *" --package "* | *" --workspace "* | *" --exclude "*)
        WORKSPACE_SELECTION="scope args select packages explicitly"
        ;;
    *)
        TEST_ARGS+=(--workspace)
        WORKSPACE_SELECTION="--workspace (all members)"
        ;;
esac

if [ -n "${HOMEBOY_TEST_SCOPE_MESSAGE:-}" ]; then
    echo "$HOMEBOY_TEST_SCOPE_MESSAGE"
fi

rust_append_scope_args "$SCOPE_JSON"

# Always state the resolved scope and the exact cargo invocation. "Which targets
# ran" was previously only recoverable by reverse-engineering `Running
# deps/<crate>-<hash>` lines out of the log, which is how a root-package-only
# run went unnoticed across three separate scope fixes (#10477).
echo "Rust test scope kind: ${SCOPE_KIND}"
echo "Rust test package selection: ${WORKSPACE_SELECTION}"
echo "Rust test invocation: cargo ${TEST_ARGS[*]}"

# Builds COMMAND_LABEL/COMMAND_BINARY from the current TEST_ARGS. Shared so a
# widened re-run reuses the exact runner selection the first attempt used.
rust_build_test_command() {
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
}

PARSE_RESULTS="${EXTENSION_PATH}/scripts/parse-test-results.sh"

rust_execute_test_run() {
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: ${COMMAND_BINARY[*]} $*"
    fi

    rust_emit_test_plan "$SELECTED_RUNNER" "$COMMAND_LABEL" "$SCOPE_JSON" "started" 0
    homeboy_run_step_capture TEST_TMPFILE TEST_EXIT "$COMMAND_LABEL" -- "${COMMAND_BINARY[@]}" "$@" || true
    rust_emit_test_plan "$SELECTED_RUNNER" "$COMMAND_LABEL" "$SCOPE_JSON" "completed" "$TEST_EXIT"

    # Parse test results for homeboy core (best-effort, non-blocking)
    if [ -n "${HOMEBOY_TEST_RESULTS_FILE:-}" ] && [ -f "$PARSE_RESULTS" ]; then
        bash "$PARSE_RESULTS" "$TEST_TMPFILE" || true
    fi
}

# Reports whether the captured run executed no tests at all. Cargo runs several
# test binaries (unit, integration, doc-tests) and some legitimately have zero
# tests, so only a zero total across every summary line counts.
rust_run_executed_no_tests() {
    local total
    total=$( { grep -Eo '[0-9]+ passed' "$TEST_TMPFILE" || true; } | awk '{s+=$1} END {print s+0}' )
    [ "$total" -eq 0 ]
}

rust_build_test_command
rust_execute_test_run "$@"

# A derived scope that selects zero tests is a scope-derivation gap, not a
# failing test suite: the filter is built from changed file paths, so a module
# mounted with `#[path]` (or any future derivation gap) compiles a filter that
# matches nothing. Failing there reports a red build for code that was never
# executed. Widen to the full suite once instead, so the worst case is a slow
# pass rather than a false failure.
if [ "$TEST_EXIT" -eq 0 ] \
    && [ -n "${HOMEBOY_CHANGED_TEST_FILES:-}" ] \
    && [ "$SCOPE_KIND" != "workspace" ] && [ "$SCOPE_KIND" != "full" ] \
    && rust_run_executed_no_tests; then
    echo ""
    echo "Derived test scope executed no tests; re-running the full test command."
    rm -f "$TEST_TMPFILE"

    TEST_ARGS=(
        test
        --manifest-path "${PROJECT_PATH}/Cargo.toml"
        --workspace
    )
    SCOPE_KIND="full"
    WORKSPACE_SELECTION="--workspace (all members)"
    echo "Rust test invocation: cargo ${TEST_ARGS[*]}"
    SCOPE_JSON="$(printf '%s' "$SCOPE_JSON" | jq -c '.kind = "full" | .args = [] | .reason = "Derived scope executed no tests; widened to the full test command."')"
    rust_build_test_command
    rust_execute_test_run "$@"
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
        python3 "${EXTENSION_PATH}/scripts/parse-test-failures.py" "$PROJECT_PATH" "$TEST_TMPFILE" "$TEST_FAILURES_TMP"
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
#
# A derived scope has already been widened to the full command by this point,
# so reaching here on a changed-files run means the whole suite executed
# nothing — a real configuration problem rather than a scoping gap.
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
