#!/usr/bin/env bash
set -euo pipefail

# Standalone JavaScript linting script using ESLint
# Supports auto-fix mode via HOMEBOY_AUTO_FIX=1
# Supports summary mode via HOMEBOY_SUMMARY_MODE=1

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: ESLint Environment variables:"
    echo "HOMEBOY_MODULE_PATH=${HOMEBOY_MODULE_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_AUTO_FIX=${HOMEBOY_AUTO_FIX:-NOT_SET}"
    echo "HOMEBOY_SUMMARY_MODE=${HOMEBOY_SUMMARY_MODE:-NOT_SET}"
    echo "HOMEBOY_ERRORS_ONLY=${HOMEBOY_ERRORS_ONLY:-NOT_SET}"
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

# Check if component has JavaScript files
js_file_count=$(find "$PLUGIN_PATH" -type f \( -name "*.js" -o -name "*.jsx" -o -name "*.ts" -o -name "*.tsx" \) \
    -not -path "*/node_modules/*" \
    -not -path "*/vendor/*" \
    -not -path "*/build/*" \
    -not -path "*/dist/*" \
    -not -name "*.min.js" \
    2>/dev/null | wc -l | tr -d ' ')

if [ "$js_file_count" -eq 0 ]; then
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: No JavaScript files found, skipping ESLint"
    fi
    exit 0
fi

# Determine lint target (file, glob, or full component)
LINT_FILES=("$PLUGIN_PATH")

if [ -n "${HOMEBOY_LINT_FILE:-}" ]; then
    LINT_FILES=("${PLUGIN_PATH}/${HOMEBOY_LINT_FILE}")
    if [ ! -f "${LINT_FILES[0]}" ]; then
        echo "Error: File not found: ${LINT_FILES[0]}"
        exit 1
    fi

    # Skip non-JS files
    case "${HOMEBOY_LINT_FILE}" in
        *.js|*.jsx|*.ts|*.tsx)
            echo "Linting single file: ${HOMEBOY_LINT_FILE}"
            ;;
        *)
            if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
                echo "DEBUG: Skipping non-JS file: ${HOMEBOY_LINT_FILE}"
            fi
            exit 0
            ;;
    esac
elif [ -n "${HOMEBOY_LINT_GLOB:-}" ]; then
    cd "$PLUGIN_PATH"

    MATCHED_FILES=()
    eval 'for f in '"${HOMEBOY_LINT_GLOB}"'; do [ -e "$f" ] && MATCHED_FILES+=("$f"); done'

    if [ ${#MATCHED_FILES[@]} -eq 0 ]; then
        echo "No JS files match pattern: ${HOMEBOY_LINT_GLOB}"
        exit 0
    fi

    echo "Linting ${#MATCHED_FILES[@]} files matching: ${HOMEBOY_LINT_GLOB}"
    LINT_FILES=("${MATCHED_FILES[@]}")
    cd - > /dev/null
else
    echo "Running JavaScript linting..."
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "Module path: $MODULE_PATH"
    echo "Plugin path: $PLUGIN_PATH"
    echo "Lint files: ${LINT_FILES[*]}"
    echo "Auto-fix: ${HOMEBOY_AUTO_FIX:-0}"
fi

ESLINT_BIN="${MODULE_PATH}/node_modules/.bin/eslint"
ESLINT_CONFIG="${MODULE_PATH}/.eslintrc.json"

# Validate tools exist
if [ ! -f "$ESLINT_BIN" ]; then
    echo "Warning: ESLint not found at $ESLINT_BIN, skipping JavaScript linting"
    exit 0
fi

if [ ! -f "$ESLINT_CONFIG" ]; then
    echo "Warning: .eslintrc.json not found at $ESLINT_CONFIG, skipping JavaScript linting"
    exit 0
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

# Build base ESLint arguments
eslint_base_args=(--config "$ESLINT_CONFIG" --ext .js,.jsx,.ts,.tsx)

if [ -n "$TEXT_DOMAIN" ]; then
    eslint_base_args+=(--rule "@wordpress/i18n-text-domain: [error, { allowedTextDomain: \"$TEXT_DOMAIN\" }]")
fi

if [[ "${HOMEBOY_ERRORS_ONLY:-}" == "1" ]]; then
    eslint_base_args+=(--quiet)
fi

# Run from plugin directory to ensure jsconfig.json is found by import resolver
cd "$PLUGIN_PATH"

# Auto-fix mode
if [[ "${HOMEBOY_AUTO_FIX:-}" == "1" ]]; then
    echo "Running ESLint auto-fix..."
    set +e
    "$ESLINT_BIN" "${eslint_base_args[@]}" --fix "${LINT_FILES[@]}"
    FIX_EXIT=$?
    set -e

    if [ "$FIX_EXIT" -ne 0 ]; then
        echo ""
        echo "WARNING: Some ESLint errors could not be auto-fixed."
    fi
    echo ""
fi

# Get JSON report for summary
set +e
json_output=$("$ESLINT_BIN" "${eslint_base_args[@]}" --format json "${LINT_FILES[@]}" 2>/dev/null)
json_exit=$?
set -e

# Parse JSON and print summary header (only if issues exist)
if [ -n "$json_output" ] && command -v node &> /dev/null; then
    summary=$(node -e '
        const data = JSON.parse(process.argv[1]);
        let errors = 0, warnings = 0, fixable = 0, filesWithIssues = 0;
        data.forEach(file => {
            errors += file.errorCount || 0;
            warnings += file.warningCount || 0;
            fixable += file.fixableErrorCount + file.fixableWarningCount || 0;
            if (file.errorCount > 0 || file.warningCount > 0) filesWithIssues++;
        });
        if (errors > 0 || warnings > 0) {
            console.log("============================================");
            console.log(`ESLINT SUMMARY: ${errors} errors, ${warnings} warnings`);
            console.log(`Fixable: ${fixable} | Files with issues: ${filesWithIssues} of ${data.length}`);
            console.log("============================================");
        }
    ' "$json_output" 2>/dev/null)

    if [ -n "$summary" ]; then
        echo ""
        echo "$summary"
    fi
fi

# Summary mode: show summary header + top violations, skip full report
if [[ "${HOMEBOY_SUMMARY_MODE:-}" == "1" ]]; then
    if [ -n "$json_output" ] && command -v node &> /dev/null; then
        top_violations=$(node -e '
            const data = JSON.parse(process.argv[1]);
            const rules = {};
            data.forEach(file => {
                (file.messages || []).forEach(msg => {
                    const rule = msg.ruleId || "Unknown";
                    rules[rule] = (rules[rule] || 0) + 1;
                });
            });
            const sorted = Object.entries(rules).sort((a, b) => b[1] - a[1]);
            if (sorted.length > 0) {
                console.log("\nTOP VIOLATIONS:");
                sorted.slice(0, 10).forEach(([rule, count]) => {
                    console.log(`  ${rule.padEnd(55)} ${count.toString().padStart(5)}`);
                });
            }
        ' "$json_output" 2>/dev/null)

        if [ -n "$top_violations" ]; then
            echo "$top_violations"
        fi
    fi

    # Exit with appropriate code
    if [ "$json_exit" -eq 0 ]; then
        echo ""
        echo "ESLint linting passed"
        exit 0
    else
        echo ""
        echo "ESLint linting failed"
        exit 1
    fi
fi

# Full report mode (default)
if "$ESLINT_BIN" "${eslint_base_args[@]}" "${LINT_FILES[@]}"; then
    echo "ESLint linting passed"
    exit 0
else
    echo "ESLint linting failed"
    exit 1
fi
