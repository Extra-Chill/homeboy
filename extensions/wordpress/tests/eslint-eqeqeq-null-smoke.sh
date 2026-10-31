#!/usr/bin/env bash
#
# Smoke test: confirm the wordpress extension's ESLint config exempts
# `== null` / `!= null` from the eqeqeq rule while still catching loose
# equality against non-null operands.
#
# Why this test exists (issue #459):
#
# `== null` matches both `null` and `undefined` in a single comparison and
# is the idiomatic JavaScript pattern for an early-return guard. Replacing
# it with `=== null` silently regresses behaviour because
# `undefined === null` is `false`. The eslint `eqeqeq` rule has a built-in
# `{ null: 'ignore' }` option for this exact case, but
# `@wordpress/eslint-plugin`'s recommended preset does NOT enable it
# (configs/es5.js sets `eqeqeq: 'error'` only). The extension's own
# `eslint.config.mjs` therefore overrides the rule explicitly. This smoke
# guards against future config edits dropping the null exemption.
#
# Reference: https://eslint.org/docs/latest/rules/eqeqeq#allow-null

set -euo pipefail

EXTENSION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ESLINT_BIN="$EXTENSION_DIR/node_modules/.bin/eslint"
ESLINT_CONFIG="$EXTENSION_DIR/eslint.config.mjs"

if [ ! -x "$ESLINT_BIN" ]; then
    echo "Skipping: $ESLINT_BIN not found (run \`npm ci\` in $EXTENSION_DIR)" >&2
    exit 0
fi

if [ ! -f "$ESLINT_CONFIG" ]; then
    echo "Missing eslint config: $ESLINT_CONFIG" >&2
    exit 1
fi

TMP_DIR="$(mktemp -d "${EXTENSION_DIR}/eslint-smoke.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

SAMPLE="$TMP_DIR/sample.js"
cat > "$SAMPLE" <<'JS'
// Idiomatic null-or-undefined guard; must NOT be flagged.
export const isSameId = ( a, b ) => {
	if ( a == null || b == null ) {
		return false;
	}
	return String( a ) === String( b );
};

// Loose equality against non-null; MUST still be flagged.
export const looseEquals = ( a, b ) => a == b;

// Loose inequality against non-null; MUST still be flagged.
export const looseNotEquals = ( a, b ) => a != b;
JS

OUTPUT="$TMP_DIR/eslint.out"
set +e
( cd "$EXTENSION_DIR" && \
    "$ESLINT_BIN" \
        --config "$ESLINT_CONFIG" \
        --rule '{"import/no-unresolved": "off", "import/named": "off", "import/default": "off"}' \
        "$SAMPLE" \
) > "$OUTPUT" 2>&1
status=$?
set -e

# We expect exactly the two non-null violations to remain. ESLint exits
# non-zero on findings, which is fine here -- we assert on the report shape.
if grep -E "eqeqeq" "$OUTPUT" | grep -E ":3:|:4:" >/dev/null; then
    echo "FAIL: \`== null\` was flagged by eqeqeq -- null exemption missing." >&2
    cat "$OUTPUT" >&2
    exit 1
fi

if ! grep -E "eqeqeq" "$OUTPUT" | grep -E "Expected '==='" >/dev/null; then
    echo "FAIL: loose \`==\` against non-null was NOT flagged. Rule is too lax." >&2
    cat "$OUTPUT" >&2
    exit 1
fi

if ! grep -E "eqeqeq" "$OUTPUT" | grep -E "Expected '!=='" >/dev/null; then
    echo "FAIL: loose \`!=\` against non-null was NOT flagged. Rule is too lax." >&2
    cat "$OUTPUT" >&2
    exit 1
fi

violation_count=$(grep -cE "eqeqeq" "$OUTPUT" || true)
if [ "$violation_count" -ne 2 ]; then
    echo "FAIL: expected exactly 2 eqeqeq violations, got $violation_count." >&2
    cat "$OUTPUT" >&2
    exit 1
fi

echo "wordpress eslint eqeqeq null-exemption smoke passed (status=$status, violations=$violation_count)"
