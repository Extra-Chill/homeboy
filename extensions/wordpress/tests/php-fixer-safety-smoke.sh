#!/usr/bin/env bash
# Regression coverage for conservative custom PHP fixer behavior (#2557).
set -euo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${TESTS_DIR}/.." && pwd)"
FIXERS_DIR="${EXTENSION_DIR}/scripts/lint/php-fixers"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

assert_contains() {
	if ! grep -qE "$1" "$2"; then
		fail "$3"
	fi
}

assert_not_contains() {
	if grep -qE "$1" "$2"; then
		fail "$3"
	fi
}

cat > "${TMP_DIR}/alternatives.php" <<'PHP'
<?php
function normalize_legacy( $text ) {
	if ( function_exists( 'wp_strip_all_tags' ) ) {
		return wp_strip_all_tags( $text );
	}

	return strip_tags( $text );
}

function normalize_current( $text ) {
	return strip_tags( $text );
}
PHP

php "${FIXERS_DIR}/wp-alternatives-fixer.php" "${TMP_DIR}/alternatives.php" >/dev/null
assert_contains 'return strip_tags\( \$text \);' "${TMP_DIR}/alternatives.php" "availability-guarded fallback must remain strip_tags"
assert_contains 'return wp_strip_all_tags\( \$text \);' "${TMP_DIR}/alternatives.php" "unguarded strip_tags call must still be fixed"

cat > "${TMP_DIR}/catches.php" <<'PHP'
<?php
try {
	throw new SuppressedException();
} catch ( SuppressedException ) { // phpcs:ignore Generic.CodeAnalysis.EmptyStatement.DetectedCatch
}

try {
	throw new FixedException();
} catch ( FixedException ) {
}
PHP

php "${FIXERS_DIR}/empty-catch-fixer.php" "${TMP_DIR}/catches.php" >/dev/null
assert_contains 'SuppressedException.*phpcs:ignore' "${TMP_DIR}/catches.php" "suppressed catch must remain intact"
assert_not_contains 'SuppressedException \$e' "${TMP_DIR}/catches.php" "suppressed catch must not gain a variable"
assert_contains 'unset\( \$e \);' "${TMP_DIR}/catches.php" "unsuppressed empty catch must still be fixed"

cat > "${TMP_DIR}/phpcs" <<'SH'
#!/usr/bin/env bash
case "$5" in
*private.php)
	printf '%s\n' '{"files":{"'"$5"'":{"messages":[{"message":"The method parameter $unused is never used","line":3,"column":1,"source":"Generic.CodeAnalysis.UnusedFunctionParameter"}]}}}'
	;;
*)
	printf '%s\n' '{"files":{"'"$5"'":{"messages":[{"message":"The function parameter $context is never used","line":2,"column":1,"source":"Generic.CodeAnalysis.UnusedFunctionParameter"}]}}}'
	;;
esac
SH
chmod +x "${TMP_DIR}/phpcs"
touch "${TMP_DIR}/phpcs.xml"

cat > "${TMP_DIR}/public.php" <<'PHP'
<?php
function public_callback( $value, $context = null ) {
	return $value;
}

public_callback( 'value', 'live context' );
PHP

php "${FIXERS_DIR}/unused-param-fixer.php" "${TMP_DIR}/public.php" --phpcs-binary="${TMP_DIR}/phpcs" --phpcs-standard="${TMP_DIR}/phpcs.xml" >/dev/null
assert_contains 'function public_callback\( \$value, \$context = null \)' "${TMP_DIR}/public.php" "global callable signature must be preserved"
assert_contains "public_callback\( 'value', 'live context' \);" "${TMP_DIR}/public.php" "live two-argument caller must be preserved"
assert_contains 'unset\( \$context \);' "${TMP_DIR}/public.php" "global callable must receive a noop reference"

cat > "${TMP_DIR}/private.php" <<'PHP'
<?php
class Example {
	private function format( $value, $unused ) {
		return $value;
	}

	public function run() {
		return $this->format( 'value', 'unused' );
	}
}
PHP

php "${FIXERS_DIR}/unused-param-fixer.php" "${TMP_DIR}/private.php" --phpcs-binary="${TMP_DIR}/phpcs" --phpcs-standard="${TMP_DIR}/phpcs.xml" >/dev/null
assert_contains 'private function format\( \$value[[:space:]]*\)' "${TMP_DIR}/private.php" "private method signature must still be safely trimmed"
assert_not_contains "format\( 'value'," "${TMP_DIR}/private.php" "private method caller must be updated with the signature"

echo "php fixer safety smoke passed"
