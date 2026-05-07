#!/usr/bin/env bash
#
# Sed-replacement-escape regression smoke test.
#
# Asserts that the `sed_escape_replacement` helper used by
# bench-runner-playground.sh and test-runner-playground.sh correctly
# escapes `\` and `&` so JSON values round-trip through the sed `s`
# command without corruption.
#
# Why this matters
# ----------------
# GNU sed (CI) treats `&` in the replacement string as a backreference
# to the matched pattern and `\X` as an escape sequence. JSON values
# routinely contain `\"` (escaped quote) and may contain literal `&`
# (e.g. inside an arbitrary string payload). Substituting an unescaped
# JSON value into a runner template with `s${DELIM}{{PLACEHOLDER}}${DELIM}${VALUE}${DELIM}`
# silently mangles the JSON — `\"` becomes `"` (drops the backslash) and
# `&` gets replaced with the matched placeholder. The PHP-side
# json_decode then returns null and ALL declared bench_env /
# wp_config_defines entries drop, including unrelated values like
# GITHUB_TOKEN.
#
# This smoke replays the corruption without standing up Playground:
#   1. Renders a tiny inline template with the historic (broken)
#      substitution and asserts the corruption pattern.
#   2. Renders the same template using the escaped helper and asserts
#      bit-for-bit JSON round-trip.
#
# Run via: bash wordpress/scripts/bench/playground-bench-env-sed-escape-smoke.sh
# Exit:    0 = round-trips, non-zero = regression

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Pathological JSON value covering both metacharacters:
#   - PAYLOAD contains `\"` (escaped JSON quotes within a string).
#   - The text `a & b` contains a literal `&`.
#   - GITHUB_TOKEN is the bystander we care about — it must survive
#     unmangled when one of its sibling keys carries `\` or `&`.
PATHOLOGICAL_JSON='{"GITHUB_TOKEN":"ghs_x","PAYLOAD":"{\"text\":\"a & b\"}"}'

# Mirror the SOH delimiter and template shape used by the real runners.
DELIM=$(printf '\1')
TEMPLATE_BODY="\$bench_env_raw = '{{BENCH_ENV_JSON}}';"

echo "============================================"
echo "sed_escape_replacement smoke test"
echo "============================================"
echo "Input JSON:"
echo "  $PATHOLOGICAL_JSON"
echo ""

# --- Stage 1: confirm the bug is reproducible without the helper ---------
RAW_OUT=$(printf '%s' "$TEMPLATE_BODY" \
    | sed -e "s${DELIM}{{BENCH_ENV_JSON}}${DELIM}${PATHOLOGICAL_JSON}${DELIM}g")

echo "Unescaped sed output (regression baseline):"
echo "  $RAW_OUT"
echo ""

# Both corruption shapes must show up — `\"` collapses to `"`, and `&`
# gets replaced with the placeholder text. If either invariant changes
# (e.g. sed semantics shift, or the test template stops triggering the
# bug), the after-fix check below would also pass vacuously, so we gate
# on it explicitly here.
if ! printf '%s' "$RAW_OUT" | grep -q '{{BENCH_ENV_JSON}}'; then
    echo "ERROR: expected unescaped sed output to substitute '&' with the matched"
    echo "       placeholder ({{BENCH_ENV_JSON}}). Got:" >&2
    echo "       $RAW_OUT" >&2
    exit 1
fi
if printf '%s' "$RAW_OUT" | grep -q '\\"text\\"'; then
    echo "ERROR: expected unescaped sed output to drop backslashes from \\\"" >&2
    echo "       Got: $RAW_OUT" >&2
    exit 1
fi

# --- Stage 2: confirm the helper produces a clean round-trip --------------
sed_escape_replacement() {
    printf '%s' "$1" | sed -e 's/[\&]/\\&/g'
}

ESC=$(sed_escape_replacement "$PATHOLOGICAL_JSON")
FIXED_OUT=$(printf '%s' "$TEMPLATE_BODY" \
    | sed -e "s${DELIM}{{BENCH_ENV_JSON}}${DELIM}${ESC}${DELIM}g")

EXPECTED="\$bench_env_raw = '${PATHOLOGICAL_JSON}';"

echo "Escaped sed output:"
echo "  $FIXED_OUT"
echo ""

if [ "$FIXED_OUT" != "$EXPECTED" ]; then
    echo "ERROR: escaped substitution did not round-trip JSON value." >&2
    echo "  expected: $EXPECTED" >&2
    echo "  actual:   $FIXED_OUT" >&2
    exit 1
fi

# --- Stage 3: confirm both real runners still ship the helper -------------
# Guards against a future refactor that drops the helper while leaving
# the placeholder substitution in place — which would silently
# reintroduce the bug.
EXTENSION_PATH="$(cd "$SCRIPT_DIR/../.." && pwd)"
for runner in \
    "$EXTENSION_PATH/scripts/bench/bench-runner-playground.sh" \
    "$EXTENSION_PATH/scripts/test/test-runner-playground.sh"
do
    if [ ! -f "$runner" ]; then
        echo "ERROR: runner not found at $runner" >&2
        exit 1
    fi
    if ! grep -q 'sed_escape_replacement' "$runner"; then
        echo "ERROR: $runner no longer calls sed_escape_replacement." >&2
        echo "       JSON values substituted into the PHP template will be" >&2
        echo "       silently corrupted whenever they contain '\\' or '&'." >&2
        exit 1
    fi
done

echo "============================================"
echo "✓ sed_escape_replacement smoke test PASSED"
echo "  - Unescaped substitution corrupts JSON (regression baseline holds)."
echo "  - Escaped substitution round-trips '\\' and '&' losslessly."
echo "  - bench-runner-playground.sh and test-runner-playground.sh still"
echo "    invoke the helper."
echo "============================================"
