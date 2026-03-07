#!/usr/bin/env php
<?php
/**
 * WP Alternatives Fixer
 *
 * Replaces PHP functions with their WordPress equivalents:
 *
 *   strip_tags(...)            → wp_strip_all_tags(...)
 *   unlink($f)                 → wp_delete_file($f)
 *
 * All fixes are real code changes — NO phpcs:ignore comments.
 *
 * Usage: php wp-alternatives-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ( $argc < 2 ) {
	echo "Usage: php wp-alternatives-fixer.php <path>\n";
	exit( 1 );
}

$path = $argv[1];

if ( ! file_exists( $path ) ) {
	echo "Error: Path not found: $path\n";
	exit( 1 );
}

$result = fixer_process_path( $path, 'process_file' );

if ( $result['total_fixes'] > 0 ) {
	echo "WP alternatives fixer: Fixed {$result['total_fixes']} violation(s) in {$result['files_fixed']} file(s)\n";
} else {
	echo "WP alternatives fixer: No fixable patterns found\n";
}

exit( 0 );

/**
 * Process a single PHP file.
 */
function process_file( $filepath ) {
	$source = file_get_contents( $filepath );
	if ( false === $source ) {
		return 0;
	}

	$lines   = explode( "\n", $source );
	$fixes   = 0;
	$changed = false;

	foreach ( $lines as $idx => &$line ) {
		// strip_tags(...) → wp_strip_all_tags(...)
		// Match standalone strip_tags calls, not inside wp_strip_all_tags already.
		if ( preg_match( '/\bstrip_tags\s*\(/', $line ) && false === strpos( $line, 'wp_strip_all_tags' ) ) {
			$line    = preg_replace( '/\bstrip_tags\s*\(\s*/', 'wp_strip_all_tags( ', $line );
			$fixes++;
			$changed = true;
			continue;
		}

		// unlink($f) → wp_delete_file($f)
		// Matches both standalone and conditional unlink calls.
		// Skip if already using wp_delete_file.
		if ( preg_match( '/\bunlink\s*\(/', $line ) && false === strpos( $line, 'wp_delete_file' ) ) {
			// Skip lines already suppressed with phpcs:ignore.
			if ( false !== strpos( $line, 'phpcs:ignore' ) ) {
				continue;
			}
			if ( $idx > 0 && false !== strpos( $lines[ $idx - 1 ], 'phpcs:ignore' ) ) {
				continue;
			}

			$line    = preg_replace( '/\bunlink\s*\(\s*(.+?)\s*\)/', 'wp_delete_file( $1 )', $line );
			$fixes++;
			$changed = true;
			continue;
		}
	}
	unset( $line );

	if ( $changed ) {
		file_put_contents( $filepath, implode( "\n", $lines ) );
	}

	return $fixes;
}
