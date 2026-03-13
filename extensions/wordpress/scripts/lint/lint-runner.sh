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
# Supports step filtering via HOMEBOY_STEP/HOMEBOY_SKIP (steps: phpcs, eslint, phpstan)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_STEPS_HELPER="${HOMEBOY_RUNTIME_RUNNER_STEPS:-${SCRIPT_DIR}/../lib/runner-steps.sh}"
# shellcheck source=../lib/runner-steps.sh
source "${RUNNER_STEPS_HELPER}"

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Environment variables:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
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
if [ -n "${HOMEBOY_EXTENSION_PATH:-}" ]; then
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH}"
    COMPONENT_PATH="${HOMEBOY_COMPONENT_PATH:-.}"
    PLUGIN_PATH="$COMPONENT_PATH"
else
    EXTENSION_PATH="$(dirname "$SCRIPT_DIR")"
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
    echo "Extension path: $EXTENSION_PATH"
    echo "Plugin path: $PLUGIN_PATH"
    echo "Lint files: ${LINT_FILES[*]}"
    echo "Auto-fix: ${HOMEBOY_AUTO_FIX:-0}"
fi

PHPCS_BIN="${EXTENSION_PATH}/vendor/bin/phpcs"
PHPCBF_BIN="${EXTENSION_PATH}/vendor/bin/phpcbf"
YODA_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/yoda-fixer.php"
IN_ARRAY_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/in-array-strict-fixer.php"
SHORT_TERNARY_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/short-ternary-fixer.php"
ESCAPE_I18N_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/escape-i18n-fixer.php"
ECHO_TRANSLATE_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/echo-translate-fixer.php"
SAFE_REDIRECT_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/safe-redirect-fixer.php"
WP_DIE_TRANSLATE_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/wp-die-translate-fixer.php"
STRICT_COMPARISON_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/strict-comparison-fixer.php"
LONELY_IF_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/lonely-if-fixer.php"
LOOP_COUNT_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/loop-count-fixer.php"
RESERVED_PARAM_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/reserved-param-fixer.php"
UNUSED_PARAM_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/unused-param-fixer.php"
SILENCED_ERROR_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/silenced-error-fixer.php"
EMPTY_CATCH_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/empty-catch-fixer.php"
READDIR_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/readdir-fixer.php"
COMMENTED_CODE_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/commented-code-fixer.php"
WP_ALTERNATIVES_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/wp-alternatives-fixer.php"
WP_FILESYSTEM_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/wp-filesystem-fixer.php"
TEXT_DOMAIN_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/text-domain-fixer.php"
PHPCS_IGNORE_FIXER="${EXTENSION_PATH}/scripts/lint/php-fixers/phpcs-ignore-fixer.php"
PHPCS_CONFIG="${EXTENSION_PATH}/phpcs.xml.dist"

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

# Auto-detect PHP version from composer.json (overrides phpcs.xml.dist default)
# Priority: HOMEBOY_PHP_VERSION env var > composer.json require.php > phpcs.xml.dist default
PHP_VERSION=""
if [ -n "${HOMEBOY_PHP_VERSION:-}" ]; then
    PHP_VERSION="${HOMEBOY_PHP_VERSION}"
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: PHP version from env: $PHP_VERSION"
    fi
