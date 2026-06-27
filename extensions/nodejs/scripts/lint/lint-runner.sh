#!/usr/bin/env bash
set -euo pipefail

# Node.js lint runner for `homeboy lint`.
#
# Detection order:
#   1. HOMEBOY_NODE_LINT_COMMAND env var (full override)
#   2. package.json scripts.lint (`{npm,pnpm,yarn} run lint`)
#   3. Built-in default: `npx eslint .` if eslint config is present
#   4. No-op if no lint surface detected (emit empty findings array)
#
# In fix mode (HOMEBOY_FIX_ONLY=1), prefers `lint:fix` script if defined,
# otherwise falls back to `eslint . --fix`.
#
# Output: writes a normalized LintFinding[] JSON array to
# HOMEBOY_LINT_FINDINGS_FILE. Core consumes `id`, `message`, and `category`;
# richer fields remain available through the flattened metadata map.
#
# We try to consume ESLint's machine-readable JSON output (--format=json)
# when ESLint is the runner. For arbitrary `npm run lint` scripts the user
# wired up, we fall back to "exit code is the truth, findings array stays
# empty unless we recognize the format."

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER_PRELUDE="${HOMEBOY_RUNTIME_RUNNER_PRELUDE:?HOMEBOY_RUNTIME_RUNNER_PRELUDE is required}"
FIX_RESULTS_HELPER="${HOMEBOY_RUNTIME_FIX_RESULTS:-${SCRIPT_DIR}/../../../scripts/lib/fix-results.sh}"
# shellcheck source=/dev/null
source "$RUNNER_PRELUDE"
homeboy_runner_init --bash 4 --sidecar-writer --failure-trap
# shellcheck source=../../../scripts/lib/fix-results.sh
source "$FIX_RESULTS_HELPER"
# shellcheck source=../lib/node-helpers.sh
source "${SCRIPT_DIR}/../lib/node-helpers.sh"
homeboy_require_package_json
homeboy_detect_package_manager

FIX_MODE="${HOMEBOY_FIX_ONLY:-0}"
FINDINGS_FILE="${HOMEBOY_LINT_FINDINGS_FILE:-${PROJECT_PATH}/.node-lint-findings.json}"
export HOMEBOY_LINT_FINDINGS_FILE="$FINDINGS_FILE"
if ! type homeboy_sidecar_merge >/dev/null 2>&1; then
    echo "Error: HOMEBOY_RUNTIME_SIDECAR_WRITER is required to write lint findings" >&2
    exit 1
fi

# Detect if eslint is available locally (vendored in node_modules) — that's
# our preferred runner because we can ask for JSON output.
ESLINT_BIN=""
if [ -x "${PROJECT_PATH}/node_modules/.bin/eslint" ]; then
    ESLINT_BIN="${PROJECT_PATH}/node_modules/.bin/eslint"
fi

# Detect if an eslint config exists somewhere reasonable.
HAS_ESLINT_CONFIG=0
for cfg in "eslint.config.js" "eslint.config.mjs" "eslint.config.cjs" \
           ".eslintrc.js" ".eslintrc.cjs" ".eslintrc.json" ".eslintrc.yml" ".eslintrc.yaml"; do
    if [ -f "${PROJECT_PATH}/${cfg}" ]; then
        HAS_ESLINT_CONFIG=1
        break
    fi
done

# Resolve the lint command.
USE_ESLINT_JSON=0
if [ -n "${HOMEBOY_NODE_LINT_COMMAND:-}" ]; then
    if [ "$HOMEBOY_NODE_LINT_COMMAND" = "__homeboy_typecheck" ]; then
        if homeboy_has_npm_script "typecheck"; then
            LINT_CMD="$(homeboy_project_run_script_command typecheck)"
        else
            FAILED_STEP="No typecheck script defined"
            FAILURE_OUTPUT="CI job requested typecheck, but package.json does not define scripts.typecheck. Set HOMEBOY_NODE_LINT_COMMAND to a project-specific command or add scripts.typecheck."
            homeboy_sidecar_write lint.findings
            exit 1
        fi
    else
        LINT_CMD="$HOMEBOY_NODE_LINT_COMMAND"
    fi
elif [ "$FIX_MODE" = "1" ] && homeboy_has_npm_script "lint:fix"; then
    LINT_CMD="$(homeboy_project_run_script_command lint:fix)"
elif homeboy_has_npm_script "lint"; then
    LINT_CMD="$(homeboy_project_run_script_command lint)"
    [ "$FIX_MODE" = "1" ] && LINT_CMD="$LINT_CMD -- --fix"
elif [ $HAS_ESLINT_CONFIG -eq 1 ]; then
    if [ -n "$ESLINT_BIN" ]; then
        if [ "$FIX_MODE" = "1" ]; then
            LINT_CMD="$ESLINT_BIN . --fix"
        else
            LINT_CMD="$ESLINT_BIN . --format=json"
            USE_ESLINT_JSON=1
        fi
    else
        # eslint config exists but isn't installed locally — bail with a
        # clear message rather than silently npx-installing it.
        FAILED_STEP="eslint config found but eslint not installed"
        FAILURE_OUTPUT="Run: npm i -D eslint (or your package manager equivalent) in ${PROJECT_PATH}"
        homeboy_sidecar_write lint.findings
        exit 1
    fi
else
    # Nothing to lint — emit empty findings, exit clean.
    echo ""
    echo "⚠ No lint surface detected (no scripts.lint, no eslint config)."
    echo "  Skipping lint — emitting empty findings."
    echo ""
    homeboy_sidecar_write lint.findings
    exit 0
fi

if [ "${HOMEBOY_DEBUG:-}" = "1" ]; then
    echo "DEBUG: lint command: $LINT_CMD" >&2
    echo "DEBUG: fix mode: $FIX_MODE" >&2
