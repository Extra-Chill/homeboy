#!/usr/bin/env php
<?php
/**
 * Silenced Error Fixer
 *
 * Replaces error-suppressed function calls (@func) with explicit guard patterns:
 *   @unlink($f)           → if ( file_exists( $f ) ) { unlink( $f ); }
 *   @file($f, ...)        → is_readable($f) check + file() call
 *   @file_get_contents(…) → is_readable check + file_get_contents() call
 *
 * Does NOT add phpcs:ignore — produces real code changes.
 *
 * Usage: php silenced-error-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ( $argc < 2 ) {
	echo "Usage: php silenced-error-fixer.php <path>\n";
	exit( 1 );
}

$path = $argv[1];

if ( ! file_exists( $path ) ) {
	echo "Error: Path not found: $path\n";
	exit( 1 );
}

$result = fixer_process_path( $path, 'process_file' );

if ( $result['total_fixes'] > 0 ) {
	echo "Silenced error fixer: Fixed {$result['total_fixes']} silenced call(s) in {$result['files_fixed']} file(s)\n";
} else {
	echo "Silenced error fixer: No fixable patterns found\n";
}

exit( 0 );

/**
 * Process a single PHP file.
 *
 * Uses token-based scanning to find @ operators followed by function calls,
 * then rewrites the containing statement with an explicit guard.
 */
function process_file( $filepath ) {
	$source = file_get_contents( $filepath );
	if ( $source === false ) {
		return 0;
	}

	$tokens = token_get_all( $source );
	$count  = count( $tokens );
	$fixes  = 0;

	// Work line-based for simpler replacement.
	$lines   = explode( "\n", $source );
	$changed = false;

	foreach ( $lines as $idx => &$line ) {
		// Match @unlink( ... ) — with or without WP spacing.
		if ( preg_match( '/^(\s*)@unlink\s*\(\s*(.+?)\s*\)\s*;/', $line, $m ) ) {
			$indent = $m[1];
			$arg    = $m[2];

			// Check if there's already a file_exists guard nearby (within 3 lines above).
			$has_guard = false;
			for ( $look = max( 0, $idx - 3 ); $look < $idx; $look++ ) {
				if ( strpos( $lines[ $look ], 'file_exists' ) !== false ) {
					$has_guard = true;
					break;
				}
			}

			if ( $has_guard ) {
				// Already guarded — just remove the @ operator.
				$line    = preg_replace( '/@unlink/', 'unlink', $line, 1 );
				$fixes++;
				$changed = true;
			} else {
				// Check for phpcs:ignore on the preceding line.
				$phpcs_preceding = '';
				if ( $idx > 0 && preg_match( '/^\s*(\/\/\s*phpcs:ignore\s+.*)$/', $lines[ $idx - 1 ], $pm ) ) {
					$phpcs_preceding = $pm[1];
					// Remove the preceding comment line — we'll move it inside the guard.
					unset( $lines[ $idx - 1 ] );
				}

				// Also check for phpcs:ignore inline on this line.
				$phpcs_inline = '';
				if ( preg_match( '/(\/\/\s*phpcs:ignore\s+.*)$/', $line, $cm ) ) {
					$phpcs_inline = ' ' . $cm[1];
				}

				$phpcs_comment = $phpcs_inline;
				$phpcs_prefix  = '';
				if ( $phpcs_preceding !== '' ) {
					$phpcs_prefix = "{$indent}\t{$phpcs_preceding}\n";
				}

				$line    = "{$indent}if ( file_exists( {$arg} ) ) {";
				$line   .= "\n{$phpcs_prefix}{$indent}\tunlink( {$arg} );{$phpcs_comment}";
				$line   .= "\n{$indent}}";
				$fixes++;
				$changed = true;
			}
			continue;
		}

		// Match @file( ... ) — the file() function for reading lines.
		if ( preg_match( '/^(\s*)(\$\w+)\s*=\s*@file\s*\(\s*(.+?)\s*\)\s*;/', $line, $m ) ) {
			$indent  = $m[1];
			$var     = $m[2];
			$args    = $m[3];

			// Extract the file path argument (first arg before comma or end).
			$first_arg = $args;
			if ( strpos( $args, ',' ) !== false ) {
				$first_arg = trim( substr( $args, 0, strpos( $args, ',' ) ) );
			}

			// Replace with is_readable guard.
			$line    = "{$indent}if ( is_readable( {$first_arg} ) ) {";
			$line   .= "\n{$indent}\t{$var} = file( {$args} );";
			$line   .= "\n{$indent}} else {";
			$line   .= "\n{$indent}\t{$var} = false;";
			$line   .= "\n{$indent}}";
			$fixes++;
			$changed = true;
			continue;
		}

		// Match @file_get_contents( ... )
		if ( preg_match( '/^(\s*)(\$\w+)\s*=\s*@file_get_contents\s*\(\s*(.+?)\s*\)\s*;/', $line, $m ) ) {
			$indent  = $m[1];
			$var     = $m[2];
			$args    = $m[3];

			$first_arg = $args;
			if ( strpos( $args, ',' ) !== false ) {
				$first_arg = trim( substr( $args, 0, strpos( $args, ',' ) ) );
			}

			$line    = "{$indent}if ( is_readable( {$first_arg} ) ) {";
			$line   .= "\n{$indent}\t{$var} = file_get_contents( {$args} );";
			$line   .= "\n{$indent}} else {";
			$line   .= "\n{$indent}\t{$var} = false;";
			$line   .= "\n{$indent}}";
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
