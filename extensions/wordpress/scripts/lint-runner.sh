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

# Determine lint target (file, glob, or full component)
# Use array to properly handle paths with spaces
LINT_FILES=("$PLUGIN_PATH")

if [ -n "${HOMEBOY_LINT_FILE:-}" ]; then
    LINT_FILES=("${PLUGIN_PATH}/${HOMEBOY_LINT_FILE}")
    if [ ! -f "${LINT_FILES[0]}" ]; then
        echo "Error: File not found: ${LINT_FILES[0]}"
        exit 1
    fi
    echo "Linting single file: ${HOMEBOY_LINT_FILE}"
elif [ -n "${HOMEBOY_LINT_GLOB:-}" ]; then
    cd "$PLUGIN_PATH"
    shopt -s nullglob globstar
    MATCHED_FILES=( ${HOMEBOY_LINT_GLOB} )
    shopt -u nullglob globstar

    if [ ${#MATCHED_FILES[@]} -eq 0 ]; then
        echo "Error: No files match pattern: ${HOMEBOY_LINT_GLOB}"
        exit 1
    fi

    echo "Linting ${#MATCHED_FILES[@]} files matching: ${HOMEBOY_LINT_GLOB}"
    LINT_FILES=("${MATCHED_FILES[@]}")
    cd - > /dev/null
else
    echo "Running PHP linting..."
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "Module path: $MODULE_PATH"
    echo "Plugin path: $PLUGIN_PATH"
    echo "Lint files: ${LINT_FILES[*]}"
    echo "Auto-fix: ${HOMEBOY_AUTO_FIX:-0}"
fi

PHPCS_BIN="${MODULE_PATH}/vendor/bin/phpcs"
PHPCBF_BIN="${MODULE_PATH}/vendor/bin/phpcbf"
YODA_FIXER="${MODULE_PATH}/scripts/yoda-fixer.php"
IN_ARRAY_FIXER="${MODULE_PATH}/scripts/in-array-strict-fixer.php"
SHORT_TERNARY_FIXER="${MODULE_PATH}/scripts/short-ternary-fixer.php"
ESCAPE_I18N_FIXER="${MODULE_PATH}/scripts/escape-i18n-fixer.php"
ECHO_TRANSLATE_FIXER="${MODULE_PATH}/scripts/echo-translate-fixer.php"
SAFE_REDIRECT_FIXER="${MODULE_PATH}/scripts/safe-redirect-fixer.php"
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
TEXT_DOMAIN=""
MAIN_PLUGIN_FILE=$(find "$PLUGIN_PATH" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)
if [ -n "$MAIN_PLUGIN_FILE" ]; then
    TEXT_DOMAIN=$(grep -m1 "Text Domain:" "$MAIN_PLUGIN_FILE" 2>/dev/null | sed 's/.*Text Domain:[[:space:]]*//' | tr -d ' \r')
    if [ -n "$TEXT_DOMAIN" ] && [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Detected text domain: $TEXT_DOMAIN"
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

    # Run escape i18n fixer (_e -> esc_html_e)
    if [ -f "$ESCAPE_I18N_FIXER" ]; then
        php "$ESCAPE_I18N_FIXER" "$PLUGIN_PATH"
    fi

    # Run echo translate fixer (echo __() -> echo esc_html__())
    if [ -f "$ECHO_TRANSLATE_FIXER" ]; then
        php "$ECHO_TRANSLATE_FIXER" "$PLUGIN_PATH"
    fi

    # Run safe redirect fixer (wp_redirect -> wp_safe_redirect)
    if [ -f "$SAFE_REDIRECT_FIXER" ]; then
        php "$SAFE_REDIRECT_FIXER" "$PLUGIN_PATH"
    fi

    # Run phpcbf for remaining auto-fixable issues
    if [ -f "$PHPCBF_BIN" ]; then
        echo "Running auto-fix (phpcbf)..."

        # Build phpcbf command arguments as array for proper path escaping
        phpcbf_args=(--standard="$PHPCS_CONFIG")
        if [ -n "$TEXT_DOMAIN" ]; then
            phpcbf_args+=(--runtime-set text_domain "$TEXT_DOMAIN")
        fi
        phpcbf_args+=("${LINT_FILES[@]}")

        # phpcbf exit codes: 0=no changes, 1=changes made, 2=some errors unfixable
        set +e
        "$PHPCBF_BIN" "${phpcbf_args[@]}"
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

# Validation
echo "Validating with PHPCS..."

# Build phpcs command arguments as array for proper path escaping
phpcs_args=(--standard="$PHPCS_CONFIG")
if [ -n "$TEXT_DOMAIN" ]; then
    phpcs_args+=(--runtime-set text_domain "$TEXT_DOMAIN")
fi
if [[ "${HOMEBOY_SUMMARY_MODE:-}" == "1" ]]; then
    phpcs_args+=(--report=summary)
fi
if [[ "${HOMEBOY_ERRORS_ONLY:-}" == "1" ]]; then
    phpcs_args+=(--warning-severity=0)
fi
phpcs_args+=("${LINT_FILES[@]}")

if "$PHPCS_BIN" "${phpcs_args[@]}"; then
    echo "PHPCS linting passed"
    exit 0
else
    echo "PHPCS linting failed"
    exit 1
fi