fi

echo "Running Node.js lint..."
echo "  Component: ${COMPONENT_ID} (${PROJECT_PATH})"
echo "  Command:   ${LINT_CMD}"
[ "$FIX_MODE" = "1" ] && echo "  Mode:      fix"
echo ""

cd "$PROJECT_PATH"

OUTPUT_FILE=$(mktemp "${TMPDIR:-/tmp}/homeboy-node-lint.XXXXXX")
FIX_BEFORE=""
if [ "$FIX_MODE" = "1" ]; then
    FIX_BEFORE=$(mktemp "${TMPDIR:-/tmp}/homeboy-node-lint-before.XXXXXX")
    homeboy_fix_results_capture "$FIX_BEFORE" "$PROJECT_PATH"
fi
set +e
if [ $USE_ESLINT_JSON -eq 1 ]; then
    # Capture JSON-only stream; let stderr surface to the user.
    # shellcheck disable=SC2086
    $LINT_CMD > "$OUTPUT_FILE" 2>&1
    LINT_EXIT=$?
else
    # shellcheck disable=SC2086
    $LINT_CMD "$@" 2>&1 | tee "$OUTPUT_FILE"
    LINT_EXIT=${PIPESTATUS[0]}
fi
set -e

if [ "$FIX_MODE" = "1" ] && [ -n "$FIX_BEFORE" ]; then
    homeboy_fix_results_append_changed "nodejs_lint" "rewrite" "$FIX_BEFORE" "" "$PROJECT_PATH"
    rm -f "$FIX_BEFORE"
    homeboy_fix_results_write
fi

# ── Parse findings ──
# When we ran eslint with --format=json we have machine-readable output.
# Otherwise, we fall back to "exit code is the truth" with empty findings.
if [ $USE_ESLINT_JSON -eq 1 ] && [ -s "$OUTPUT_FILE" ]; then
    # ESLint JSON: array of {filePath, messages: [{ruleId, message, line, column, severity}, ...]}
    # severity: 1=warning, 2=error. We surface errors as findings; warnings
    # only if HOMEBOY_LINT_INCLUDE_WARNINGS=1 (matches Rust extension's
    # ratchet-friendly default).
    INCLUDE_WARNINGS="${HOMEBOY_LINT_INCLUDE_WARNINGS:-0}"
    FINDINGS_TMP="$(mktemp)"
    node - "$OUTPUT_FILE" "$FINDINGS_TMP" "$INCLUDE_WARNINGS" "$PROJECT_PATH" <<'NODEJS'
const crypto = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const [, , inputFile, outputFile, includeWarningsFlag, projectPath] = process.argv;
const includeWarnings = includeWarningsFlag === '1';

function relativeFile(filePath) {
    if (!filePath) return undefined;
    const absolute = path.resolve(filePath);
    const relative = path.relative(projectPath, absolute).replaceAll(path.sep, '/');
    return relative && !relative.startsWith('..') ? relative : absolute;
}

function readExcerpt(filePath, line) {
    if (!filePath || !Number.isFinite(line) || line <= 0) return undefined;
    try {
        return fs.readFileSync(filePath, 'utf8').split(/\r?\n/)[line - 1]?.trim() || undefined;
    } catch {
        return undefined;
    }
}

let raw;
try {
    raw = fs.readFileSync(inputFile, 'utf8').trim();
} catch (err) {
    fs.writeFileSync(outputFile, '[]');
    process.exit(0);
}

// ESLint may print non-JSON banner on stderr-merged output; find first '[' and
// last ']' as a recovery path.
let jsonStr = raw;
if (!jsonStr.startsWith('[')) {
    const start = jsonStr.indexOf('[');
    const end = jsonStr.lastIndexOf(']');
    if (start >= 0 && end > start) jsonStr = jsonStr.slice(start, end + 1);
}

let report;
try {
    report = JSON.parse(jsonStr);
} catch {
    fs.writeFileSync(outputFile, '[]');
    process.exit(0);
}

const findings = [];
for (const file of report) {
    for (const msg of file.messages || []) {
        if (!includeWarnings && msg.severity !== 2) continue;
        const rule = msg.ruleId || 'parse-error';
        const relative = relativeFile(file.filePath);
        const line = Number(msg.line || 0);
        const column = Number(msg.column || 0);
        const loc = `${relative || file.filePath}:${line}:${column}`;
        const id = `eslint:${rule}:${loc}`;
        const severity = msg.severity === 2 ? 'error' : 'warning';
        const excerpt = readExcerpt(file.filePath, line);
        findings.push({
            // Stable fingerprint id: rule + path + 1-based line. Matches
            // baseline expectations — same finding twice deduplicates.
            id,
            file: relative,
            line,
            column,
            severity,
            source: 'eslint',
            code: rule,
            message: msg.message || '(no message)',
            category: severity,
            fixable: Boolean(msg.fix),
            fingerprint: crypto.createHash('sha1').update(id).digest('hex'),
            ...(excerpt ? { excerpt } : {}),
        });
    }
}

fs.writeFileSync(outputFile, JSON.stringify(findings, null, 2));
NODEJS
    homeboy_sidecar_merge lint.findings "$FINDINGS_TMP"
    rm -f "$FINDINGS_TMP"
else
    # Unknown runner output — emit empty findings, rely on exit code.
    homeboy_sidecar_write lint.findings
fi

rm -f "$OUTPUT_FILE"

if [ "$FIX_MODE" = "1" ]; then
    # In fix mode the engine runs validation separately. Just report exit.
    exit $LINT_EXIT
fi

if [ $LINT_EXIT -ne 0 ]; then
    FAILED_STEP="Lint reported errors (exit $LINT_EXIT)"
fi

exit $LINT_EXIT
