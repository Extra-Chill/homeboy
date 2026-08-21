#!/usr/bin/env bash
set -euo pipefail

# Standalone JavaScript linting script using ESLint
# Supports fix-only mode via HOMEBOY_FIX_ONLY=1 (sent by `homeboy refactor`)
# Supports summary mode via HOMEBOY_SUMMARY_MODE=1
#
# HOMEBOY_FIX_ONLY=1 is the single auto-fix contract: the runner executes
# ESLint --fix and exits before the validation pass (#1145).

# Debug environment variables (only shown when HOMEBOY_DEBUG=1)
if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: ESLint Environment variables:"
    echo "HOMEBOY_EXTENSION_PATH=${HOMEBOY_EXTENSION_PATH:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_ID=${HOMEBOY_COMPONENT_ID:-NOT_SET}"
    echo "HOMEBOY_COMPONENT_PATH=${HOMEBOY_COMPONENT_PATH:-NOT_SET}"
    echo "HOMEBOY_FIX_ONLY=${HOMEBOY_FIX_ONLY:-NOT_SET}"
    echo "HOMEBOY_SUMMARY_MODE=${HOMEBOY_SUMMARY_MODE:-NOT_SET}"
    echo "HOMEBOY_ERRORS_ONLY=${HOMEBOY_ERRORS_ONLY:-NOT_SET}"
fi

# Resolve execution context (shared helper)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHARED_LIB_DIR="${HOMEBOY_SHARED_LIB_DIR:-}"
if [ -z "$SHARED_LIB_DIR" ] && [ -n "${HOMEBOY_EXTENSION_PATH:-}" ] && [ -d "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" ]; then
    SHARED_LIB_DIR="$(cd "${HOMEBOY_EXTENSION_PATH}/../scripts/lib" && pwd)"
fi
SHARED_LIB_DIR="${SHARED_LIB_DIR:-$(cd "${SCRIPT_DIR}/../../../scripts/lib" && pwd)}"
RESOLVE_CONTEXT_HELPER="${HOMEBOY_RUNTIME_RESOLVE_CONTEXT:-}"
# Homeboy core provides the sidecar writer via HOMEBOY_RUNTIME_SIDECAR_WRITER.
# When it is genuinely unavailable, the findings/annotation sidecar writes
# below degrade to no-ops — they are observability output, not lint results, so
# they must never fail the lint gate (homeboy-extensions#1402).
SIDECAR_WRITER_HELPER="${HOMEBOY_RUNTIME_SIDECAR_WRITER:-}"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/runner-harness.sh"
# shellcheck source=/dev/null
source "${SHARED_LIB_DIR}/lint-findings-adapter.sh"
if [ -n "$RESOLVE_CONTEXT_HELPER" ]; then
    # shellcheck source=/dev/null
    source "${RESOLVE_CONTEXT_HELPER}"
    homeboy_resolve_context --component-alias PLUGIN_PATH
else
    PLUGIN_PATH="${HOMEBOY_COMPONENT_PATH:-$(pwd)}"
    EXTENSION_PATH="${HOMEBOY_EXTENSION_PATH:-$(cd "${SCRIPT_DIR}/../.." && pwd)}"
fi
homeboy_runner_harness_source_if_file "$SIDECAR_WRITER_HELPER"

write_eslint_findings_sidecar() {
    local target="$1"
    local source="$2"

    [ -z "$target" ] && return 0
    [ ! -s "$source" ] && return 0

    rm -f "$target"
    local previous_target="${HOMEBOY_LINT_FINDINGS_FILE:-}"
    HOMEBOY_LINT_FINDINGS_FILE="$target"
    homeboy_lint_findings_merge_file "$source"
    HOMEBOY_LINT_FINDINGS_FILE="$previous_target"
}

