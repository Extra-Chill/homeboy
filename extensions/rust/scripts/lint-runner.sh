#!/usr/bin/env bash
set -euo pipefail

# Rust lint runner for homeboy lint.
#
# Runs cargo fmt --check and cargo clippy as lint steps.
# Supports the standard homeboy extension env vars:
#   HOMEBOY_EXTENSION_PATH  — path to this extension
#   HOMEBOY_COMPONENT_PATH  — path to the Rust project
#   HOMEBOY_FIX_ONLY        — if "1", run cargo fmt + clippy --fix + cargo fix,
#                             then exit without the validation pass (the engine
#                             runs validation separately). Sent by
#                             `homeboy refactor --from lint --write`.
#   HOMEBOY_SUMMARY_MODE    — if "1", show compact output
#   HOMEBOY_CHANGED_SINCE   — git ref to scope fmt check to changed files only
#   HOMEBOY_LINT_GLOB       — file glob (currently unused for Rust — cargo operates on crates)
#   HOMEBOY_LINT_FILE       — single file (currently unused for Rust)
#   HOMEBOY_ERRORS_ONLY     — if "1", treat warnings as errors with `-D warnings`
#   HOMEBOY_CLIPPY_ALL      — if "1", opt into `-W clippy::all` warning expansion
#   HOMEBOY_STEP            — comma-separated steps to run (fmt, clippy)
#   HOMEBOY_SKIP            — comma-separated steps to skip
#   HOMEBOY_DEBUG           — if "1", show debug output
#   HOMEBOY_FIX_RESULTS_FILE — JSON sidecar receiving applied fix records

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHARED_LIB_DIR="${HOMEBOY_SHARED_LIB_DIR:-}"
if [ -z "$SHARED_LIB_DIR" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
    SHARED_LIB_DIR="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
fi
SHARED_LIB_DIR="${SHARED_LIB_DIR:-$(cd "${SCRIPT_DIR}/../../scripts/lib" && pwd)}"
FIX_RESULTS_HELPER="${HOMEBOY_RUNTIME_FIX_RESULTS:-${SHARED_LIB_DIR}/fix-results.sh}"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/runner-harness.sh"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/lint-findings-adapter.sh"
homeboy_runner_harness_init --steps --failure-trap --sidecar-writer
homeboy_lint_findings_init
# shellcheck source=/dev/null
homeboy_runner_harness_source_command_capture
# shellcheck source=../../scripts/lib/fix-results.sh
source "$FIX_RESULTS_HELPER"

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Rust Lint Environment:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_FIX_ONLY=${HOMEBOY_FIX_ONLY:-NOT_SET}"
    echo "HOMEBOY_SUMMARY_MODE=${HOMEBOY_SUMMARY_MODE:-NOT_SET}"
    echo "HOMEBOY_ERRORS_ONLY=${HOMEBOY_ERRORS_ONLY:-NOT_SET}"
    echo "PROJECT_PATH=${PROJECT_PATH}"
fi

write_fix_results_sidecar() {
    if [ "${HOMEBOY_FIX_ONLY:-}" = "1" ] && [ -n "${HOMEBOY_FIX_RESULTS_FILE:-}" ]; then
        homeboy_fix_results_write
    fi
}

# Register before any harness temporary so the harness composes this sidecar.
trap write_fix_results_sidecar EXIT

clippy_all_enabled() {
    if [ "${HOMEBOY_CLIPPY_ALL:-}" = "1" ]; then
        return 0
    fi

    python3 - <<'PY'
import json
import os
import sys

try:
    settings = json.loads(os.environ.get("HOMEBOY_SETTINGS_JSON", "{}"))
except json.JSONDecodeError:
    settings = {}

enabled = settings.get("clippy_all") is True
sys.exit(0 if enabled else 1)
PY
}

write_lint_findings_from_output() {
    local tool="$1"
    local output_file="$2"

    if [ -z "${HOMEBOY_LINT_FINDINGS_FILE:-}" ] || [ ! -f "$output_file" ]; then
        return 0
    fi

    local findings_file
    homeboy_runner_harness_temp findings_file "homeboy-rust-lint-findings.XXXXXX"

    python3 - "$PROJECT_PATH" "$tool" "$output_file" "$findings_file" <<'PY'
import hashlib
import json
import os
import re
import sys

project, tool, output_file, target = sys.argv[1:]
try:
    with open(target, encoding="utf-8") as handle:
        findings = json.load(handle)
        if not isinstance(findings, list):
            findings = []
except (OSError, json.JSONDecodeError):
    findings = []

def excerpt(file, line):
    try:
        with open(os.path.join(project, file), encoding="utf-8") as handle:
            lines = handle.read().splitlines()
        if 1 <= line <= len(lines):
            return lines[line - 1][:240]
    except OSError:
        return None
    return None

with open(output_file, encoding="utf-8") as handle:
    lines = handle.read().splitlines()

if tool == "rustfmt":
    pattern = re.compile(r"^Diff in (?P<file>.+) at line (?P<line>\d+):")
    for raw in lines:
        match = pattern.match(raw)
        if not match:
            continue
        file = match.group("file")
        if os.path.isabs(file):
            file = os.path.relpath(file, project)
        line = int(match.group("line"))
        identity = f"rust:rustfmt:{file}:{line}"
        findings.append({
            "id": identity,
            "file": file,
            "line": line,
            "column": 1,
            "severity": "warning",
            "source": "rustfmt",
            "code": "formatting",
            "category": "format",
            "message": "File needs formatting",
            "fixable": True,
            "fingerprint": hashlib.sha1(identity.encode()).hexdigest(),
            "excerpt": excerpt(file, line),
        })
elif tool == "clippy":
    pending = None
    diagnostic = re.compile(r"^(?P<severity>warning|error)(\[(?P<code>[^\]]+)\])?: (?P<message>.+)$")
    location = re.compile(r"^\s+--> (?P<file>[^:\n]+):(?P<line>\d+):(?P<column>\d+)")
    for raw in lines:
        match = diagnostic.match(raw)
        if match:
            pending = match.groupdict()
            continue
        match = location.match(raw)
        if not match or not pending:
            continue
        file = match.group("file")
        line = int(match.group("line"))
        column = int(match.group("column"))
        code = pending.get("code") or "clippy"
        message = pending.get("message") or "clippy finding"
        severity = pending.get("severity") or "warning"
        identity = f"rust:clippy:{file}:{line}:{column}:{code}:{message}"
        findings.append({
            "id": identity,
            "file": file,
            "line": line,
            "column": column,
            "severity": severity,
            "source": "clippy",
            "code": code,
            "category": "correctness",
            "message": message,
            "fixable": False,
            "fingerprint": hashlib.sha1(identity.encode()).hexdigest(),
            "excerpt": excerpt(file, line),
        })
        pending = None

with open(target, "w", encoding="utf-8") as handle:
    json.dump(findings, handle, indent=2)
    handle.write("\n")
PY
    homeboy_lint_findings_merge_file "$findings_file"
}

# Verify this is a Rust project
if [ ! -f "${PROJECT_PATH}/Cargo.toml" ]; then
    echo "Error: No Cargo.toml found at ${PROJECT_PATH}"
    echo "Not a Rust project — cannot run lint."
    exit 1
fi

echo "Running Rust lint checks..."

# ── Step 1: cargo fmt ──
if should_run_step "fmt"; then
    if [ "${HOMEBOY_FIX_ONLY:-}" = "1" ]; then
        echo ""
        echo "Running cargo fmt (fix mode)..."
        FMT_BEFORE="$(mktemp)"
        homeboy_fix_results_capture "$FMT_BEFORE" "$PROJECT_PATH" '*.rs'
        set +e
        FMT_OUTPUT=$(cargo fmt --manifest-path "${PROJECT_PATH}/Cargo.toml" 2>&1)
        FMT_EXIT=$?
        set -e

        if [ $FMT_EXIT -eq 0 ]; then
            homeboy_fix_results_append_changed "rustfmt" "format" "$FMT_BEFORE" "" "$PROJECT_PATH" '*.rs'
            echo "cargo fmt: applied formatting fixes"
        else
            rm -f "$FMT_BEFORE"
            echo "cargo fmt failed:"
            echo "$FMT_OUTPUT"
            FAILED_STEP="cargo fmt"
            FAILURE_OUTPUT="$FMT_OUTPUT"
            exit 1
        fi
        rm -f "$FMT_BEFORE"
    else
        # Determine whether to scope fmt to changed files only.
        # When HOMEBOY_CHANGED_SINCE is set (CI), only check files the PR
        # actually changed — don't fail on pre-existing formatting debt.
        SCOPED_FMT=0
        CHANGED_RS_FILES=()
        if [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
            mapfile -t CHANGED_RS_FILES < <(
                git -C "${PROJECT_PATH}" diff --name-only --diff-filter=ACMR \
                    "${HOMEBOY_CHANGED_SINCE}" -- '*.rs' 2>/dev/null || true
            )
            if [ ${#CHANGED_RS_FILES[@]} -gt 0 ]; then
                SCOPED_FMT=1
            fi
        fi

        if [ "$SCOPED_FMT" = "1" ]; then
            echo ""
            echo "Running rustfmt --check on ${#CHANGED_RS_FILES[@]} changed files..."
            set +e
            # Build absolute paths for rustfmt
            FMT_TARGETS=()
            for f in "${CHANGED_RS_FILES[@]}"; do
                FMT_TARGETS+=("${PROJECT_PATH}/${f}")
            done
            FMT_OUTPUT=$(rustfmt --check --edition 2021 "${FMT_TARGETS[@]}" 2>&1)
            FMT_EXIT=$?
            set -e
        else
            echo ""
            echo "Running cargo fmt --check..."
            set +e
            FMT_OUTPUT=$(cargo fmt --manifest-path "${PROJECT_PATH}/Cargo.toml" --check 2>&1)
            FMT_EXIT=$?
            set -e
        fi

        if [ $FMT_EXIT -eq 0 ]; then
            echo "cargo fmt: passed"
        else
            if [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ]; then
                FMT_FINDINGS_TMP="$(mktemp)"
                printf '%s\n' "$FMT_OUTPUT" > "$FMT_FINDINGS_TMP"
                write_lint_findings_from_output "rustfmt" "$FMT_FINDINGS_TMP"
                rm -f "$FMT_FINDINGS_TMP"
            fi

            if [ "${HOMEBOY_SUMMARY_MODE:-}" = "1" ]; then
                # Count files with formatting issues
                FILE_COUNT=$(echo "$FMT_OUTPUT" | grep -c "^Diff in" || true)
                echo ""
                echo "============================================"
                echo "FMT SUMMARY: ${FILE_COUNT} files need formatting"
                echo "============================================"
                echo ""
                echo "Fix: homeboy lint <component> --fix"
            else
                echo ""
                echo "$FMT_OUTPUT"
            fi

            # Write annotations sidecar for fmt issues
            # Parse "Diff in /path/to/file.rs at line N:" format
            if [ -n "${HOMEBOY_ANNOTATIONS_DIR:-}" ] && [ -d "${HOMEBOY_ANNOTATIONS_DIR}" ]; then
                if ! type homeboy_sidecar_merge >/dev/null 2>&1; then
                    echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write annotations" >&2
                    exit 1
                fi
                FMT_ANNOTATIONS_TMP="$(mktemp)"
                echo "$FMT_OUTPUT" | awk -v comp_path="${PROJECT_PATH}/" '
                    /^Diff in .+ at line [0-9]+:/ {
                        file = $3
                        line = $6
                        sub(/:$/, "", line)
                        # Strip component path prefix
                        sub(comp_path, "", file)
                        gsub(/"/, "\\\"", file)
                        annotations = annotations (annotations ? ",\n" : "") \
                            "  {\"file\": \"" file "\", \"line\": " line ", \"message\": \"File needs formatting (run homeboy lint --fix)\", \"source\": \"rustfmt\", \"severity\": \"warning\", \"code\": \"formatting\"}"
                    }
                    END {
                        if (annotations) {
                            print "[\n" annotations "\n]"
                        }
                    }
                ' > "$FMT_ANNOTATIONS_TMP" 2>/dev/null || true
                homeboy_sidecar_merge annotation.rustfmt "$FMT_ANNOTATIONS_TMP"
                rm -f "$FMT_ANNOTATIONS_TMP"
            fi

            FAILED_STEP="cargo fmt --check"
            FAILURE_OUTPUT="$(echo "$FMT_OUTPUT" | tail -20)"
            exit 1
        fi
    fi
else
    echo "Skipping cargo fmt (step filter)"
fi

# ── Step 2: cargo clippy ──
if should_run_step "clippy"; then
    echo ""
    echo "Running cargo clippy..."

    CLIPPY_ARGS=(
        clippy
        --manifest-path "${PROJECT_PATH}/Cargo.toml"
        --all-targets
    )

    # In fix-only mode, apply clippy suggestions
    if [ "${HOMEBOY_FIX_ONLY:-}" = "1" ]; then
        CLIPPY_ARGS+=(--fix --allow-dirty --allow-staged)
    fi

    CLIPPY_ARGS+=(--)

    if [ "${HOMEBOY_ERRORS_ONLY:-}" = "1" ]; then
        CLIPPY_ARGS+=(-D warnings)
    elif clippy_all_enabled; then
        CLIPPY_ARGS+=(-W clippy::all)
    else
        :
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: cargo ${CLIPPY_ARGS[*]}"
    fi

    CLIPPY_BEFORE=""
    if [ "${HOMEBOY_FIX_ONLY:-}" = "1" ]; then
        CLIPPY_BEFORE="$(mktemp)"
        homeboy_fix_results_capture "$CLIPPY_BEFORE" "$PROJECT_PATH" '*.rs'
    fi

    homeboy_run_step_capture CLIPPY_TMPFILE CLIPPY_EXIT "cargo clippy" -- cargo "${CLIPPY_ARGS[@]}" || true

    CLIPPY_OUTPUT=$(cat "$CLIPPY_TMPFILE")
    homeboy_cleanup_step_capture "$CLIPPY_TMPFILE"

    # Write annotations sidecar JSON for CI inline comments
    # Parse clippy's "warning: message\n  --> file:line:col" format
    if [ -n "${HOMEBOY_ANNOTATIONS_DIR:-}" ] && [ -d "${HOMEBOY_ANNOTATIONS_DIR}" ]; then
        if ! type homeboy_sidecar_merge >/dev/null 2>&1; then
            echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write annotations" >&2
            exit 1
        fi
        CLIPPY_ANNOTATIONS_TMP="$(mktemp)"
        # Use awk to pair "warning/error" lines with their "--> file:line:col" location
        echo "$CLIPPY_OUTPUT" | awk '
            /^(warning|error)(\[.+\])?: / {
                severity = ($1 == "error" || $1 ~ /^error/) ? "error" : "warning"
                # Extract code from brackets: warning[clippy::foo] or error[E0001]
                code = ""
                if (match($0, /\[([^\]]+)\]/, m)) { code = m[1] }
                # Message is everything after "warning: " or "error: " or "error[...]: "
                msg = $0
                sub(/^(warning|error)(\[[^\]]+\])?: /, "", msg)
                next_is_location = 1
                next
            }
            next_is_location && /^\s+-->/ {
                # Parse "  --> src/foo.rs:42:10"
                loc = $2
                split(loc, parts, ":")
                file = parts[1]
                line = parts[2]
                if (file != "" && line != "") {
                    # Escape quotes in message for JSON
                    gsub(/"/, "\\\"", msg)
                    annotations = annotations (annotations ? ",\n" : "") \
                        "  {\"file\": \"" file "\", \"line\": " line ", \"message\": \"" msg "\", \"source\": \"clippy\", \"severity\": \"" severity "\", \"code\": \"" code "\"}"
                }
                next_is_location = 0
                next
            }
            { next_is_location = 0 }
            END {
                if (annotations) {
                    print "[\n" annotations "\n]"
                }
            }
        ' > "$CLIPPY_ANNOTATIONS_TMP" 2>/dev/null || true
        homeboy_sidecar_merge annotation.clippy "$CLIPPY_ANNOTATIONS_TMP"
        rm -f "$CLIPPY_ANNOTATIONS_TMP"
    fi

    if [ $CLIPPY_EXIT -eq 0 ]; then
        if [ -n "$CLIPPY_BEFORE" ]; then
            homeboy_fix_results_append_changed "clippy" "rewrite" "$CLIPPY_BEFORE" "" "$PROJECT_PATH" '*.rs'
            rm -f "$CLIPPY_BEFORE"
        fi
        echo "cargo clippy: passed"
    else
        if [ -n "$CLIPPY_BEFORE" ]; then
            rm -f "$CLIPPY_BEFORE"
        fi
        if [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ]; then
            CLIPPY_FINDINGS_TMP="$(mktemp)"
            printf '%s\n' "$CLIPPY_OUTPUT" > "$CLIPPY_FINDINGS_TMP"
            write_lint_findings_from_output "clippy" "$CLIPPY_FINDINGS_TMP"
            rm -f "$CLIPPY_FINDINGS_TMP"
        fi

        if [ "${HOMEBOY_SUMMARY_MODE:-}" = "1" ]; then
            WARNING_COUNT=$(echo "$CLIPPY_OUTPUT" | grep -c "^warning\[" || true)
            ERROR_COUNT=$(echo "$CLIPPY_OUTPUT" | grep -c "^error\[" || true)
            echo ""
            echo "============================================"
            echo "CLIPPY SUMMARY: ${ERROR_COUNT} errors, ${WARNING_COUNT} warnings"
            echo "============================================"
        fi
        FAILED_STEP="cargo clippy"
        FAILURE_OUTPUT="$(echo "$CLIPPY_OUTPUT" | grep -E "^(error|warning)\[" | head -20)"
        exit 1
    fi
else
    echo "Skipping cargo clippy (step filter)"
fi

# ── Step 3: cargo fix (compiler warnings) ──
# Only in fix mode — applies compiler suggestions for dead_code, unused_imports,
# unused_variables, etc. These are the warnings that `cargo check` reports.
if should_run_step "fix"; then
    if [ "${HOMEBOY_FIX_ONLY:-}" = "1" ]; then
        echo ""
        echo "Running cargo fix (compiler warnings)..."
        FIX_BEFORE="$(mktemp)"
        homeboy_fix_results_capture "$FIX_BEFORE" "$PROJECT_PATH" '*.rs'

        set +e
        FIX_OUTPUT=$(cargo fix \
            --manifest-path "${PROJECT_PATH}/Cargo.toml" \
            --allow-dirty --allow-staged \
            --lib --bins 2>&1)
        FIX_EXIT=$?
        set -e

        if [ $FIX_EXIT -eq 0 ]; then
            homeboy_fix_results_append_changed "cargo_fix" "rewrite" "$FIX_BEFORE" "" "$PROJECT_PATH" '*.rs'
            echo "cargo fix: applied compiler warning fixes"
        else
            homeboy_fix_results_append_changed "cargo_fix" "rewrite" "$FIX_BEFORE" "" "$PROJECT_PATH" '*.rs'
            # cargo fix failure is non-fatal in fix-only mode — some warnings
            # can't be auto-fixed (e.g., dead_code on pub items).
            echo "cargo fix: exited non-zero (${FIX_EXIT}), continuing"
            if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
                echo "DEBUG: cargo fix output:"
                echo "$FIX_OUTPUT"
            fi
        fi
        rm -f "$FIX_BEFORE"
    else
        echo ""
        echo "Skipping cargo fix (only runs in fix-only mode)"
    fi
else
    echo "Skipping cargo fix (step filter)"
fi

echo ""
echo "Rust lint checks passed"
