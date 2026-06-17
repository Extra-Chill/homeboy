#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_PRELUDE="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:?HOMEBOY_RUNTIME_RUNNER_PRELUDE is required}"
# shellcheck source=/dev/null
source "$RUNNER_PRELUDE"
homeboy_runner_init --component-alias COMPONENT_PATH --sidecar-writer

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Environment variables:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_SETTINGS_JSON=${HOMEBOY_SETTINGS_JSON:-NOT_SET}"
fi

SETTINGS_JSON="${HOMEBOY_SETTINGS_JSON:-}"

echo "Running Swift tests for: $COMPONENT_ID"
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "Component path: $COMPONENT_PATH"
fi

# Parse test type from settings
TEST_TYPE="script"
if [ -n "${SETTINGS_JSON:-}" ] && [ "$SETTINGS_JSON" != "{}" ]; then
    if command -v jq >/dev/null 2>&1; then
        TEST_TYPE=$(printf '%s' "$SETTINGS_JSON" | jq -r '.test_type // "script"')
    elif printf '%s' "$SETTINGS_JSON" | grep -q '"test_type"[[:space:]]*:[[:space:]]*"xcodebuild"'; then
        TEST_TYPE="xcodebuild"
    fi
fi

# Look for tests directory in component
TEST_DIR="${COMPONENT_PATH}/tests"
if [ ! -d "$TEST_DIR" ]; then
    echo "Error: No tests/ directory found in $COMPONENT_PATH"
    exit 1
fi

if [ "$TEST_TYPE" = "xcodebuild" ]; then
    # XCTest mode - run via xcodebuild
    echo "Running XCTest suite..."

    # Find xcodeproj or xcworkspace
    WORKSPACE=$(find "$COMPONENT_PATH" -maxdepth 1 -name "*.xcworkspace" | head -1)
    PROJECT=$(find "$COMPONENT_PATH" -maxdepth 1 -name "*.xcodeproj" | head -1)

    XCODE_OUTPUT="$(mktemp)"
    trap 'rm -f "$XCODE_OUTPUT"' EXIT
    if [ -n "$WORKSPACE" ]; then
        set +e
        xcodebuild test -workspace "$WORKSPACE" -scheme "$(basename "$WORKSPACE" .xcworkspace)" -destination 'platform=macOS' "$@" 2>&1 | tee "$XCODE_OUTPUT"
        XCODE_EXIT=${PIPESTATUS[0]}
        set -e
    elif [ -n "$PROJECT" ]; then
        set +e
        xcodebuild test -project "$PROJECT" -scheme "$(basename "$PROJECT" .xcodeproj)" -destination 'platform=macOS' "$@" 2>&1 | tee "$XCODE_OUTPUT"
        XCODE_EXIT=${PIPESTATUS[0]}
        set -e
    else
        echo "Error: No Xcode project or workspace found"
        exit 1
    fi

    if [ "$XCODE_EXIT" -ne 0 ] && [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ]; then
        if ! type homeboy_merge_test_failures >/dev/null 2>&1; then
            echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write test failures" >&2
            exit 1
        fi
        TEST_FAILURES_TMP="$(mktemp)"
        python3 - "$COMPONENT_PATH" "$XCODE_OUTPUT" "$TEST_FAILURES_TMP" <<'PY'
import hashlib
import json
import re
import sys

component, output_file, target = sys.argv[1:]
with open(output_file, encoding="utf-8") as handle:
    lines = handle.read().splitlines()

failures = []
pattern = re.compile(r"Test Case '-\[(?P<suite>[^ ]+) (?P<test>[^\]]+)\]' failed(?: \((?P<seconds>[^)]+)\))?\.")
for raw in lines:
    match = pattern.search(raw)
    if not match:
        continue
    suite = match.group("suite")
    test = match.group("test")
    test_id = f"{suite}.{test}"
    identity = f"swift:xctest:{test_id}"
    failures.append({
        "test_id": test_id,
        "suite": suite,
        "file": None,
        "line": None,
        "message": raw.strip(),
        "failure_type": "test_failure",
        "fingerprint": hashlib.sha256(identity.encode()).hexdigest(),
        "stdout_excerpt": "\n".join(lines)[-4000:],
        "stderr_excerpt": "",
    })