elif [ -f "${PLUGIN_PATH}/composer.json" ] && command -v php &> /dev/null; then
    PHP_VERSION=$(php -r '
        $json = json_decode(file_get_contents($argv[1]), true);
        $constraint = $json["require"]["php"] ?? "";
        if ($constraint === "") exit;
        // Extract minimum version from constraint: ">=8.2" -> "8.2", "^8.1" -> "8.1", "~8.0" -> "8.0", "8.2.*" -> "8.2"
        if (preg_match("/(\d+\.\d+)/", $constraint, $m)) {
            echo $m[1];
        }
    ' "${PLUGIN_PATH}/composer.json" 2>/dev/null || echo "")
    if [ -n "$PHP_VERSION" ] && [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: PHP version from composer.json: $PHP_VERSION"
    fi
fi

if [ -n "$PHP_VERSION" ]; then
    echo "PHP compatibility target: ${PHP_VERSION}-"
fi

# Auto-fix mode: run custom fixers, then phpcbf, then phpcs
if [[ "${HOMEBOY_AUTO_FIX:-}" == "1" ]]; then
    # --- Fix results sidecar ---
    # Track what each fixer does so homeboy can report structured fix output.
    # Each fixer prints "NAME fixer: Fixed N thing(s) in N file(s)" on success.
    # We capture that and build a JSON array for HOMEBOY_FIX_RESULTS_FILE.
    FIX_RESULTS_JSON="[]"

    # Run a fixer and capture its results for the sidecar.
    # Usage: run_fixer <rule_name> <fixer_binary> [args...]
    run_fixer() {
        local rule="$1"; shift
        local fixer_bin="$1"; shift

        if [ ! -f "$fixer_bin" ]; then
            return 0
        fi

        local fixer_output
        set +e
        fixer_output=$(php "$fixer_bin" "$@" 2>&1)
        local fixer_exit=$?
        set -e
        echo "$fixer_output"

        # Parse "Fixed N thing(s) in N file(s)" from output
        local fix_count
        fix_count=$(echo "$fixer_output" | grep -oE 'Fixed [0-9]+' | head -1 | grep -oE '[0-9]+' || echo "0")
        local file_count
        file_count=$(echo "$fixer_output" | grep -oE 'in [0-9]+ file' | head -1 | grep -oE '[0-9]+' || echo "0")

        if [ "$fix_count" != "0" ] && [ "$fix_count" -gt 0 ] 2>/dev/null; then
            # Append one entry per fix (rule-level granularity, not per-file)
            FIX_RESULTS_JSON=$(python3 -c "
import json, sys
results = json.loads(sys.argv[1])
results.append({'file': '(multiple)' if int(sys.argv[3]) > 1 else '(single)', 'rule': sys.argv[2], 'action': 'rewrite'})
print(json.dumps(results))
" "$FIX_RESULTS_JSON" "$rule" "$file_count" 2>/dev/null || echo "$FIX_RESULTS_JSON")
        fi

        return $fixer_exit
    }

    # Run custom fixers on each target file/directory
    for lint_target in "${LINT_FILES[@]}"; do
        run_fixer "yoda-condition" "$YODA_FIXER" "$lint_target"
        run_fixer "in-array-strict" "$IN_ARRAY_FIXER" "$lint_target"
        run_fixer "short-ternary" "$SHORT_TERNARY_FIXER" "$lint_target"
        run_fixer "escape-i18n" "$ESCAPE_I18N_FIXER" "$lint_target"
        run_fixer "echo-translate" "$ECHO_TRANSLATE_FIXER" "$lint_target"
        run_fixer "safe-redirect" "$SAFE_REDIRECT_FIXER" "$lint_target"
        run_fixer "wp-die-translate" "$WP_DIE_TRANSLATE_FIXER" "$lint_target"
        run_fixer "strict-comparison" "$STRICT_COMPARISON_FIXER" "$lint_target"
        run_fixer "lonely-if" "$LONELY_IF_FIXER" "$lint_target"
        run_fixer "loop-count" "$LOOP_COUNT_FIXER" "$lint_target"

        # Reserved param fixer runs OUTSIDE this loop (needs cross-file manifest)

        # Unused parameter fixer needs extra args
        run_fixer "unused-param" "$UNUSED_PARAM_FIXER" "$lint_target" --phpcs-binary="$PHPCS_BIN" --phpcs-standard="$PHPCS_CONFIG"

        run_fixer "silenced-error" "$SILENCED_ERROR_FIXER" "$lint_target"
        run_fixer "empty-catch" "$EMPTY_CATCH_FIXER" "$lint_target"
        run_fixer "readdir" "$READDIR_FIXER" "$lint_target"
        run_fixer "commented-code" "$COMMENTED_CODE_FIXER" "$lint_target"
        run_fixer "wp-alternatives" "$WP_ALTERNATIVES_FIXER" "$lint_target"
        run_fixer "wp-filesystem" "$WP_FILESYSTEM_FIXER" "$lint_target"

        # Text domain fixer: replace wrong text domains in i18n function calls.
        # Needs --text-domain arg if detected from plugin header.
        if [ -n "$TEXT_DOMAIN" ]; then
            run_fixer "text-domain" "$TEXT_DOMAIN_FIXER" "$lint_target" --text-domain="$TEXT_DOMAIN"
        fi
    done

    # Run reserved keyword parameter name fixer ($default -> $default_value, etc.)
    # MUST run on full plugin path (not per-file) because its two-pass architecture
    # builds a rename manifest in Pass 1 (declarations) and applies it to call sites
    # in Pass 2 (named arguments). Per-file invocation loses the manifest between
    # processes, leaving call sites with stale parameter names.
    run_fixer "reserved-param" "$RESERVED_PARAM_FIXER" "$PLUGIN_PATH"

    # Run phpcbf for remaining auto-fixable issues
    if [ -f "$PHPCBF_BIN" ]; then
        echo "Running auto-fix (phpcbf)..."

        # Build phpcbf command arguments as array for proper path escaping
        phpcbf_args=(--standard="$PHPCS_CONFIG")
        if [ -n "$TEXT_DOMAIN" ]; then
            phpcbf_args+=(--runtime-set text_domain "$TEXT_DOMAIN")
        fi
        if [ -n "$PHP_VERSION" ]; then
            phpcbf_args+=(--runtime-set testVersion "${PHP_VERSION}-")
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
            # Record phpcbf fixes in sidecar
            FIX_RESULTS_JSON=$(python3 -c "
import json, sys
results = json.loads(sys.argv[1])
results.append({'file': '(multiple)', 'rule': 'phpcbf', 'action': 'format'})
print(json.dumps(results))
" "$FIX_RESULTS_JSON" 2>/dev/null || echo "$FIX_RESULTS_JSON")
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

    # Run phpcs:ignore fixer LAST — adds ignore comments for known false positives
    # (PreparedSQL table names, base64_encode for auth, mt_srand, ValidHookName)
    # This must run after all real-code fixers and phpcbf
    for lint_target in "${LINT_FILES[@]}"; do
        run_fixer "phpcs-ignore" "$PHPCS_IGNORE_FIXER" "$lint_target" --phpcs-binary="$PHPCS_BIN" --phpcs-standard="$PHPCS_CONFIG"
    done

    # Write fix plan sidecar for planning flows (same shape as fix results)
    if [ -n "${HOMEBOY_FIX_PLAN_FILE:-}" ]; then
        echo "$FIX_RESULTS_JSON" > "${HOMEBOY_FIX_PLAN_FILE}"
    fi

    # Write fix results sidecar for homeboy to consume
    if [ -n "${HOMEBOY_FIX_RESULTS_FILE:-}" ]; then
        echo "$FIX_RESULTS_JSON" > "${HOMEBOY_FIX_RESULTS_FILE}"
    fi

    # Post-fix syntax validation — catch any fixer that produced broken PHP
    # This is a safety net: if any fixer introduces a syntax error, we catch it
    # here before PHPCS validation (which would report confusing errors)
    echo "Verifying PHP syntax after auto-fix..."
    syntax_errors=0
    syntax_error_files=()
    for lint_target in "${LINT_FILES[@]}"; do
        if [ -d "$lint_target" ]; then
            # Walk directory for PHP files
            while IFS= read -r -d '' php_file; do
                if ! php -l "$php_file" > /dev/null 2>&1; then
                    syntax_errors=$((syntax_errors + 1))
                    syntax_error_files+=("$php_file")
                fi
            done < <(find "$lint_target" -name '*.php' -not -path '*/vendor/*' -not -path '*/node_modules/*' -print0)
        elif [ -f "$lint_target" ]; then
            if ! php -l "$lint_target" > /dev/null 2>&1; then
                syntax_errors=$((syntax_errors + 1))
                syntax_error_files+=("$lint_target")
            fi
        fi
    done

    if [ "$syntax_errors" -gt 0 ]; then
        echo ""
        echo "============================================"
        echo "CRITICAL: Auto-fix introduced $syntax_errors PHP syntax error(s)!"
        echo "============================================"
        echo ""
        echo "The following files have syntax errors after auto-fix:"
        for errfile in "${syntax_error_files[@]}"; do
            echo "  - $errfile"
            php -l "$errfile" 2>&1 | grep -v "^$" | sed 's/^/    /'
        done
        echo ""
        echo "This indicates a fixer bug. Do NOT commit these changes."
        echo "Report this to the homeboy-extensions maintainer."
        echo ""
        exit 1
    fi
    echo "Syntax OK — all PHP files pass php -l"
fi

# Validation
echo "Validating with PHPCS..."

# Build base phpcs arguments
phpcs_base_args=(--standard="$PHPCS_CONFIG")
if [ -n "$TEXT_DOMAIN" ]; then
    phpcs_base_args+=(--runtime-set text_domain "$TEXT_DOMAIN")
fi
if [ -n "$PHP_VERSION" ]; then
    phpcs_base_args+=(--runtime-set testVersion "${PHP_VERSION}-")
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
if ! should_run_step "phpcs"; then
    json_output=""
    json_exit=0
    echo "Skipping PHPCS (step filter)"
else
    set +e
    json_output=$("$PHPCS_BIN" "${phpcs_base_args[@]}" --report=json "${LINT_FILES[@]}" 2>/dev/null)
    json_exit=$?
    set -e
fi

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

    # Write annotations sidecar JSON for CI inline comments
    if [ -n "${HOMEBOY_ANNOTATIONS_DIR:-}" ] && [ -d "${HOMEBOY_ANNOTATIONS_DIR}" ]; then
        echo "$json_output" | php -r '
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json || empty($json["files"])) exit;
            $componentPath = $argv[1] ?? "";
            $annotations = [];
            foreach ($json["files"] as $filePath => $data) {
                $displayPath = $filePath;
                if ($componentPath && strpos($filePath, $componentPath) === 0) {
                    $displayPath = ltrim(substr($filePath, strlen($componentPath)), "/");
                }
                foreach ($data["messages"] ?? [] as $msg) {
                    $annotations[] = [
                        "file" => $displayPath,
                        "line" => $msg["line"] ?? 0,
                        "message" => $msg["message"] ?? "Unknown",
                        "source" => "phpcs",
                        "severity" => ($msg["type"] ?? "ERROR") === "ERROR" ? "error" : "warning",
                        "code" => $msg["source"] ?? "unknown",
                        "fixable" => $msg["fixable"] ?? false,
                    ];
                }
            }
            $outDir = $argv[2] ?? "";
            if ($outDir && !empty($annotations)) {
                file_put_contents($outDir . "/phpcs.json", json_encode($annotations, JSON_PRETTY_PRINT) . "\n");
            }
        ' "$PLUGIN_PATH" "${HOMEBOY_ANNOTATIONS_DIR}" 2>/dev/null || true
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
    ESLINT_RUNNER="${EXTENSION_PATH}/scripts/lint/eslint-runner.sh"
    ESLINT_PASSED=1

    if ! should_run_step "eslint"; then
        echo ""
        echo "Skipping ESLint (step filter)"
    elif [ -f "$ESLINT_RUNNER" ]; then
        echo ""
        set +e
        bash "$ESLINT_RUNNER"
        ESLINT_EXIT=$?
        set -e

        if [ "$ESLINT_EXIT" -ne 0 ]; then
            ESLINT_PASSED=0
        fi
    fi

    # Run PHPStan in summary mode
    run_phpstan_summary() {
        local phpstan_runner="${EXTENSION_PATH}/scripts/lint/phpstan-runner.sh"
        if [ ! -f "$phpstan_runner" ]; then
            return 0
        fi

        if ! should_run_step "phpstan"; then
            echo ""
            echo "Skipping PHPStan (step filter)"
            return 0
        fi

        if [[ "${HOMEBOY_SKIP_PHPSTAN:-}" == "1" ]]; then
            echo "Skipping PHPStan (HOMEBOY_SKIP_PHPSTAN=1)"
            return 0
        fi

        echo ""
        set +e
        HOMEBOY_SUMMARY_MODE=1 bash "$phpstan_runner"
        local phpstan_exit=$?
        set -e

        if [ "$phpstan_exit" -ne 0 ]; then
            return 1
        fi
        return 0
    }

    # Run PHPStan (warn-only - does not affect exit code)
    PHPSTAN_PASSED=1
    run_phpstan_summary || PHPSTAN_PASSED=0

    # Always exit 0 (warn-only mode) - lint issues are warnings, not failures
    if [ "$PHPCS_PASSED" -eq 1 ] && [ "$ESLINT_PASSED" -eq 1 ] && [ "$PHPSTAN_PASSED" -eq 1 ]; then
        echo "Linting passed"
    else
        echo "Linting found issues (see above)"
    fi
    exit 0
fi

# Full report mode (default)
PHPCS_PASSED=0
if ! should_run_step "phpcs"; then
    echo "Skipping PHPCS (step filter)"
    PHPCS_PASSED=1
elif "$PHPCS_BIN" "${phpcs_base_args[@]}" "${LINT_FILES[@]}"; then
    echo "PHPCS linting passed"
    PHPCS_PASSED=1
else
    echo "PHPCS linting failed"
fi

# Run ESLint for JavaScript files
ESLINT_RUNNER="${EXTENSION_PATH}/scripts/lint/eslint-runner.sh"
ESLINT_PASSED=1

if ! should_run_step "eslint"; then
    echo ""
    echo "Skipping ESLint (step filter)"
elif [ -f "$ESLINT_RUNNER" ]; then
    echo ""
    set +e
    bash "$ESLINT_RUNNER"
    ESLINT_EXIT=$?
    set -e

    if [ "$ESLINT_EXIT" -ne 0 ]; then
        ESLINT_PASSED=0
    fi
fi

# Run PHPStan in warn-only mode (optional static analysis)
run_phpstan() {
    local phpstan_runner="${EXTENSION_PATH}/scripts/lint/phpstan-runner.sh"
    if [ ! -f "$phpstan_runner" ]; then
        return 0
    fi

    if ! should_run_step "phpstan"; then
        echo ""
        echo "Skipping PHPStan (step filter)"
        return 0
    fi

    if [[ "${HOMEBOY_SKIP_PHPSTAN:-}" == "1" ]]; then
        echo "Skipping PHPStan (HOMEBOY_SKIP_PHPSTAN=1)"
        return 0
    fi

    echo ""
    set +e
    HOMEBOY_SUMMARY_MODE=1 bash "$phpstan_runner"
    local phpstan_exit=$?
    set -e

    if [ "$phpstan_exit" -ne 0 ]; then
        # Return 1 to indicate issues found, but caller handles exit code
        return 1
    fi
    return 0
}

# Run PHPStan (warn-only - does not affect exit code)
PHPSTAN_PASSED=1
run_phpstan || PHPSTAN_PASSED=0

# Always exit 0 (warn-only mode) - lint issues are warnings, not failures
if [ "$PHPCS_PASSED" -eq 1 ] && [ "$ESLINT_PASSED" -eq 1 ] && [ "$PHPSTAN_PASSED" -eq 1 ]; then
    echo ""
    echo "Linting passed"
else
    echo ""
    echo "Linting found issues (see above)"
fi
exit 0
