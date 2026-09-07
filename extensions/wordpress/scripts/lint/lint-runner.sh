#!/usr/bin/env bash
set -euo pipefail

# Standalone PHP linting script using PHPCS/PHPCBF
# Supports fix-only mode via HOMEBOY_FIX_ONLY=1 (sent by `homeboy refactor`)
# Supports summary mode via HOMEBOY_SUMMARY_MODE=1
# Supports step filtering via HOMEBOY_STEP/HOMEBOY_SKIP (steps: phpcs, eslint, phpstan)
#
# HOMEBOY_FIX_ONLY=1 is the single auto-fix contract: the runner executes
# fixers (phpcbf + custom) and skips its own validation pass. All auto-fix
# flows go through `homeboy refactor --from lint --write` (#1145).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STABLE_FINGERPRINT_HELPER="${SCRIPT_DIR}/stable-fingerprint.php"
SHARED_LIB_DIR="${HOMEBOY_SHARED_LIB_DIR:-}"
if [ -z "$SHARED_LIB_DIR" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
    SHARED_LIB_DIR="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
fi
SHARED_LIB_DIR="${SHARED_LIB_DIR:-$(cd "${SCRIPT_DIR}/../../../scripts/lib" && pwd)}"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/runner-harness.sh"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/lint-findings-adapter.sh"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/repository-file-discovery.sh"
homeboy_runner_harness_init --bash 4 --steps --sidecar-writer --component-alias PLUGIN_PATH
homeboy_lint_findings_init

FIX_RESULTS_HELPER="${HOMEBOY_RUNTIME_FIX_RESULTS:-${SHARED_LIB_DIR}/fix-results.sh}"
# shellcheck source=../../../scripts/lib/fix-results.sh
source "$FIX_RESULTS_HELPER"

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Environment variables:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_FIX_ONLY=${HOMEBOY_FIX_ONLY:-NOT_SET}"
    echo "HOMEBOY_SUMMARY_MODE=${HOMEBOY_SUMMARY_MODE:-NOT_SET}"
    echo "HOMEBOY_SNIFFS=${HOMEBOY_SNIFFS:-NOT_SET}"
    echo "HOMEBOY_EXCLUDE_SNIFFS=${HOMEBOY_EXCLUDE_SNIFFS:-NOT_SET}"
    echo "HOMEBOY_CATEGORY=${HOMEBOY_CATEGORY:-NOT_SET}"
fi

wordpress_lint_role_for_path() {
    local rel_path="$1"

    case "$rel_path" in
        scoper.inc.php|scoper.php|*.scoper.inc.php)
            printf '%s\n' 'scoper_config'
            ;;
        tools/*|bin/*)
            printf '%s\n' 'tooling'
            ;;
        tests/*-smoke.php|tests/smoke-*.php|*/smoke-*.php|*/*-smoke.php)
            printf '%s\n' 'smoke_harness'
            ;;
        tests/*Test.php|tests/*TestCase.php|*/tests/*Test.php|*/tests/*TestCase.php)
            printf '%s\n' 'phpunit_test'
            ;;
        *)
            printf '%s\n' 'production'
            ;;
    esac
}

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

COMPONENT_SHAPE="${HOMEBOY_COMPONENT_SHAPE:-}"
if [ -z "$COMPONENT_SHAPE" ]; then
    DETECT_COMPONENT_HELPER="${HOMEBOY_RUNTIME_DETECT_COMPONENT:-${SCRIPT_DIR}/../lib/detect-component.sh}"
    # shellcheck source=../lib/detect-component.sh
    source "${DETECT_COMPONENT_HELPER}"
    if homeboy_detect_component "$PLUGIN_PATH"; then
        COMPONENT_SHAPE="$HOMEBOY_COMPONENT_TYPE"
    fi
fi

if [ "$COMPONENT_SHAPE" = "core-dev" ]; then
    exec bash "${SCRIPT_DIR}/lint-runner-core-dev.sh" "$@"
fi

homeboy_mktemp() {
    local template="$1"
    homeboy_runner_harness_mktemp "$template"
}

merge_findings_into_sidecar() {
    local extra_file="$1"
    [ ! -f "$extra_file" ] && return 0
    homeboy_lint_findings_merge_file "$extra_file"
}

write_lint_producers_sidecar() {
    local phpcs_passed="$1"
    local eslint_passed="$2"
    local phpstan_passed="$3"
    local python_bin=""

    [ -z "${HOMEBOY_LINT_PRODUCERS_FILE:-}" ] && return 0

    if command -v python3 >/dev/null 2>&1; then
        python_bin="python3"
    elif command -v python >/dev/null 2>&1; then
        python_bin="python"
    else
        echo "Error: python3 or python is required to write lint producer summaries" >&2
        return 1
    fi

    "$python_bin" - "$HOMEBOY_LINT_PRODUCERS_FILE" "${HOMEBOY_LINT_FINDINGS_FILE:-}" "$phpcs_passed" "$eslint_passed" "$phpstan_passed" "${HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE:-}" <<'PYEOF'
import json
import os
import sys
import tempfile

target, findings_path, phpcs_passed, eslint_passed, phpstan_passed, phpstan_metadata_path = sys.argv[1:7]
counts = {"phpcs": 0, "eslint": 0, "phpstan": 0}
if findings_path and os.path.exists(findings_path) and os.path.getsize(findings_path) > 0:
    with open(findings_path, "r", encoding="utf-8") as handle:
        findings = json.load(handle)
    for finding in findings if isinstance(findings, list) else []:
        tool = finding.get("tool") if isinstance(finding, dict) else None
        if tool in counts:
            counts[tool] += 1

statuses = {
    "phpcs": "passed" if phpcs_passed == "1" else "failed",
    "eslint": "passed" if eslint_passed == "1" else "failed",
    "phpstan": "passed" if phpstan_passed == "1" else "failed",
}
phpstan_metadata = {"source_sidecar": "lint-producers"}
if phpstan_metadata_path and os.path.exists(phpstan_metadata_path) and os.path.getsize(phpstan_metadata_path) > 0:
    with open(phpstan_metadata_path, "r", encoding="utf-8") as handle:
        loaded = json.load(handle)
    if isinstance(loaded, dict):
        phpstan_metadata.update(loaded)
producers = [
    {
        "tool": tool,
        "status": statuses[tool],
        "finding_count": counts[tool],
        "step": tool,
        "metadata": phpstan_metadata if tool == "phpstan" else {"source_sidecar": "lint-producers"},
    }
    for tool in ("phpcs", "eslint", "phpstan")
]

directory = os.path.dirname(target) or "."
os.makedirs(directory, exist_ok=True)
fd, tmp = tempfile.mkstemp(prefix=".homeboy-lint-producers-", suffix=".json", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        json.dump(producers, handle, separators=(",", ":"))
        handle.write("\n")
    os.replace(tmp, target)
except Exception:
    try:
        os.unlink(tmp)
    except OSError:
        pass
    raise
PYEOF
}

# Determine lint target (file, glob, or full component).
# Explicit targets may intentionally include generated or Git-ignored files.
# Use an array to properly handle paths with spaces.
LINT_FILES=()

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
    while IFS= read -r -d '' lint_file; do
        LINT_FILES+=("${PLUGIN_PATH}/${lint_file#./}")
    done < <(homeboy_discover_repository_files "$PLUGIN_PATH" '*.php')

    if [ "${#LINT_FILES[@]}" -eq 0 ]; then
        echo "No PHP files found, skipping PHPCS."
        exit 0
    fi
fi

WORDPRESS_LINT_ROLE="production"
if [ -n "${HOMEBOY_WORDPRESS_LINT_ROLE:-}" ]; then
    WORDPRESS_LINT_ROLE="$HOMEBOY_WORDPRESS_LINT_ROLE"
elif [ -n "${HOMEBOY_LINT_FILE:-}" ]; then
    WORDPRESS_LINT_ROLE=$(wordpress_lint_role_for_path "$HOMEBOY_LINT_FILE")
fi
export HOMEBOY_WORDPRESS_LINT_ROLE="$WORDPRESS_LINT_ROLE"

if [ "$WORDPRESS_LINT_ROLE" != "production" ]; then
    echo "WordPress lint role: ${WORDPRESS_LINT_ROLE}"
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "Extension path: $EXTENSION_PATH"
    echo "Plugin path: $PLUGIN_PATH"
    echo "Lint files: ${LINT_FILES[*]}"
    echo "Fix-only: ${HOMEBOY_FIX_ONLY:-0}"
fi

homeboy_lint_relpath() {
    local path="$1"
    path="${path#$PLUGIN_PATH/}"
    path="${path#./}"
    printf '%s\n' "$path"
}

homeboy_wordpress_runtime_lint_file() {
    local rel_path="$1"
    local lint_role

    case "$rel_path" in
        vendor/*|vendor_prefixed/*|vendor-prefixed/*|vendor_scoped/*|vendor-scoped/*|node_modules/*|dist/*|build/*)
            return 1
            ;;
    esac

    lint_role=$(wordpress_lint_role_for_path "$rel_path")
    case "$lint_role" in
        scoper_config|smoke_harness|phpunit_test)
            return 1
            ;;
    esac

    return 0
}

homeboy_php_syntax_check() {
    local syntax_errors=0
    local lint_target php_file rel_path

    for lint_target in "$@"; do
        if [ -d "$lint_target" ]; then
            while IFS= read -r -d '' php_file; do
                rel_path=$(homeboy_lint_relpath "$php_file")
                if ! homeboy_wordpress_runtime_lint_file "$rel_path"; then
                    if ! php -l "$php_file" > /dev/null 2>&1; then
                        php -l "$php_file" || true
                        syntax_errors=$((syntax_errors + 1))
                    fi
                fi
            done < <(find "$lint_target" -type f -name '*.php' -print0)
        elif [ -f "$lint_target" ] && [[ "$lint_target" == *.php ]]; then
            if ! php -l "$lint_target" > /dev/null 2>&1; then
                php -l "$lint_target" || true
                syntax_errors=$((syntax_errors + 1))
            fi
        fi
    done

    if [ "$syntax_errors" -gt 0 ]; then
        echo "PHP syntax check failed for ${syntax_errors} non-runtime file(s)"
        return 1
    fi

    return 0
}

# The WordPress lint profile targets production plugin/theme runtime files. Keep
# php-scoper config, build tooling, smoke harnesses, PHPUnit tests, and generated
# vendored code out of that profile; syntax checking is enough for those roles.
if [ -n "${HOMEBOY_LINT_FILE:-}" ] || [ -n "${HOMEBOY_LINT_GLOB:-}" ]; then
    RUNTIME_LINT_FILES=()
    NON_RUNTIME_LINT_FILES=()

    for lint_target in "${LINT_FILES[@]}"; do
        rel_target=$(homeboy_lint_relpath "$lint_target")
        if [ -f "$lint_target" ] && ! homeboy_wordpress_runtime_lint_file "$rel_target"; then
            NON_RUNTIME_LINT_FILES+=("$lint_target")
        else
            RUNTIME_LINT_FILES+=("$lint_target")
        fi
    done

    if [ "${#NON_RUNTIME_LINT_FILES[@]}" -gt 0 ]; then
        echo "Non-runtime WordPress lint profile: syntax-checking ${#NON_RUNTIME_LINT_FILES[@]} file(s)"
        homeboy_php_syntax_check "${NON_RUNTIME_LINT_FILES[@]}"
    fi

    if [ "${#RUNTIME_LINT_FILES[@]}" -eq 0 ]; then
        echo "Skipping production WordPress lint profile for non-runtime file scope"
        echo "Linting passed"
        exit 0
    fi

    LINT_FILES=("${RUNTIME_LINT_FILES[@]}")
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
PHPCS_CONFIG=""
COMPONENT_PHPCS_CONFIG=0
for phpcs_config_name in phpcs.xml .phpcs.xml phpcs.xml.dist .phpcs.xml.dist; do
    if [ -f "${PLUGIN_PATH}/${phpcs_config_name}" ]; then
        PHPCS_CONFIG="${PLUGIN_PATH}/${phpcs_config_name}"
        COMPONENT_PHPCS_CONFIG=1
        break
    fi
done
# Fall back to the shipped consumer ruleset (rulesets/homeboy-wordpress-project.xml),
# NOT this extension's own phpcs.xml.dist, when the component has no PHPCS config of
# its own. phpcs.xml.dist exists to dogfood the shipped ruleset for this extension's
# own repo and references it via a relative `<rule ref="rulesets/...">` — PHPCS
# resolves that relative path against the running process's CWD, not the referencing
# ruleset file's directory. That CWD is the extension root during this extension's own
# self-check lint (where phpcs.xml.dist correctly resolves), but is always the
# consumer component's own directory here, so pointing at phpcs.xml.dist made PHPCS
# fail to load *any* standard for every consumer with no ruleset of its own
# (homeboy-extensions#2797): "ERROR: Referenced sniff \"rulesets/homeboy-wordpress-project.xml\"
# does not exist." — which aborts before scanning a single file, not a scan that walks
# vendor/. The shipped ruleset has no relative file refs (only named-standard refs
# resolved via `installed_paths`, set up above), so it resolves identically regardless
# of CWD.
PHPCS_CONFIG="${PHPCS_CONFIG:-${EXTENSION_PATH}/rulesets/homeboy-wordpress-project.xml}"

if [ "$COMPONENT_PHPCS_CONFIG" -eq 1 ]; then
    if [ -x "${PLUGIN_PATH}/vendor/bin/phpcs" ]; then
        PHPCS_BIN="${PLUGIN_PATH}/vendor/bin/phpcs"
    fi
    if [ -x "${PLUGIN_PATH}/vendor/bin/phpcbf" ]; then
        PHPCBF_BIN="${PLUGIN_PATH}/vendor/bin/phpcbf"
    fi
fi

# Validate tools exist
if [ ! -f "$PHPCS_BIN" ]; then
    echo "Error: phpcs not found at $PHPCS_BIN"
    exit 1
fi

if [ ! -f "$PHPCS_CONFIG" ]; then
    echo "Error: PHPCS config not found at $PHPCS_CONFIG"
    exit 1
fi

# Composer's PHPCS installer can be bypassed in linked extension installs. Keep
# the extension binary self-healing so WordPress-Extra and HomeboyWordPress
# always resolve, but preserve component Composer standards when its binary is
# selected for a component-owned ruleset.
PHPCS_STANDARD_PATHS=()
if [ "$PHPCS_BIN" = "${EXTENSION_PATH}/vendor/bin/phpcs" ]; then
    for phpcs_standard_path in \
        "${EXTENSION_PATH}/vendor/wp-coding-standards/wpcs" \
        "${EXTENSION_PATH}/vendor/phpcsstandards/phpcsextra" \
        "${EXTENSION_PATH}/vendor/phpcsstandards/phpcsutils" \
        "${EXTENSION_PATH}/HomeboyWordPress"; do
        if [ -d "$phpcs_standard_path" ]; then
            PHPCS_STANDARD_PATHS+=("$phpcs_standard_path")
        fi
    done

    if [ "${#PHPCS_STANDARD_PATHS[@]}" -gt 0 ]; then
        PHPCS_INSTALLED_PATHS=$(IFS=','; printf '%s' "${PHPCS_STANDARD_PATHS[*]}")
        "$PHPCS_BIN" --config-set installed_paths "$PHPCS_INSTALLED_PATHS" --quiet > /dev/null 2>&1 || true
    fi
fi

# Auto-detect text domain from plugin/theme header (shared helper)
DETECT_COMPONENT_HELPER="${HOMEBOY_RUNTIME_DETECT_COMPONENT:-${SCRIPT_DIR}/../lib/detect-component.sh}"
# shellcheck source=../lib/detect-component.sh
source "${DETECT_COMPONENT_HELPER}"
homeboy_detect_component "$PLUGIN_PATH" || true

TEXT_DOMAIN="${HOMEBOY_COMPONENT_TEXT_DOMAIN:-}"

# Require text domain header for plugins (themes use stylesheet slug)
if [ "$HOMEBOY_COMPONENT_TYPE" = "plugin" ] && [ -z "$TEXT_DOMAIN" ] && [ -n "$HOMEBOY_COMPONENT_MAIN_FILE" ]; then
    echo "" >&2
    echo "============================================" >&2
    echo "ERROR: Missing Text Domain header" >&2
    echo "============================================" >&2
    echo "File: $HOMEBOY_COMPONENT_MAIN_FILE" >&2
    echo "" >&2
    echo "Add this line to your plugin header:" >&2
    echo "  * Text Domain: your-plugin-slug" >&2
    echo "" >&2
    exit 1
fi

if [ -n "$TEXT_DOMAIN" ] && [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Detected text domain: $TEXT_DOMAIN"
fi

# Auto-detect PHP version for PHPCS and PHPStan.
# Priority: HOMEBOY_PHP_VERSION env var > Requires PHP header > composer.json require.php > default
PHP_VERSION=""
if [ -n "${HOMEBOY_PHP_VERSION:-}" ]; then
    PHP_VERSION="${HOMEBOY_PHP_VERSION}"
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: PHP version from env: $PHP_VERSION"
    fi
elif [ -n "${HOMEBOY_COMPONENT_REQUIRES_PHP:-}" ]; then
    PHP_VERSION="${HOMEBOY_COMPONENT_REQUIRES_PHP}"
    if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
        echo "DEBUG: PHP version from Requires PHP header: $PHP_VERSION"
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

# Export for child scripts (phpstan-runner.sh reads this)
if [ -n "$PHP_VERSION" ]; then
    export HOMEBOY_PHP_VERSION="$PHP_VERSION"
    echo "PHP compatibility target: ${PHP_VERSION}-"
fi

# Fix-only mode: run custom fixers, then phpcbf, then exit before validation.
# Sent by `homeboy refactor --from lint --write` — the engine validates separately.
if [[ "${HOMEBOY_FIX_ONLY:-}" == "1" ]]; then
    # Fixer confidence tiers:
    #   safe       — Mechanical token rewrite, no semantic ambiguity. Always correct.
    #   guarded    — Safe with guardrails (cross-file lookup, contract detection, syntax validation).
    #   advisory   — May produce false positives or behavior changes. Review recommended.
    declare -A FIXER_CONFIDENCE
    FIXER_CONFIDENCE["yoda-condition"]="safe"
    FIXER_CONFIDENCE["in-array-strict"]="safe"
    FIXER_CONFIDENCE["escape-i18n"]="safe"
    FIXER_CONFIDENCE["echo-translate"]="safe"
    FIXER_CONFIDENCE["safe-redirect"]="safe"
    FIXER_CONFIDENCE["wp-die-translate"]="safe"
    FIXER_CONFIDENCE["lonely-if"]="safe"
    FIXER_CONFIDENCE["loop-count"]="safe"
    FIXER_CONFIDENCE["empty-catch"]="safe"
    FIXER_CONFIDENCE["wp-alternatives"]="safe"
    FIXER_CONFIDENCE["text-domain"]="safe"
    FIXER_CONFIDENCE["commented-code"]="safe"
    FIXER_CONFIDENCE["phpcs-ignore"]="safe"
    FIXER_CONFIDENCE["phpcbf"]="safe"
    FIXER_CONFIDENCE["reserved-param"]="guarded"
    FIXER_CONFIDENCE["unused-param"]="guarded"
    FIXER_CONFIDENCE["wp-filesystem"]="guarded"
    FIXER_CONFIDENCE["silenced-error"]="guarded"
    FIXER_CONFIDENCE["readdir"]="guarded"
    FIXER_CONFIDENCE["short-ternary"]="advisory"
    FIXER_CONFIDENCE["strict-comparison"]="advisory"

    # Run a fixer and capture its results for the sidecar.
    # Usage: run_fixer <rule_name> <fixer_binary> [args...]
    run_fixer() {
        local rule="$1"; shift
        local fixer_bin="$1"; shift

        if [ ! -f "$fixer_bin" ]; then
            return 0
        fi

        local fixer_output
        local fixer_before
        fixer_before="$(homeboy_mktemp 'homeboy-wp-fixer-before.XXXXXX')"
        homeboy_fix_results_capture "$fixer_before" "$PLUGIN_PATH"

        set +e
        fixer_output=$(php "$fixer_bin" "$@" 2>&1)
        local fixer_exit=$?
        set -e
        echo "$fixer_output"

        homeboy_fix_results_append_changed "$rule" "rewrite" "$fixer_before" "${FIXER_CONFIDENCE[$rule]:-advisory}" "$PLUGIN_PATH"
        rm -f "$fixer_before"

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
    # MUST run on full plugin path because its two-pass architecture builds a
    # rename manifest in Pass 1 and applies it to call sites in Pass 2. In scoped
    # fix-only mode, skip it rather than dirtying unrelated files outside the
    # review scope.
    if [ -n "${HOMEBOY_LINT_FILE:-}" ] || [ -n "${HOMEBOY_LINT_GLOB:-}" ]; then
        echo "Skipping reserved-param fixer for scoped lint fix; it requires a full-component scan."
    else
        run_fixer "reserved-param" "$RESERVED_PARAM_FIXER" "$PLUGIN_PATH"
    fi

    # Run phpcbf for remaining auto-fixable issues
    if [ -f "$PHPCBF_BIN" ]; then
        echo "Running auto-fix (phpcbf)..."

        # Auto-detect parallelism from available CPU cores
        PARALLEL_PROCS=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo "1")
        # Cap at 8 to avoid overwhelming the system
        if [ "$PARALLEL_PROCS" -gt 8 ]; then
            PARALLEL_PROCS=8
        fi

        # Build phpcbf command arguments as array for proper path escaping
        phpcbf_args=(--standard="$PHPCS_CONFIG")
        if [ "$PARALLEL_PROCS" -gt 1 ]; then
            phpcbf_args+=(--parallel="$PARALLEL_PROCS")
        fi
        if [ -n "$TEXT_DOMAIN" ]; then
            phpcbf_args+=(--runtime-set text_domain "$TEXT_DOMAIN")
        fi
        if [ -n "$PHP_VERSION" ]; then
            phpcbf_args+=(--runtime-set testVersion "${PHP_VERSION}-")
        fi
        phpcbf_args+=("${LINT_FILES[@]}")

        phpcbf_before="$(homeboy_mktemp 'homeboy-phpcbf-before.XXXXXX')"
        homeboy_fix_results_capture "$phpcbf_before" "$PLUGIN_PATH"

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
        homeboy_fix_results_append_changed "phpcbf" "format" "$phpcbf_before" "safe" "$PLUGIN_PATH"
        rm -f "$phpcbf_before"

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
        FIX_RESULTS_TMPFILE=$(homeboy_mktemp 'wordpress-fix-results.XXXXXX')
        printf '%s\n' "$HOMEBOY_FIX_RESULTS_JSON" > "$FIX_RESULTS_TMPFILE"
        write_json_array_sidecar_file "${HOMEBOY_FIX_PLAN_FILE}" "$FIX_RESULTS_TMPFILE"
        rm -f "$FIX_RESULTS_TMPFILE"
    fi

    # Write fix results sidecar for homeboy to consume
    if [ -n "${HOMEBOY_FIX_RESULTS_FILE:-}" ]; then
        if type homeboy_write_fix_results >/dev/null 2>&1; then
            homeboy_fix_results_write
        else
            echo "$HOMEBOY_FIX_RESULTS_JSON" > "${HOMEBOY_FIX_RESULTS_FILE}"
        fi
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
            done < <(find "$lint_target" -name '*.php' \
                -not -path '*/vendor/*' \
                -not -path '*/vendor_prefixed/*' \
                -not -path '*/vendor-prefixed/*' \
                -not -path '*/vendor_scoped/*' \
                -not -path '*/vendor-scoped/*' \
                -not -path '*/node_modules/*' \
                -not -path '*/dist/*' \
                -not -path '*/build/*' \
                -print0)
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

    # Fix-only mode always skips the validation pass — `homeboy refactor`
    # validates separately via the diagnose phase. Saves ~35s on large
    # codebases by avoiding a redundant PHPCS scan.
    echo ""
    echo "Fix-only mode: skipping validation (run 'homeboy lint' separately to validate)"
    exit 0
fi

# Validation
echo "Validating with PHPCS..."
fixable_count=0

# Build base phpcs arguments
phpcs_base_args=(--standard="$PHPCS_CONFIG")
phpcs_base_args+=(--ignore='*/vendor/*,*/vendor_prefixed/*,*/vendor-prefixed/*,*/vendor_scoped/*,*/vendor-scoped/*,*/node_modules/*,*/dist/*,*/build/*,*/tools/*,*/scoper.inc.php')

if [ "$WORDPRESS_LINT_ROLE" = "scoper_config" ]; then
    phpcs_base_args+=(--sniffs=Generic.PHP.Syntax)
elif [ "$WORDPRESS_LINT_ROLE" = "tooling" ]; then
    phpcs_base_args+=(--exclude=WordPress.WP.AlternativeFunctions,WordPress.PHP.DevelopmentFunctions,WordPress.Security.EscapeOutput)
fi

# Auto-detect parallelism from available CPU cores
if [ -z "${PARALLEL_PROCS:-}" ]; then
    PARALLEL_PROCS=$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo "1")
    if [ "$PARALLEL_PROCS" -gt 8 ]; then
        PARALLEL_PROCS=8
    fi