if not failures:
    identity = "swift:xcodebuild:failed"
    failures.append({
        "test_id": "xcodebuild test",
        "suite": None,
        "file": None,
        "line": None,
        "message": "xcodebuild test failed before XCTest failures could be parsed",
        "failure_type": "infrastructure",
        "fingerprint": hashlib.sha256(identity.encode()).hexdigest(),
        "stdout_excerpt": "\n".join(lines)[-4000:],
        "stderr_excerpt": "",
    })

with open(target, "w", encoding="utf-8") as handle:
    json.dump(failures, handle, indent=2)
    handle.write("\n")
PY
        homeboy_merge_test_failures "$TEST_FAILURES_TMP"
        rm -f "$TEST_FAILURES_TMP"
    fi
    exit "$XCODE_EXIT"
else
    # Script mode - run .swift files directly
    TESTS_RUN=0
    TESTS_FAILED=0
    FAILURES_FILE="$(mktemp)"
    : > "$FAILURES_FILE"
    trap 'rm -f "$FAILURES_FILE"' EXIT

    for test_file in "$TEST_DIR"/*.swift; do
        if [ -f "$test_file" ]; then
            TESTS_RUN=$((TESTS_RUN + 1))
            TEST_NAME=$(basename "$test_file")
            echo "Running: $TEST_NAME"

            TEST_OUTPUT="$(mktemp)"
            if swift "$test_file" "$TEST_DIR" > "$TEST_OUTPUT" 2>&1; then
                cat "$TEST_OUTPUT"
                echo "  PASS"
                rm -f "$TEST_OUTPUT"
            else
                cat "$TEST_OUTPUT"
                echo "  FAIL"
                TESTS_FAILED=$((TESTS_FAILED + 1))
                printf '%s\t%s\n' "$TEST_NAME" "$TEST_OUTPUT" >> "$FAILURES_FILE"
            fi
        fi
    done

    if [ $TESTS_RUN -eq 0 ]; then
        echo "Warning: No .swift test files found in $TEST_DIR"
        exit 0
    fi

    echo ""
    echo "Results: $((TESTS_RUN - TESTS_FAILED))/$TESTS_RUN tests passed"

    if [ $TESTS_FAILED -gt 0 ]; then
        if [ -n "${HOMEBOY_TEST_FAILURES_FILE:-}" ]; then
            if ! type homeboy_merge_test_failures >/dev/null 2>&1; then
                echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write test failures" >&2
                exit 1
            fi
            TEST_FAILURES_TMP="$(mktemp)"
            python3 - "$FAILURES_FILE" "$TEST_FAILURES_TMP" <<'PY'
import hashlib
import json
import sys

failures_file, target = sys.argv[1:]
failures = []
with open(failures_file, encoding="utf-8") as handle:
    for raw in handle:
        name, _, output_path = raw.rstrip("\n").partition("\t")
        if not name or not output_path:
            continue
        try:
            with open(output_path, encoding="utf-8") as output_handle:
                output = output_handle.read()
        except OSError:
            output = ""
        identity = f"swift:script:{name}"
        failures.append({
            "test_id": name,
            "suite": "script",
            "file": f"tests/{name}",
            "line": None,
            "message": f"Swift script test failed: {name}",
            "failure_type": "test_failure",
            "fingerprint": hashlib.sha256(identity.encode()).hexdigest(),
            "stdout_excerpt": output[-4000:],
            "stderr_excerpt": "",
        })

with open(target, "w", encoding="utf-8") as handle:
    json.dump(failures, handle, indent=2)
    handle.write("\n")
PY
            homeboy_merge_test_failures "$TEST_FAILURES_TMP"
            rm -f "$TEST_FAILURES_TMP"
        fi
        while IFS=$'\t' read -r _ output_path; do
            [ -n "$output_path" ] && rm -f "$output_path"
        done < "$FAILURES_FILE"
        exit 1
    fi
fi

echo "All Swift tests passed"
