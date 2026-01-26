#!/usr/bin/env bash
set -euo pipefail

# Bash 4.0+ required for associative arrays
if ((BASH_VERSINFO[0] < 4)); then
    echo "Error: This script requires bash 4.0+ (found ${BASH_VERSION})" >&2
    case "$(uname -s)" in
        Darwin)
            echo "macOS ships with bash 3.2. Install newer bash: brew install bash" >&2
            ;;
        Linux)
            echo "Update bash via your package manager (apt, dnf, pacman, etc.)" >&2
            ;;
        MINGW*|MSYS*|CYGWIN*)
            echo "Update Git Bash or use WSL with a modern bash version" >&2
            ;;
        *)
            echo "Install bash 4.0 or later for your platform" >&2
            ;;
    esac
    exit 1
fi

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
    echo "HOMEBOY_SNIFFS=${HOMEBOY_SNIFFS:-NOT_SET}"
    echo "HOMEBOY_EXCLUDE_SNIFFS=${HOMEBOY_EXCLUDE_SNIFFS:-NOT_SET}"
    echo "HOMEBOY_CATEGORY=${HOMEBOY_CATEGORY:-NOT_SET}"
fi

# Category to sniff mappings
declare -A CATEGORY_SNIFFS
CATEGORY_SNIFFS["security"]="WordPress.Security.EscapeOutput,WordPress.Security.NonceVerification,WordPress.Security.ValidatedSanitizedInput,WordPress.DB.PreparedSQL,WordPress.DB.PreparedSQLPlaceholders"
CATEGORY_SNIFFS["i18n"]="WordPress.WP.I18n"
CATEGORY_SNIFFS["yoda"]="WordPress.PHP.YodaConditions"
CATEGORY_SNIFFS["whitespace"]="WordPress.WhiteSpace"

# Resolve category to sniffs
EFFECTIVE_SNIFFS="${HOMEBOY_SNIFFS:-}"
if [ -n "${HOMEBOY_CATEGORY:-}" ]; then
    if [ -n "${CATEGORY_SNIFFS[${HOMEBOY_CATEGORY}]:-}" ]; then
        EFFECTIVE_SNIFFS="${CATEGORY_SNIFFS[${HOMEBOY_CATEGORY}]}"
        echo "Filtering to category: ${HOMEBOY_CATEGORY}"
    else
        echo "Warning: Unknown category '${HOMEBOY_CATEGORY}'. Available: security, i18n, yoda, whitespace"
    fi
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

    # Use eval for brace expansion (works in both bash and zsh)
    # The glob comes from Rust as "{file1,file2,file3}" format
    MATCHED_FILES=()
    eval 'for f in '"${HOMEBOY_LINT_GLOB}"'; do [ -e "$f" ] && MATCHED_FILES+=("$f"); done'

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
YODA_FIXER="${MODULE_PATH}/scripts/lint/php-fixers/yoda-fixer.php"
IN_ARRAY_FIXER="${MODULE_PATH}/scripts/lint/php-fixers/in-array-strict-fixer.php"
SHORT_TERNARY_FIXER="${MODULE_PATH}/scripts/lint/php-fixers/short-ternary-fixer.php"
ESCAPE_I18N_FIXER="${MODULE_PATH}/scripts/lint/php-fixers/escape-i18n-fixer.php"
ECHO_TRANSLATE_FIXER="${MODULE_PATH}/scripts/lint/php-fixers/echo-translate-fixer.php"
SAFE_REDIRECT_FIXER="${MODULE_PATH}/scripts/lint/php-fixers/safe-redirect-fixer.php"
WP_DIE_TRANSLATE_FIXER="${MODULE_PATH}/scripts/lint/php-fixers/wp-die-translate-fixer.php"
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

