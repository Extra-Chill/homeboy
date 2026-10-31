#!/usr/bin/env bash
#
# Smoke test: confirm the phpcs-ignore-fixer never injects a `phpcs:enable`
# (or `phpcs:disable`) comment INSIDE a multi-line SQL string literal.
#
# Why this test exists (issue #981):
#
# The fixer wraps known false-positive `WordPress.DB.PreparedSQL` violations
# (table names from `$wpdb->prefix`) in `// phpcs:disable` / `// phpcs:enable`
# blocks. The old statement-boundary finders were line-based regex heuristics
# that were NOT string-content-aware: `find_statement_end()` walked forward and
# returned the first line ending in `;`/`);`. Inside a multi-line SQL string
# there is no real terminator until the closing `);` of the whole
# `$wpdb->prepare()` call — so the `// phpcs:enable` marker was spliced INSIDE
# the SQL string literal. PHP does not treat `//` as a comment inside a string,
# so the literal text became part of the SQL, producing invalid runtime SQL
# that MariaDB/MySQL rejected with ERROR 1064. This silently broke
# extrachill-community draft-storage (downstream: extrachill-community#113,
# fixed there via PR #118). The fixer is now token-aware
# (`find_statement_boundaries_tokenized()`): it tokenizes the file and walks
# paren depth back to zero, skipping the contents of string-literal tokens, so
# the markers always land on their own PHP comment lines outside any string.
#
# This is the same class of bug as #878 and #458 (php-fixers operating on
# regex/line heuristics instead of the PHP token stream).
#
# The test:
#   1. Builds a temp PHP fixture with a multi-line `$wpdb->prepare("INSERT INTO
#      {$table} ... ON DUPLICATE KEY UPDATE ...;", ...)` (the exact shape that
#      broke draft-storage — note the interior `;` inside the string that the
#      old line-based finder mistook for the statement end).
#   2. Runs the fixer against it (real phpcs + WordPress standard from vendor).
#   3. Asserts:
#        a. The fixed file still passes `php -l`.
#        b. NO `phpcs:` text appears inside ANY string-literal token (the core
#           regression guard — verified via token_get_all, not a fragile grep).
#        c. The `WordPress.DB.PreparedSQL` violation is actually suppressed
#           (the disable/enable block did its job).

set -euo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXTENSION_DIR="$(cd "${TESTS_DIR}/.." && pwd)"
FIXER="${EXTENSION_DIR}/scripts/lint/php-fixers/phpcs-ignore-fixer.php"
PHPCS_BIN="${EXTENSION_DIR}/vendor/bin/phpcs"

if ! command -v php >/dev/null 2>&1; then
	echo "Skipping: php not found on PATH" >&2
	exit 0
fi

if [ ! -f "$FIXER" ]; then
	echo "Missing fixer: $FIXER" >&2
	exit 1
fi

if [ ! -x "$PHPCS_BIN" ]; then
	echo "Skipping: $PHPCS_BIN not found (run \`composer install\` in $EXTENSION_DIR)" >&2
	exit 0
fi

# Resolve the WordPress coding standard the same way lint-runner.sh does, so
# this test is self-contained regardless of the global phpcs config.
PHPCS_STANDARD_PATHS=()
for phpcs_standard_path in \
	"${EXTENSION_DIR}/vendor/wp-coding-standards/wpcs" \
	"${EXTENSION_DIR}/vendor/phpcsstandards/phpcsextra" \
	"${EXTENSION_DIR}/vendor/phpcsstandards/phpcsutils" \
	"${EXTENSION_DIR}/HomeboyWordPress"; do
	if [ -d "$phpcs_standard_path" ]; then
		PHPCS_STANDARD_PATHS+=("$phpcs_standard_path")
	fi
done

if [ ! -d "${EXTENSION_DIR}/vendor/wp-coding-standards/wpcs" ]; then
	echo "Skipping: WordPress coding standard not installed in vendor" >&2
	exit 0
