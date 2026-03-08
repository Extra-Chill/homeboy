#!/usr/bin/env bash
set -euo pipefail

# Rust lint runner for homeboy lint.
#
# Runs cargo fmt --check and cargo clippy as lint steps.
# Supports the standard homeboy extension env vars:
#   HOMEBOY_EXTENSION_PATH  — path to this extension
#   HOMEBOY_COMPONENT_PATH  — path to the Rust project
#   HOMEBOY_AUTO_FIX        — if "1", run cargo fmt (fix mode) instead of --check
#   HOMEBOY_SUMMARY_MODE    — if "1", show compact output
#   HOMEBOY_CHANGED_SINCE   — git ref to scope fmt check to changed files only
#   HOMEBOY_LINT_GLOB       — file glob (currently unused for Rust — cargo operates on crates)
#   HOMEBOY_LINT_FILE       — single file (currently unused for Rust)
#   HOMEBOY_ERRORS_ONLY     — if "1", only show errors (suppresses warnings in clippy)
#   HOMEBOY_STEP            — comma-separated steps to run (fmt, clippy)
#   HOMEBOY_SKIP            — comma-separated steps to skip
#   HOMEBOY_DEBUG           — if "1", show debug output

FAILED_STEP=""
FAILURE_OUTPUT=""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${SCRIPT_DIR}/lib/runner-steps.sh}"
# shellcheck source=./lib/runner-steps.sh
source "${RUNNER_STEPS_HELPER}"

print_failure_summary() {
    if [ -n "$FAILED_STEP" ]; then
        echo ""
        echo "============================================"
        echo "BUILD FAILED: $FAILED_STEP"
        echo "============================================"
        if [ -n "$FAILURE_OUTPUT" ]; then
            echo ""
            echo "Error details:"
            echo "$FAILURE_OUTPUT"
        fi
    fi
}
trap print_failure_summary EXIT

# Determine project path
if [ -n "${HOMEBOY_COMPONENT_PATH:-}" ]; then
    PROJECT_PATH="${HOMEBOY_COMPONENT_PATH}"
else
    PROJECT_PATH="$(pwd)"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Rust Lint Environment:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_AUTO_FIX=${HOMEBOY_AUTO_FIX:-NOT_SET}"
    echo "HOMEBOY_SUMMARY_MODE=${HOMEBOY_SUMMARY_MODE:-NOT_SET}"
    echo "HOMEBOY_ERRORS_ONLY=${HOMEBOY_ERRORS_ONLY:-NOT_SET}"
    echo "PROJECT_PATH=${PROJECT_PATH}"
fi

# Verify this is a Rust project
if [ ! -f "${PROJECT_PATH}/Cargo.toml" ]; then
    echo "Error: No Cargo.toml found at ${PROJECT_PATH}"
    echo "Not a Rust project — cannot run lint."
    exit 1
fi

echo "Running Rust lint checks..."

# ── Step 1: cargo fmt ──
if should_run_step "fmt"; then
    if [ "${HOMEBOY_AUTO_FIX:-}" = "1" ]; then
        echo ""
        echo "Running cargo fmt (fix mode)..."
        set +e
        FMT_OUTPUT=$(cargo fmt --manifest-path "${PROJECT_PATH}/Cargo.toml" 2>&1)
        FMT_EXIT=$?
        set -e

        if [ $FMT_EXIT -eq 0 ]; then
            echo "cargo fmt: applied formatting fixes"
        else
            echo "cargo fmt failed:"
            echo "$FMT_OUTPUT"
            FAILED_STEP="cargo fmt"
            FAILURE_OUTPUT="$FMT_OUTPUT"
            exit 1
        fi
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
                ' > "${HOMEBOY_ANNOTATIONS_DIR}/rustfmt.json" 2>/dev/null || true
                [ -s "${HOMEBOY_ANNOTATIONS_DIR}/rustfmt.json" ] || rm -f "${HOMEBOY_ANNOTATIONS_DIR}/rustfmt.json"
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

    # In fix mode, apply clippy suggestions
    if [ "${HOMEBOY_AUTO_FIX:-}" = "1" ]; then
        CLIPPY_ARGS+=(--fix --allow-dirty --allow-staged)
    fi

    CLIPPY_ARGS+=(--)

    if [ "${HOMEBOY_ERRORS_ONLY:-}" = "1" ]; then
        CLIPPY_ARGS+=(-D warnings)
    else
        CLIPPY_ARGS+=(-W clippy::all)
    fi

    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: cargo ${CLIPPY_ARGS[*]}"
    fi

    CLIPPY_TMPFILE=$(mktemp)

    set +e
    cargo "${CLIPPY_ARGS[@]}" 2>&1 | tee "$CLIPPY_TMPFILE"
    CLIPPY_EXIT=${PIPESTATUS[0]}
    set -e

    CLIPPY_OUTPUT=$(cat "$CLIPPY_TMPFILE")
    rm -f "$CLIPPY_TMPFILE"

    # Write annotations sidecar JSON for CI inline comments
    # Parse clippy's "warning: message\n  --> file:line:col" format
    if [ -n "${HOMEBOY_ANNOTATIONS_DIR:-}" ] && [ -d "${HOMEBOY_ANNOTATIONS_DIR}" ]; then
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
        ' > "${HOMEBOY_ANNOTATIONS_DIR}/clippy.json" 2>/dev/null || true
        # Remove empty file if no annotations were written
        [ -s "${HOMEBOY_ANNOTATIONS_DIR}/clippy.json" ] || rm -f "${HOMEBOY_ANNOTATIONS_DIR}/clippy.json"
    fi

    if [ $CLIPPY_EXIT -eq 0 ]; then
        echo "cargo clippy: passed"
    else
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

echo ""
echo "Rust lint checks passed"