fi
if [ "$PARALLEL_PROCS" -gt 1 ]; then
    phpcs_base_args+=(--parallel="$PARALLEL_PROCS")
fi

if [ -n "$TEXT_DOMAIN" ]; then
    phpcs_base_args+=(--runtime-set text_domain "$TEXT_DOMAIN")
fi
if [ -n "$PHP_VERSION" ]; then
    phpcs_base_args+=(--runtime-set testVersion "${PHP_VERSION}-")
fi
if [[ "${HOMEBOY_ERRORS_ONLY:-}" == "1" ]]; then
    phpcs_base_args+=(--warning-severity=0)
fi
# Gate severity: PHPCS errors always block, but warnings are reported (still
# printed in the summary and counted in findings) without failing the phpcs
# step. PHPCS distinguishes errors from warnings precisely so CI can gate on
# errors while surfacing warnings; blocking on warnings pushed operators to
# `--skip-checks=lint`, which discarded the error gate too. `ignore_warnings_on_exit`
# makes PHPCS exit 0 when only warnings are present, so the exit code becomes an
# errors-only gate. Set HOMEBOY_LINT_FAIL_ON=warnings to restore legacy behavior
# where warnings also fail the gate (#2234).
HOMEBOY_LINT_FAIL_ON="${HOMEBOY_LINT_FAIL_ON:-errors}"
if [ "$HOMEBOY_LINT_FAIL_ON" != "warnings" ]; then
    phpcs_base_args+=(--runtime-set ignore_warnings_on_exit 1)
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
            echo "PHPCS SUMMARY: " . $errors . " errors, " . $warnings . " warnings\n";
            echo "Fixable: " . $fixable . " | Files with issues: " . $filesWithIssues . " of " . $files . "\n";
            echo "============================================\n";
        }
    ' 2>/dev/null)

    if [ -n "$summary" ]; then
        echo ""
        echo "$summary"
    fi

    # Prominent auto-fix nudge: when PHPCS reports auto-fixable findings, surface
    # a dedicated call-to-action block so developers see them before pushing.
    # This closes the behavior gap where warnings fire but the warn-only exit 0
    # lets auto-fixable findings slip through to reviewers (homeboy-extensions#229).
    fixable_count=$(echo "$json_output" | php -r '
        $json = json_decode(file_get_contents("php://stdin"), true);
        echo (int) ($json["totals"]["fixable"] ?? 0);
    ' 2>/dev/null || echo "0")

    if [ -n "$fixable_count" ] && [ "$fixable_count" -gt 0 ] 2>/dev/null; then
        fix_target="${HOMEBOY_COMPONENT_ID:-${HOMEBOY_PROJECT_ID:-<component>}}"
        echo ""
        echo "============================================"
        echo "AUTO-FIXABLE: ${fixable_count} lint finding(s) can be fixed automatically."
        fix_scope_args=""
        if [ -n "${HOMEBOY_CHANGED_SINCE:-}" ]; then
            fix_scope_args=" --changed-since ${HOMEBOY_CHANGED_SINCE}"
        fi
        echo "  Run:  homeboy refactor --from lint --write${fix_scope_args} ${fix_target}"
        echo "============================================"
    fi

    # Write annotations sidecar JSON for CI inline comments. Annotations are
    # Homeboy observability output, not a lint result — if the sidecar writer is
    # unavailable (standalone run without HOMEBOY_RUNTIME_SIDECAR_WRITER), skip
    # writing them rather than failing the lint step. A missing writer must
    # never masquerade as a lint finding (homeboy-extensions#1402).
    if [ -n "${HOMEBOY_ANNOTATIONS_DIR:-}" ] && [ -d "${HOMEBOY_ANNOTATIONS_DIR}" ] && type homeboy_sidecar_merge >/dev/null 2>&1; then
        _PHPCS_ANNOTATIONS_TMPFILE=$(homeboy_mktemp 'phpcs-annotations.XXXXXX')
        echo "$json_output" | php -r '
            ini_set("memory_limit", "-1");
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
            $outputFile = $argv[2] ?? "";
            if ($outputFile && !empty($annotations)) {
                file_put_contents($outputFile, json_encode($annotations, JSON_UNESCAPED_SLASHES) . "\n");
            }
        ' "$PLUGIN_PATH" "$_PHPCS_ANNOTATIONS_TMPFILE" 2>/dev/null || true
        homeboy_sidecar_merge annotation.phpcs "$_PHPCS_ANNOTATIONS_TMPFILE"
        rm -f "$_PHPCS_ANNOTATIONS_TMPFILE"
    fi

    # Write lint findings sidecar for homeboy baseline and categorized issues.
    # Transforms PHPCS JSON report into the current LintFinding shape; Homeboy
    # owns structured source metadata outside this sidecar payload.
    # Category is derived from the top-level PHPCS source namespace.
    if [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ]; then
        _PHPCS_FINDINGS_TMPFILE=$(homeboy_mktemp 'phpcs-findings.XXXXXX')
        echo "$json_output" | php -r '
            ini_set("memory_limit", "-1");
            require $argv[3];
            $json = json_decode(file_get_contents("php://stdin"), true);
            if (!$json || empty($json["files"])) {
                file_put_contents($argv[2], "[]\n");
                exit;
            }
            $componentPath = $argv[1] ?? "";
            $categoryMap = [
                "WordPress.Security" => "security",
                "WordPress.WP.I18n" => "i18n",
                "WordPress.PHP.YodaConditions" => "yoda",
                "WordPress.WhiteSpace" => "whitespace",
                "WordPress.DB" => "database",
                "WordPress.WP.AlternativeFunctions" => "wp-alternatives",
                "WordPress.WP.GlobalVariablesOverride" => "globals",
                "WordPress.CodeAnalysis" => "code-analysis",
                "WordPress.NamingConventions" => "naming",
                "WordPress.PHP.StrictComparisons" => "strict-comparisons",
                "WordPress.PHP.StrictInArray" => "strict-comparisons",
                "Generic.CodeAnalysis" => "code-analysis",
                "Generic.PHP" => "php",
                "Generic.Formatting" => "formatting",
                "Squiz" => "formatting",
                "PEAR" => "formatting",
                "PSR" => "formatting",
                "PHPCompatibility" => "compatibility",
            ];
            $findings = [];
            $readExcerpt = static function ($path, $line) {
                if (!$path || !$line || !is_readable($path)) {
                    return null;
                }

                $lines = @file($path, FILE_IGNORE_NEW_LINES);
                return $lines[$line - 1] ?? null;
            };
            foreach ($json["files"] as $filePath => $data) {
                $relPath = $filePath;
                if ($componentPath && strpos($filePath, $componentPath) === 0) {
                    $relPath = ltrim(substr($filePath, strlen($componentPath)), "/");
                }
                foreach ($data["messages"] ?? [] as $msg) {
                    $code = $msg["source"] ?? "unknown";
                    $line = $msg["line"] ?? 0;
                    $column = $msg["column"] ?? null;
                    $id = $relPath . "::" . $code . "::" . $line;
                    $message = ($msg["message"] ?? "Unknown") . " (" . $code . ")";
                    // Derive category from source namespace
                    $category = "other";
                    foreach ($categoryMap as $prefix => $cat) {
                        if (strpos($code, $prefix) === 0) {
                            $category = $cat;
                            break;
                        }
                    }
                    $findings[] = [
                        "id" => $id,
                        "tool" => "phpcs",
                        "file" => $relPath,
                        "line" => $line,
                        "column" => $column,
                        "severity" => strtolower($msg["type"] ?? "error"),
                        "code" => $code,
                        "rule" => $code,
                        "category" => $category,
                        "message" => $message,
                        "fixable" => (bool) ($msg["fixable"] ?? false),
                        "excerpt" => $readExcerpt($filePath, $line),
                    ];
                }
            }
            $findings = homeboy_assign_stable_lint_fingerprints($findings);
            file_put_contents($argv[2], json_encode($findings, JSON_UNESCAPED_SLASHES) . "\n");
        ' "$PLUGIN_PATH" "$_PHPCS_FINDINGS_TMPFILE" "$STABLE_FINGERPRINT_HELPER" 2>/dev/null || true
        # Writing the findings sidecar is best-effort observability; never let a
        # sidecar-writer failure fail the lint gate (homeboy-extensions#1402).
        merge_findings_into_sidecar "$_PHPCS_FINDINGS_TMPFILE" || true
        rm -f "$_PHPCS_FINDINGS_TMPFILE"
    fi
fi

# Create temp files for per-tool findings before ESLint/PHPStan run. Each tool
# writes its own parsed array, then the core sidecar helper owns final merging.
_ESLINT_FINDINGS_TMPFILE=""
_PHPSTAN_FINDINGS_TMPFILE=""
HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE=""
if [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ]; then
    _ESLINT_FINDINGS_TMPFILE=$(homeboy_mktemp 'eslint-findings.XXXXXX')
    _PHPSTAN_FINDINGS_TMPFILE=$(homeboy_mktemp 'phpstan-findings.XXXXXX')
fi
if [ -n "${HOMEBOY_LINT_PRODUCERS_FILE:-}" ]; then
    HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE=$(homeboy_mktemp 'phpstan-producer.XXXXXX.json')
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
    ESLINT_EXIT=0

    if ! should_run_step "eslint"; then
        echo ""
        echo "Skipping ESLint (step filter)"
    elif [ -f "$ESLINT_RUNNER" ]; then
        echo ""
        set +e
        _HOMEBOY_ESLINT_FINDINGS_FILE="${_ESLINT_FINDINGS_TMPFILE:-}" \
            bash "$ESLINT_RUNNER"
        ESLINT_EXIT=$?
        set -e

        if [ "$ESLINT_EXIT" -ne 0 ]; then
            ESLINT_PASSED=0
        fi
    else
        echo "bootstrap failure: ESLint runner not found at $ESLINT_RUNNER" >&2
        ESLINT_PASSED=0
        ESLINT_EXIT=2
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
        _HOMEBOY_PHPSTAN_FINDINGS_FILE="${_PHPSTAN_FINDINGS_TMPFILE:-}" \
            HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE="${HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE:-}" \
            HOMEBOY_SUMMARY_MODE=1 bash "$phpstan_runner"
        local phpstan_exit=$?
        set -e

        if [ "$phpstan_exit" -ne 0 ]; then
            return 1
        fi
        return 0
    }

    # Run PHPStan after PHPCS/ESLint so the caller gets the full lint signal
    # before the aggregate exit code is computed.
    PHPSTAN_PASSED=1
    run_phpstan_summary || PHPSTAN_PASSED=0

    # Merge ESLint + PHPStan findings into the final baseline sidecar. PHPCS was
    # already merged after its JSON report was parsed.
    if [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ] && [ -n "$_ESLINT_FINDINGS_TMPFILE" ] && [ -f "$_ESLINT_FINDINGS_TMPFILE" ]; then
        merge_findings_into_sidecar "$_ESLINT_FINDINGS_TMPFILE"
        rm -f "$_ESLINT_FINDINGS_TMPFILE"
    fi
    if [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ] && [ -n "$_PHPSTAN_FINDINGS_TMPFILE" ] && [ -f "$_PHPSTAN_FINDINGS_TMPFILE" ]; then
        merge_findings_into_sidecar "$_PHPSTAN_FINDINGS_TMPFILE"
        rm -f "$_PHPSTAN_FINDINGS_TMPFILE"
    fi
    write_lint_producers_sidecar "$PHPCS_PASSED" "$ESLINT_PASSED" "$PHPSTAN_PASSED"
    [ -n "${HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE:-}" ] && rm -f "$HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE"

    if [ "$PHPCS_PASSED" -eq 1 ] && [ "$ESLINT_PASSED" -eq 1 ] && [ "$PHPSTAN_PASSED" -eq 1 ]; then
        echo "Linting passed"
        exit 0
    else
        echo "Linting found issues (see above)"
        if [ "${fixable_count:-0}" -gt 0 ] 2>/dev/null; then
            echo "Auto-fixable findings remain; run the refactor command above before pushing."
        fi
        [ "$ESLINT_EXIT" -ge 2 ] && exit 2
        exit 1
    fi
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
ESLINT_EXIT=0

