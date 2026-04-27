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
# Output: writes a LintFinding[] JSON array to HOMEBOY_LINT_FINDINGS_FILE.
# Each finding has `id`, `message`, `category` (matches LintFinding struct
# in homeboy/src/core/extension/lint/baseline.rs).
#
# We try to consume ESLint's machine-readable JSON output (--format=json)
# when ESLint is the runner. For arbitrary `npm run lint` scripts the user
# wired up, we fall back to "exit code is the truth, findings array stays
# empty unless we recognize the format."

if ((BASH_VERSINFO[0] < 4)); then
    echo "ERROR: bash 4.0+ required (found ${BASH_VERSION})" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/resolve-context.sh
source "${SCRIPT_DIR}/../lib/resolve-context.sh"
homeboy_resolve_context
homeboy_require_package_json
homeboy_detect_package_manager

FAILURE_TRAP_HELPER="${HOMEBOY_RUNTIME_FAILURE_TRAP:-}"
# shellcheck source=/dev/null
if [ -n "$FAILURE_TRAP_HELPER" ] && [ -f "$FAILURE_TRAP_HELPER" ]; then
    source "$FAILURE_TRAP_HELPER"
    homeboy_init_failure_trap
else
    FAILED_STEP=""
    FAILURE_OUTPUT=""
fi

FIX_MODE="${HOMEBOY_FIX_ONLY:-0}"
FINDINGS_FILE="${HOMEBOY_LINT_FINDINGS_FILE:-${PROJECT_PATH}/.node-lint-findings.json}"

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
    LINT_CMD="$HOMEBOY_NODE_LINT_COMMAND"
elif [ "$FIX_MODE" = "1" ] && homeboy_has_npm_script "lint:fix"; then
    LINT_CMD="$PKG_RUN lint:fix"
elif homeboy_has_npm_script "lint"; then
    LINT_CMD="$PKG_RUN lint"
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
        echo "[]" > "$FINDINGS_FILE"
        exit 1
    fi
else
    # Nothing to lint — emit empty findings, exit clean.
    echo ""
    echo "⚠ No lint surface detected (no scripts.lint, no eslint config)."
    echo "  Skipping lint — emitting empty findings."
    echo ""
    echo "[]" > "$FINDINGS_FILE"
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

# ── Parse findings ──
# When we ran eslint with --format=json we have machine-readable output.
# Otherwise, we fall back to "exit code is the truth" with empty findings.
if [ $USE_ESLINT_JSON -eq 1 ] && [ -s "$OUTPUT_FILE" ]; then
    # ESLint JSON: array of {filePath, messages: [{ruleId, message, line, column, severity}, ...]}
    # severity: 1=warning, 2=error. We surface errors as findings; warnings
    # only if HOMEBOY_LINT_INCLUDE_WARNINGS=1 (matches Rust extension's
    # ratchet-friendly default).
    INCLUDE_WARNINGS="${HOMEBOY_LINT_INCLUDE_WARNINGS:-0}"
    node - "$OUTPUT_FILE" "$FINDINGS_FILE" "$INCLUDE_WARNINGS" <<'NODEJS'
const fs = require('node:fs');
const [, , inputFile, outputFile, includeWarningsFlag] = process.argv;
const includeWarnings = includeWarningsFlag === '1';

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
        const loc = `${file.filePath}:${msg.line || 0}:${msg.column || 0}`;
        findings.push({
            // Stable fingerprint id: rule + path + 1-based line. Matches
            // baseline expectations — same finding twice deduplicates.
            id: `eslint:${rule}:${loc}`,
            message: msg.message || '(no message)',
            category: msg.severity === 2 ? 'error' : 'warning',
        });
    }
}

fs.writeFileSync(outputFile, JSON.stringify(findings, null, 2));
NODEJS
else
    # Unknown runner output — emit empty findings, rely on exit code.
    echo "[]" > "$FINDINGS_FILE"
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
