#!/bin/bash
set -euo pipefail

# Standalone PHP linting script using PHPCS/PHPCBF
# Supports auto-fix mode via HOMEBOY_AUTO_FIX=1
# Supports summary mode via HOMEBOY_SUMMARY_MODE=1

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Environment variables:"
    echo "HOMEBOY_MODULE_PATH=${HOMEBOY_MODULE_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_AUTO_FIX=${HOMEBOY_AUTO_FIX:-NOT_SET}"
    echo "HOMEBOY_SUMMARY_MODE=${HOMEBOY_SUMMARY_MODE:-NOT_SET}"
fi

# Determine execution context
if [ -n "${HOMEBOY_MODULE_PATH:-}" ]; then
    MODULE_PATH="${HOMEBOY_MODULE_PATH}"
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-.}"
    PLUGIN_PATH="$COMPONENT_PATH"
else
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    MODULE_PATH="$(dirname "$SCRIPT_DIR")"
    COMPONENT_PATH="$(pwd)"
    PLUGIN_PATH="$COMPONENT_PATH"
fi

echo "Running PHP linting..."
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "Module path: $MODULE_PATH"
    echo "Plugin path: $PLUGIN_PATH"
    echo "Auto-fix: ${HOMEBOY_AUTO_FIX:-0}"
fi

PHPCS_BIN="${MODULE_PATH}/vendor/bin/phpcs"
PHPCBF_BIN="${MODULE_PATH}/vendor/bin/phpcbf"
YODA_FIXER="${MODULE_PATH}/scripts/yoda-fixer.php"
IN_ARRAY_FIXER="${MODULE_PATH}/scripts/in-array-strict-fixer.php"
SHORT_TERNARY_FIXER="${MODULE_PATH}/scripts/short-ternary-fixer.php"
PHPCS_CONFIG="${MODULE_PATH}/phpcs.xml.dist"

# Validate tools exist
if [ ! -f "$PHPCS_BIN" ]; then
    echo "Error: phpcs not found at $PHPCS_BIN"
    exit 1
fi

if [ ! -f "$PHPCS_CONFIG" ]; then
    echo "Error: phpcs.xml.dist not found at $PHPCS_CONFIG"
    exit 1
fi

# Auto-detect text domain from plugin header
TEXT_DOMAIN_ARG=""
MAIN_PLUGIN_FILE=$(find "$PLUGIN_PATH" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)
if [ -n "$MAIN_PLUGIN_FILE" ]; then
    TEXT_DOMAIN=$(grep -m1 "Text Domain:" "$MAIN_PLUGIN_FILE" 2>/dev/null | sed 's/.*Text Domain:[[:space:]]*//' | tr -d ' \r')
    if [ -n "$TEXT_DOMAIN" ]; then
        TEXT_DOMAIN_ARG="--runtime-set text_domain $TEXT_DOMAIN"
        if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
            echo "DEBUG: Detected text domain: $TEXT_DOMAIN"
        fi
    fi
fi

# Auto-fix mode: run custom fixers, then phpcbf, then phpcs
if [[ "${HOMEBOY_AUTO_FIX:-}" == "1" ]]; then
    # Run Yoda condition fixer (handles cases phpcbf can't fix)
    if [ -f "$YODA_FIXER" ]; then
        php "$YODA_FIXER" "$PLUGIN_PATH"
    fi

    # Run in_array strict fixer (add true as third param)
    if [ -f "$IN_ARRAY_FIXER" ]; then
        php "$IN_ARRAY_FIXER" "$PLUGIN_PATH"
    fi

    # Run short ternary fixer (expand ?: to ? : for simple vars)
    if [ -f "$SHORT_TERNARY_FIXER" ]; then
        php "$SHORT_TERNARY_FIXER" "$PLUGIN_PATH"
    fi

    # Run phpcbf for remaining auto-fixable issues
    if [ -f "$PHPCBF_BIN" ]; then
        echo "Running auto-fix (phpcbf)..."
        # phpcbf exit codes: 0=no changes, 1=changes made, 2=some errors unfixable
        set +e
        # shellcheck disable=SC2086
        "$PHPCBF_BIN" --standard="$PHPCS_CONFIG" $TEXT_DOMAIN_ARG "$PLUGIN_PATH"
        PHPCBF_EXIT=$?
        set -e

        if [ "$PHPCBF_EXIT" -eq 2 ]; then
            echo ""
            echo "WARNING: Some errors could not be auto-fixed."
            echo "Common unfixable issues:"
            echo "  - Complex Yoda conditions (method calls, expressions)"
            echo "  - Translator comments requiring context"
        fi
        echo ""
    else
        echo "Warning: phpcbf not found, skipping auto-fix"
    fi
fi

# Summary mode: use --report=summary for compact output
REPORT_ARG=""
if [[ "${HOMEBOY_SUMMARY_MODE:-}" == "1" ]]; then
    REPORT_ARG="--report=summary"
fi

# Validation
echo "Validating with PHPCS..."
# shellcheck disable=SC2086
if "$PHPCS_BIN" --standard="$PHPCS_CONFIG" $TEXT_DOMAIN_ARG $REPORT_ARG "$PLUGIN_PATH"; then
    echo "PHPCS linting passed"
    exit 0
else
    echo "PHPCS linting failed"
    exit 1
fi