if ! should_run_step "eslint"; then
    echo ""
    echo "Skipping ESLint (step filter)"
elif [ -f "$ESLINT_RUNNER" ]; then
    echo ""
    set +e
    _HOMEBOY_ESLINT_FINDINGS_FILE="${_ESLINT_FINDINGS_TMPFILE:-}" \
        bash "$ESLINT_RUNNER"
    ESLINT_EXIT=$?
    set -e

    if [ "$ESLINT_EXIT" -ne 0 ]; then
        ESLINT_PASSED=0
    fi
else
    echo "bootstrap failure: ESLint runner not found at $ESLINT_RUNNER" >&2
    ESLINT_PASSED=0
    ESLINT_EXIT=2
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
    _HOMEBOY_PHPSTAN_FINDINGS_FILE="${_PHPSTAN_FINDINGS_TMPFILE:-}" \
        HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE="${HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE:-}" \
        HOMEBOY_SUMMARY_MODE=1 bash "$phpstan_runner"
    local phpstan_exit=$?
    set -e

    if [ "$phpstan_exit" -ne 0 ]; then
        # Return 1 to indicate issues found, but caller handles exit code
        return 1
    fi
    return 0
}

# Run PHPStan after PHPCS/ESLint so the caller gets the full lint signal
# before the aggregate exit code is computed.
PHPSTAN_PASSED=1
run_phpstan || PHPSTAN_PASSED=0

