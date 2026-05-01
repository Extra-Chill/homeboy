#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHPSTAN_RUNNER="${ROOT_DIR}/scripts/lint/phpstan-runner.sh"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

COMPONENT_DIR="${TMPDIR}/component"
OUTPUT_FILE="${TMPDIR}/phpstan-output.txt"

mkdir -p "${COMPONENT_DIR}/inc"

cat > "${COMPONENT_DIR}/inc/issue-375.php" <<'PHP'
<?php

function homeboy_issue_375_filter_callback( $value, WP_Post $post ) {
	return $value;
}

function homeboy_issue_375_action_callback( WP_Post $post, array $context ): void {
	new WP_Post( (object) array( 'ID' => 123 ) );

	WP_CLI::log( 'Inspecting ' . $post->post_type );
	WP_CLI::success( 'Post excerpt: ' . $post->post_excerpt );
	WP_CLI::warning( 'Modified at ' . $post->post_modified_gmt );

	apply_filters( 'homeboy_issue_375_filter', $post->post_title, $post, array( 'source' => 'smoke' ) );
	add_filter( 'homeboy_issue_375_filter', 'homeboy_issue_375_filter_callback', 10, 2 );
	add_action( 'homeboy_issue_375_action', 'homeboy_issue_375_action_callback', 10, 2 );
	remove_filter( 'homeboy_issue_375_filter', 'homeboy_issue_375_filter_callback', 10 );

	current_user_can( 'edit_post', $post->ID );
	wp_json_encode( array( 'post_type' => $post->post_type ) );
	get_post();
}

// Regression guard for issue #393 — host-smoke files in the same component
// must not shadow real WP signatures. Calling get_post_types() with the
// canonical 2-arg shape `(array, string)` should pass PHPStan even when a
// `function_exists`-guarded shim with a narrower arity sits beside it.
function homeboy_issue_393_caller(): array {
	$post_types = get_post_types(
		array(
			'public'              => true,
			'exclude_from_search' => false,
		),
		'names'
	);
	return array_values( $post_types );
}
PHP

# Fake host-smoke stub that historically shadowed wordpress-stubs.php through
# `bootstrapFiles:` precedence and produced false `arguments.count` errors.
# The runtime config now pulls wordpress-stubs into `scanFiles:` so the real
# 3-arg WP signature wins regardless of this redefinition. See issue #393.
mkdir -p "${COMPONENT_DIR}/tests"
cat > "${COMPONENT_DIR}/tests/issue-393-host-smoke.php" <<'PHP'
<?php

if ( ! function_exists( 'get_post_types' ) ) {
	function get_post_types( $args = array() ) {
		return array();
	}
}
PHP

if [ ! -x "${ROOT_DIR}/vendor/bin/phpstan" ]; then
	echo "FAIL: PHPStan is not installed. Run composer install in ${ROOT_DIR}." >&2
	exit 1
fi

set +e
HOMEBOY_EXTENSION_PATH="$ROOT_DIR" \
HOMEBOY_COMPONENT_PATH="$COMPONENT_DIR" \
HOMEBOY_COMPONENT_ID="phpstan-wordpress-api-stubs" \
HOMEBOY_SUMMARY_MODE=1 \
HOMEBOY_PHPSTAN_THREADS=1 \
"$PHPSTAN_RUNNER" >"$OUTPUT_FILE" 2>&1
exit_code=$?
set -e

if [ "$exit_code" -ne 0 ]; then
	echo "FAIL: representative WordPress/WP-CLI API calls should pass PHPStan" >&2
	cat "$OUTPUT_FILE" >&2
	exit 1
fi

if grep -E 'staticMethod\.resultUnused|arguments\.count|property\.notFound' "$OUTPUT_FILE" >/dev/null; then
	echo "FAIL: representative false-positive identifiers are still present" >&2
	cat "$OUTPUT_FILE" >&2
	exit 1
fi

echo "PHPStan WordPress API stubs smoke passed"