fi

if [ "${#PHPCS_STANDARD_PATHS[@]}" -gt 0 ]; then
	PHPCS_INSTALLED_PATHS=$(IFS=','; printf '%s' "${PHPCS_STANDARD_PATHS[*]}")
	"$PHPCS_BIN" --config-set installed_paths "$PHPCS_INSTALLED_PATHS" --quiet >/dev/null 2>&1 || true
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

SAMPLE="$TMP_DIR/draft-storage-fixture.php"
cat > "$SAMPLE" <<'PHP'
<?php
function save_draft( $wpdb, $table, $row ) {
	$sql = $wpdb->prepare(
		"INSERT INTO {$table}
			(user_id, blog_id, type, title, content, updated_at)
		VALUES (%d, %d, %s, %s, %s, %d)
		ON DUPLICATE KEY UPDATE
			title = VALUES(title),
			content = VALUES(content),
			updated_at = VALUES(updated_at);",
		$row['user_id'],
		$row['blog_id'],
		$row['type'],
		$row['title'],
		$row['content'],
		$row['updated_at']
	);
	return $wpdb->query( $sql );
}
PHP

# Sanity: the fixture must parse before we run the fixer.
if ! php -l "$SAMPLE" >/dev/null 2>&1; then
	echo "FAIL: fixture does not parse before fixing (test bug)." >&2
	exit 1
fi

# Run the fixer.
php "$FIXER" "$SAMPLE" --phpcs-binary="$PHPCS_BIN" --phpcs-standard=WordPress >/dev/null 2>&1 || {
	echo "FAIL: fixer exited non-zero." >&2
	exit 1
}

# Assertion (a): the fixed file still parses.
if ! php -l "$SAMPLE" >/dev/null 2>&1; then
	echo "FAIL: fixed file no longer passes php -l." >&2
	php -l "$SAMPLE" >&2 || true
	cat "$SAMPLE" >&2
	exit 1
fi

# Assertion (b): NO phpcs: marker lives inside any string-literal token. This is
# the core regression guard for #981. We use token_get_all so the check is not
# fooled by `//` appearing inside SQL text.
INSIDE_STRING=$(php -r '
$src = file_get_contents( $argv[1] );
$tokens = token_get_all( $src );
$found = 0;
foreach ( $tokens as $t ) {
	if ( is_array( $t ) && in_array( $t[0], array( T_CONSTANT_ENCAPSED_STRING, T_ENCAPSED_AND_WHITESPACE, T_INLINE_HTML ), true ) ) {
		if ( strpos( $t[1], "phpcs:" ) !== false ) {
			$found++;
		}
	}
}
echo $found;
' "$SAMPLE")

if [ "$INSIDE_STRING" -ne 0 ]; then
	echo "FAIL: found $INSIDE_STRING phpcs: marker(s) inside a string literal (issue #981 regression)." >&2
	cat "$SAMPLE" >&2
	exit 1
fi

# Assertion (c): the disable/enable block actually suppressed the violation.
REMAINING=$("$PHPCS_BIN" --report=json --sniffs=WordPress.DB.PreparedSQL --standard=WordPress "$SAMPLE" 2>/dev/null | php -r '
$d = json_decode( file_get_contents( "php://stdin" ), true );
$n = 0;
if ( ! empty( $d["files"] ) ) {
	foreach ( $d["files"] as $f ) {
		$n += count( $f["messages"] ?? array() );
	}
}
echo $n;
')

if [ "$REMAINING" -ne 0 ]; then
	echo "FAIL: expected 0 WordPress.DB.PreparedSQL violations after fix, got $REMAINING." >&2
	cat "$SAMPLE" >&2
	exit 1
fi

echo "phpcs-ignore-fixer multiline-sql boundary smoke passed (no marker inside string, php -l clean, violation suppressed)"