# Merge ESLint + PHPStan findings into the final baseline sidecar. PHPCS was
# already merged after its JSON report was parsed.
if [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ] && [ -n "$_ESLINT_FINDINGS_TMPFILE" ] && [ -f "$_ESLINT_FINDINGS_TMPFILE" ]; then
    merge_findings_into_sidecar "$_ESLINT_FINDINGS_TMPFILE"
    rm -f "$_ESLINT_FINDINGS_TMPFILE"
fi
if [ -n "${HOMEBOY_LINT_FINDINGS_FILE:-}" ] && [ -n "$_PHPSTAN_FINDINGS_TMPFILE" ] && [ -f "$_PHPSTAN_FINDINGS_TMPFILE" ]; then
    merge_findings_into_sidecar "$_PHPSTAN_FINDINGS_TMPFILE"
    rm -f "$_PHPSTAN_FINDINGS_TMPFILE"
fi
write_lint_producers_sidecar "$PHPCS_PASSED" "$ESLINT_PASSED" "$PHPSTAN_PASSED"
[ -n "${HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE:-}" ] && rm -f "$HOMEBOY_PHPSTAN_PRODUCER_METADATA_FILE"

if [ "$PHPCS_PASSED" -eq 1 ] && [ "$ESLINT_PASSED" -eq 1 ] && [ "$PHPSTAN_PASSED" -eq 1 ]; then
    echo ""
    echo "Linting passed"
    exit 0
else
    echo ""
    echo "Linting found issues (see above)"
    if [ "${fixable_count:-0}" -gt 0 ] 2>/dev/null; then
        echo "Auto-fixable findings remain; run the refactor command above before pushing."
    fi
    [ "$ESLINT_EXIT" -ge 2 ] && exit 2
    exit 1
fi