# Auto-detect text domain from plugin header (required for i18n validation)
TEXT_DOMAIN=""
MAIN_PLUGIN_FILE=$(find "$PLUGIN_PATH" -maxdepth 1 -name "*.php" -exec grep -l "Plugin Name:" {} \; 2>/dev/null | head -1)
if [ -n "$MAIN_PLUGIN_FILE" ]; then
    # Check if Text Domain header exists before extracting
    if ! grep -q "Text Domain:" "$MAIN_PLUGIN_FILE" 2>/dev/null; then
        echo "" >&2
        echo "============================================" >&2
        echo "ERROR: Missing Text Domain header" >&2
        echo "============================================" >&2
        echo "File: $MAIN_PLUGIN_FILE" >&2
        echo "" >&2
        echo "Add this line to your plugin header:" >&2
        echo "  * Text Domain: your-plugin-slug" >&2
        echo "" >&2
        exit 1
    fi
    TEXT_DOMAIN=$(grep -m1 "Text Domain:" "$MAIN_PLUGIN_FILE" | sed 's/.*Text Domain:[[:space:]]*//' | tr -d ' \r')
    if [ -n "$TEXT_DOMAIN" ] && [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: Detected text domain: $TEXT_DOMAIN"
    fi
fi

# Auto-fix mode: run custom fixers, then phpcbf, then phpcs
if [[ "${HOMEBOY_AUTO_FIX:-}" == "1" ]]; then
    # Run custom fixers on each target file/directory
    for lint_target in "${LINT_FILES[@]}"; do
        # Run Yoda condition fixer (handles cases phpcbf can't fix)
        if [ -f "$YODA_FIXER" ]; then
            php "$YODA_FIXER" "$lint_target"
        fi

        # Run in_array strict fixer (add true as third param)
        if [ -f "$IN_ARRAY_FIXER" ]; then
            php "$IN_ARRAY_FIXER" "$lint_target"
        fi

        # Run short ternary fixer (expand ?: to ? : for simple vars)
        if [ -f "$SHORT_TERNARY_FIXER" ]; then
            php "$SHORT_TERNARY_FIXER" "$lint_target"
        fi

        # Run escape i18n fixer (_e -> esc_html_e)
        if [ -f "$ESCAPE_I18N_FIXER" ]; then
            php "$ESCAPE_I18N_FIXER" "$lint_target"
        fi

        # Run echo translate fixer (echo __() -> echo esc_html__())
        if [ -f "$ECHO_TRANSLATE_FIXER" ]; then
            php "$ECHO_TRANSLATE_FIXER" "$lint_target"
        fi

        # Run safe redirect fixer (wp_redirect -> wp_safe_redirect)
        if [ -f "$SAFE_REDIRECT_FIXER" ]; then
            php "$SAFE_REDIRECT_FIXER" "$lint_target"
        fi

        # Run wp_die translate fixer (wp_die(__()) -> wp_die(esc_html__()))
        if [ -f "$WP_DIE_TRANSLATE_FIXER" ]; then
            php "$WP_DIE_TRANSLATE_FIXER" "$lint_target"
        fi
    done

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
        phpcbf_output=$("$PHPCBF_BIN" "${phpcbf_args[@]}" 2>&1)
        PHPCBF_EXIT=$?
        set -e

        # Show phpcbf output
        echo "$phpcbf_output"

        # Extract fix count from phpcbf output (e.g., "146 ERRORS WERE FIXED")
        fixed_count=$(echo "$phpcbf_output" | grep -oE '[0-9]+ ERRORS? WERE FIXED' | grep -oE '[0-9]+' || echo "0")

        echo ""
        if [ "$fixed_count" != "0" ]; then
            echo "PHPCBF fixed $fixed_count errors"
        fi

        if [ "$PHPCBF_EXIT" -eq 2 ]; then
            echo "WARNING: Some errors could not be auto-fixed."
        fi

        # Detect infinite loop (PHPCBF hit 50-pass limit)
        if echo "$phpcbf_output" | grep -q "made 50 passes"; then
            echo ""
            echo "ERROR: PHPCBF hit 50-pass limit (infinite loop detected)"
            echo "This usually means conflicting rules are fighting each other."
            echo "Check phpcs.xml.dist for rule conflicts."
        fi
        echo ""
    else
        echo "Warning: phpcbf not found, skipping auto-fix"
    fi
fi

# Validation
echo "Validating with PHPCS..."

# Build base phpcs arguments
phpcs_base_args=(--standard="$PHPCS_CONFIG")
if [ -n "$TEXT_DOMAIN" ]; then
    phpcs_base_args+=(--runtime-set text_domain "$TEXT_DOMAIN")
fi
if [[ "${HOMEBOY_ERRORS_ONLY:-}" == "1" ]]; then
    phpcs_base_args+=(--warning-severity=0)
fi
# Sniff filtering
if [ -n "$EFFECTIVE_SNIFFS" ]; then
    phpcs_base_args+=(--sniffs="$EFFECTIVE_SNIFFS")
fi
if [ -n "${HOMEBOY_EXCLUDE_SNIFFS:-}" ]; then
    phpcs_base_args+=(--exclude="${HOMEBOY_EXCLUDE_SNIFFS}")
fi

# First run: Get JSON report for summary header
set +e
json_output=$("$PHPCS_BIN" "${phpcs_base_args[@]}" --report=json "${LINT_FILES[@]}" 2>/dev/null)
json_exit=$?
set -e

# Parse JSON and print summary header (only if issues exist)
# NOTE: JSON is piped via stdin to avoid ARG_MAX limits (~1MB on macOS)
# Large codebases can generate multi-MB JSON output that exceeds shell limits
if [ -n "$json_output" ] && command -v php &> /dev/null; then
    summary=$(echo "$json_output" | php -r '
        $json = json_decode(file_get_contents("php://stdin"), true);
        if (!$json || !isset($json["totals"])) exit;
        $totals = $json["totals"];
        $errors = $totals["errors"] ?? 0;
        $warnings = $totals["warnings"] ?? 0;
        $fixable = $totals["fixable"] ?? 0;
        $files = count($json["files"] ?? []);
        $filesWithIssues = 0;
        foreach ($json["files"] ?? [] as $file) {
            if (($file["errors"] ?? 0) > 0 || ($file["warnings"] ?? 0) > 0) {
                $filesWithIssues++;
            }
        }
        if ($errors > 0 || $warnings > 0) {
            echo "============================================\n";
            echo "LINT SUMMARY: " . $errors . " errors, " . $warnings . " warnings\n";
            echo "Fixable: " . $fixable . " | Files with issues: " . $filesWithIssues . " of " . $files . "\n";
            echo "============================================\n";
        }
    ' 2>/dev/null)

    if [ -n "$summary" ]; then
        echo ""
        echo "$summary"
    fi
fi

# Summary mode: show summary header + top violations, skip full report
if [[ "${HOMEBOY_SUMMARY_MODE:-}" == "1" ]]; then
    if [ -n "$json_output" ] && command -v php &> /dev/null; then
        top_violations=$(echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json || !isset($json["totals"])) exit(1);

            // Count violations by source
            $sources = [];
            foreach ($json["files"] ?? [] as $file) {
                foreach ($file["messages"] ?? [] as $msg) {
                    $source = $msg["source"] ?? "Unknown";
                    if (!isset($sources[$source])) {
                        $sources[$source] = 0;
                    }
                    $sources[$source]++;
                }
            }

            if (empty($sources)) exit(0);

            // Sort by count descending
            arsort($sources);

            // Print top 10 violations
            echo "\nTOP VIOLATIONS:\n";
            $count = 0;
            foreach ($sources as $source => $num) {
                printf("  %-55s %5d\n", $source, $num);
                $count++;
                if ($count >= 10) break;
            }
        ' 2>/dev/null)

        if [ -n "$top_violations" ]; then
            echo "$top_violations"
        fi
    fi

    PHPCS_PASSED=0
    if [ "$json_exit" -eq 0 ]; then
        echo ""
        echo "PHPCS linting passed"
        PHPCS_PASSED=1
    else
        echo ""
        echo "PHPCS linting failed"
    fi

    # Run ESLint in summary mode
    ESLINT_RUNNER="${MODULE_PATH}/scripts/eslint-runner.sh"
    ESLINT_PASSED=1

    if [ -f "$ESLINT_RUNNER" ]; then
        echo ""
        set +e
        bash "$ESLINT_RUNNER"
        ESLINT_EXIT=$?
        set -e

        if [ "$ESLINT_EXIT" -ne 0 ]; then
            ESLINT_PASSED=0
        fi
    fi

    # Always exit 0 (warn-only mode) - lint issues are warnings, not failures
    if [ "$PHPCS_PASSED" -eq 1 ] && [ "$ESLINT_PASSED" -eq 1 ]; then
        echo "Linting passed"
    else
        echo "Linting found issues (see above)"
    fi
    exit 0
fi

# Full report mode (default)
PHPCS_PASSED=0
if "$PHPCS_BIN" "${phpcs_base_args[@]}" "${LINT_FILES[@]}"; then
    echo "PHPCS linting passed"
    PHPCS_PASSED=1
else
    echo "PHPCS linting failed"
fi

# Run ESLint for JavaScript files
ESLINT_RUNNER="${MODULE_PATH}/scripts/eslint-runner.sh"
ESLINT_PASSED=1

if [ -f "$ESLINT_RUNNER" ]; then
    echo ""
    set +e
    bash "$ESLINT_RUNNER"
    ESLINT_EXIT=$?
    set -e

    if [ "$ESLINT_EXIT" -ne 0 ]; then
        ESLINT_PASSED=0
    fi
fi

# Always exit 0 (warn-only mode) - lint issues are warnings, not failures
if [ "$PHPCS_PASSED" -eq 1 ] && [ "$ESLINT_PASSED" -eq 1 ]; then
    echo ""
    echo "Linting passed"
else
    echo ""
    echo "Linting found issues (see above)"
fi
exit 0