# Return false for dependency trees and generated assets. Repository-specific
# ignores remain ESLint configuration policy; this prevents generated output
# from becoming an explicit CLI target that can bypass that policy.
is_lint_source_file() {
    case "$1" in
        */node_modules/*|*/vendor/*|*/vendor_prefixed/*|*/vendor-prefixed/*|*/vendor_scoped/*|*/vendor-scoped/*|*/dist/*|*/build/*|*.min.js)
            return 1
            ;;
    esac

    return 0
}

SOURCE_FILES=()
while IFS= read -r source_file; do
    if is_lint_source_file "$source_file"; then
        SOURCE_FILES+=("${source_file#"$PLUGIN_PATH"/}")
    fi
done < <(find "$PLUGIN_PATH" -type f \( -name "*.js" -o -name "*.jsx" -o -name "*.ts" -o -name "*.tsx" \) 2>/dev/null)

if [ ${#SOURCE_FILES[@]} -eq 0 ]; then
    echo "No JavaScript files found, skipping ESLint."
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

    if ! is_lint_source_file "${LINT_FILES[0]}"; then
        echo "Skipping generated or dependency JavaScript file: ${HOMEBOY_LINT_FILE}"
        exit 0
    fi
elif [ -n "${HOMEBOY_LINT_GLOB:-}" ]; then
    cd "$PLUGIN_PATH"

    MATCHED_FILES=()
    set +e
    eval 'for f in '"${HOMEBOY_LINT_GLOB}"'; do [ -e "$f" ] && MATCHED_FILES+=("$f"); done'
    glob_exit=$?
    set -e
    if [ "$glob_exit" -ne 0 ] && [ ${#MATCHED_FILES[@]} -eq 0 ]; then
        MATCHED_FILES=()
    fi

    JS_FILES=()
    for matched_file in "${MATCHED_FILES[@]}"; do
        case "$matched_file" in
            *.js|*.jsx|*.ts|*.tsx)
                if is_lint_source_file "$matched_file"; then
                    JS_FILES+=("$matched_file")
                fi
                ;;
        esac
    done

    if [ ${#JS_FILES[@]} -eq 0 ]; then
        echo "No JS/TS files match pattern: ${HOMEBOY_LINT_GLOB}"
        exit 0
    fi

    echo "Linting ${#JS_FILES[@]} JS/TS files matching: ${HOMEBOY_LINT_GLOB}"
    LINT_FILES=("${JS_FILES[@]}")
    cd - > /dev/null
else
    echo "Running JavaScript linting..."
    LINT_FILES=("${SOURCE_FILES[@]}")
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "Extension path: $EXTENSION_PATH"
    echo "Plugin path: $PLUGIN_PATH"
    echo "Lint files: ${LINT_FILES[*]}"
    echo "Fix-only: ${HOMEBOY_FIX_ONLY:-0}"
fi

ESLINT_BIN="${EXTENSION_PATH}/node_modules/.bin/eslint"
ESLINT_CONFIG="${EXTENSION_PATH}/eslint.runner.config.mjs"

# Validate tools exist
if [ ! -f "$ESLINT_BIN" ]; then
    echo "Warning: ESLint not found at $ESLINT_BIN, skipping JavaScript linting"
    exit 0
fi

if [ ! -f "$ESLINT_CONFIG" ]; then
    echo "Warning: ESLint runner config not found at $ESLINT_CONFIG, skipping JavaScript linting"
    exit 0
fi

# Auto-detect text domain from plugin/theme header (shared helper)
DETECT_COMPONENT_HELPER="${HOMEBOY_RUNTIME_DETECT_COMPONENT:-${SCRIPT_DIR}/../lib/detect-component.sh}"
# shellcheck source=../lib/detect-component.sh
source "${DETECT_COMPONENT_HELPER}"
homeboy_detect_component "$PLUGIN_PATH" || true

TEXT_DOMAIN="${HOMEBOY_COMPONENT_TEXT_DOMAIN:-}"
if [ -n "$TEXT_DOMAIN" ] && [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: Detected text domain: $TEXT_DOMAIN"
fi

# Build base ESLint arguments. The provider config composes a standard flat
# config only from this resolved component path, never from a primary checkout.
export HOMEBOY_ESLINT_COMPONENT_PATH="$PLUGIN_PATH"
eslint_base_args=(--config "$ESLINT_CONFIG")

if [ -n "$TEXT_DOMAIN" ]; then
    eslint_base_args+=(--rule "@wordpress/i18n-text-domain: [error, { allowedTextDomain: \"$TEXT_DOMAIN\" }]")
fi

if [[ "${HOMEBOY_ERRORS_ONLY:-}" == "1" ]]; then
    eslint_base_args+=(--quiet)
fi

# Run from plugin directory to ensure jsconfig.json is found by import resolver
cd "$PLUGIN_PATH"

# Fix-only mode: run ESLint --fix and exit before the validation pass.
# Sent by `homeboy refactor --from lint --write` — the engine validates separately.
if [[ "${HOMEBOY_FIX_ONLY:-}" == "1" ]]; then
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
    echo "Fix-only mode: skipping validation (run 'homeboy lint' separately to validate)"
    exit 0
fi

# Get JSON report for summary
homeboy_runner_harness_temp ESLINT_JSON_STDERR "homeboy-eslint-stderr.XXXXXX"
set +e
json_output=$("$ESLINT_BIN" "${eslint_base_args[@]}" --format json "${LINT_FILES[@]}" 2>"$ESLINT_JSON_STDERR")
json_exit=$?
set -e

# ESLint normally writes its report to stdout. Keep fatal diagnostics visible
# when it cannot produce JSON rather than discarding the only actionable error.
if [ -z "$json_output" ] && [ -s "$ESLINT_JSON_STDERR" ]; then
    cat "$ESLINT_JSON_STDERR" >&2
fi

# Write ESLint lint findings sidecar for homeboy baseline and drill-down.
# The top-level lint runner passes a temp file here, then merges it with PHPCS
# and PHPStan findings. Direct ESLint runs may write HOMEBOY_LINT_FINDINGS_FILE.
ESLINT_FINDINGS_FILE="${_HOMEBOY_ESLINT_FINDINGS_FILE:-${HOMEBOY_LINT_FINDINGS_FILE:-}}"
if [ -n "$ESLINT_FINDINGS_FILE" ] && [ -n "$json_output" ] && command -v node &> /dev/null; then
    homeboy_runner_harness_temp ESLINT_FINDINGS_TMPFILE "homeboy-eslint-findings.XXXXXX"
    node -e '
        const fs = require("fs");
        const path = require("path");

        const data = JSON.parse(process.argv[1]);
        const componentPath = process.argv[2] || "";
        const outputFile = process.argv[3];
        const readExcerpt = (filePath, line) => {
            if (!filePath || !line) return null;
            try {
                return fs.readFileSync(filePath, "utf8").split(/\r?\n/)[line - 1] ?? null;
            } catch (_) {
                return null;
            }
        };
        const relPath = (filePath) => {
            if (componentPath && filePath.startsWith(componentPath)) {
                return filePath.slice(componentPath.length).replace(/^\/+/, "");
            }
            return filePath;
        };
        const findings = [];
        for (const file of data) {
            const filePath = file.filePath || "";
            const fileRelPath = relPath(filePath);
            for (const msg of file.messages || []) {
                const rule = msg.ruleId || "unknown";
                const code = `eslint.${rule}`;
                const line = msg.line || 0;
                const column = msg.column ?? null;
                const id = `${fileRelPath}::${code}::${line}`;
                const message = `${msg.message || "Unknown"} (${code})`;
                findings.push({
                    id,
                    tool: "eslint",
                    file: fileRelPath,
                    line,
                    column,
                    severity: msg.severity === 1 ? "warning" : "error",
                    code,
                    rule: code,
                    category: "eslint",
                    message,
                    fixable: Boolean(msg.fix),
                    fingerprint: require("crypto").createHash("sha1").update(id).digest("hex"),
                    excerpt: readExcerpt(filePath, line),
                });
            }
        }
        fs.writeFileSync(outputFile, JSON.stringify(findings) + "\n");
    ' "$json_output" "$PLUGIN_PATH" "$ESLINT_FINDINGS_TMPFILE" 2>/dev/null || true
    # Best-effort observability; never fail the gate on a sidecar write.
    write_eslint_findings_sidecar "$ESLINT_FINDINGS_FILE" "$ESLINT_FINDINGS_TMPFILE" || true
fi

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
