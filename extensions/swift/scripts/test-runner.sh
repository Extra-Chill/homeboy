#!/bin/bash
set -euo pipefail

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Environment variables:"
    echo "HOMEBOY_MODULE_PATH=${HOMEBOY_MODULE_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_SETTINGS_JSON=${HOMEBOY_SETTINGS_JSON:-NOT_SET}"
fi

# Determine execution context
if [ -n "${HOMEBOY_MODULE_PATH:-}" ]; then
    MODULE_PATH="${HOMEBOY_MODULE_PATH}"
    COMPONENT_ID="${HOMEBOY_COMPONENT_ID:-unknown}"
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-.}"
    SETTINGS_JSON="${HOMEBOY_SETTINGS_JSON:-}"
else
    # Called directly
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    MODULE_PATH="$(dirname "$SCRIPT_DIR")"
    COMPONENT_PATH="$(pwd)"
    COMPONENT_ID="$(basename "$COMPONENT_PATH")"
fi

echo "Running Swift tests for: $COMPONENT_ID"
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "Component path: $COMPONENT_PATH"
fi

# Parse test type from settings
TEST_TYPE="script"
if [ -n "${SETTINGS_JSON:-}" ] && [ "$SETTINGS_JSON" != "{}" ]; then
    TEST_TYPE=$(printf '%s' "$SETTINGS_JSON" | jq -r '.test_type // "script"')
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

    if [ -n "$WORKSPACE" ]; then
        xcodebuild test -workspace "$WORKSPACE" -scheme "$(basename "$WORKSPACE" .xcworkspace)" -destination 'platform=macOS' "$@"
    elif [ -n "$PROJECT" ]; then
        xcodebuild test -project "$PROJECT" -scheme "$(basename "$PROJECT" .xcodeproj)" -destination 'platform=macOS' "$@"
    else
        echo "Error: No Xcode project or workspace found"
        exit 1
    fi
else
    # Script mode - run .swift files directly
    TESTS_RUN=0
    TESTS_FAILED=0

    for test_file in "$TEST_DIR"/*.swift; do
        if [ -f "$test_file" ]; then
            TESTS_RUN=$((TESTS_RUN + 1))
            TEST_NAME=$(basename "$test_file")
            echo "Running: $TEST_NAME"

            if swift "$test_file" "$TEST_DIR" 2>&1; then
                echo "  PASS"
            else
                echo "  FAIL"
                TESTS_FAILED=$((TESTS_FAILED + 1))
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
        exit 1
    fi
fi

echo "All Swift tests passed"
