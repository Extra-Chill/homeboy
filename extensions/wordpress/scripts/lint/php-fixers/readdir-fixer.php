#!/usr/bin/env php
<?php
/**
 * Readdir Assignment-in-Condition Fixer
 *
 * Replaces `while (false !== ($entry = readdir($handle)))` patterns with
 * `scandir()` + `array_diff()` to eliminate assignment-in-condition violations.
 *
 * The readdir pattern is a PHP classic but PHPCS flags it under
 * WordPress.CodeAnalysis.AssignmentInCondition. The scandir replacement is
 * functionally equivalent and avoids the opendir/readdir/closedir ceremony.
 *
 * This fixer handles the full opendir/while-readdir/closedir block as a unit.
 *
 * Usage: php readdir-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ( $argc < 2 ) {
	echo "Usage: php readdir-fixer.php <path>\n";
	exit( 1 );
}

$path = $argv[1];

if ( ! file_exists( $path ) ) {
	echo "Error: Path not found: $path\n";
	exit( 1 );
}

$result = fixer_process_path( $path, 'process_file' );

if ( $result['total_fixes'] > 0 ) {
	echo "Readdir fixer: Fixed {$result['total_fixes']} readdir loop(s) in {$result['files_fixed']} file(s)\n";
} else {
	echo "Readdir fixer: No fixable patterns found\n";
}

exit( 0 );

/**
 * Process a single PHP file.
 *
 * Finds opendir/readdir/closedir blocks and replaces them with scandir-based loops.
 * The transformation preserves the loop body exactly, just replaces the iteration
 * mechanism and removes the handle management.
 */
function process_file( $filepath ) {
	$lines = file( $filepath );
	if ( $lines === false ) {
		return 0;
	}

	$fixes     = 0;
	$new_lines = array();
	$count     = count( $lines );
	$i         = 0;

	while ( $i < $count ) {
		$line    = $lines[ $i ];
		$trimmed = trim( $line );

		// Look for: while ( false !== ( $entry = readdir( $handle ) ) )
		// The handle variable tells us which opendir block this belongs to.
		if ( preg_match( '/while\s*\(\s*false\s*!==\s*\(\s*(\$\w+)\s*=\s*readdir\s*\(\s*(\$\w+)\s*\)\s*\)\s*\)/', $trimmed, $m ) ) {
			$entry_var  = $m[1];
			$handle_var = $m[2];

			// Determine the indent from the while line.
			preg_match( '/^(\s*)/', $line, $indent_match );
			$while_indent = $indent_match[1];

			// Look backwards to find the opendir and its guard.
			$opendir_info = find_opendir_block( $new_lines, $handle_var );

			// Find the directory variable from the opendir call.
			$dir_var = $opendir_info['dir_var'] ?? null;

			if ( $dir_var === null ) {
				// Can't determine directory — skip this instance.
				$new_lines[] = $line;
				$i++;
				continue;
			}

			// Remove the opendir line and its guard from already-emitted output.
			if ( ! empty( $opendir_info['remove_indices'] ) ) {
				$indices_to_remove = $opendir_info['remove_indices'];
				$filtered          = array();
				foreach ( $new_lines as $idx => $emitted ) {
					if ( ! in_array( $idx, $indices_to_remove, true ) ) {
						$filtered[] = $emitted;
					}
				}
				$new_lines = $filtered;
			}

			// Collect the while loop body.
			$i++; // Move past the while line.

			// The opening brace might be on the while line or the next line.
			$brace_on_while = ( substr_count( $line, '{' ) > 0 );
			$brace_depth    = $brace_on_while ? 1 : 0;

			if ( ! $brace_on_while && $i < $count ) {
				// Next line should be the opening brace.
				$brace_line = trim( $lines[ $i ] );
				if ( $brace_line === '{' ) {
					$brace_depth = 1;
					$i++;
				}
			}

			$body_lines = array();
			while ( $i < $count && $brace_depth > 0 ) {
				$inner        = $lines[ $i ];
				$brace_depth += substr_count( $inner, '{' ) - substr_count( $inner, '}' );
				if ( $brace_depth > 0 ) {
					$body_lines[] = $inner;
				}
				$i++;
			}

			// Look ahead for closedir( $handle ) and remove it.
			$closedir_removed = false;
			$lookahead        = $i;
			while ( $lookahead < $count && $lookahead < $i + 3 ) {
				$peek = trim( $lines[ $lookahead ] );
				if ( $peek === '' ) {
					$lookahead++;
					continue;
				}
				if ( preg_match( '/closedir\s*\(\s*' . preg_quote( $handle_var, '/' ) . '\s*\)\s*;/', $peek ) ) {
					// Skip this line (and any blank line before it).
					$closedir_removed = true;
					$lookahead++;
					$i = $lookahead;
					break;
				}
				break;
			}

			// Filter out the . and .. skip from the body (scandir already excludes them via array_diff).
			$filtered_body = filter_dot_skip( $body_lines, $entry_var );

			// Emit the scandir-based foreach loop.
			$scandir_expr = "scandir( {$dir_var} )";
			$new_lines[]  = "{$while_indent}foreach ( array_diff( {$scandir_expr}, array( '.', '..' ) ) as {$entry_var} ) {\n";

			foreach ( $filtered_body as $body_line ) {
				$new_lines[] = $body_line;
			}

			$new_lines[] = "{$while_indent}}\n";

			$fixes++;
			continue;
		}

		$new_lines[] = $line;
		$i++;
	}

	if ( $fixes > 0 ) {
		// Collapse consecutive blank lines left by block removal.
		$cleaned = array();
		$prev_blank = false;
		foreach ( $new_lines as $nl ) {
			$is_blank = ( trim( $nl ) === '' );
			if ( $is_blank && $prev_blank ) {
				continue;
			}
			$cleaned[]  = $nl;
			$prev_blank = $is_blank;
		}
		file_put_contents( $filepath, implode( '', $cleaned ) );
	}

	return $fixes;
}

/**
 * Search backwards through already-emitted lines to find the opendir() call
 * and its guard block for the given handle variable.
 *
 * Returns: ['dir_var' => string, 'remove_indices' => int[]]
 */
function find_opendir_block( array $emitted_lines, string $handle_var ): array {
	$result = array(
		'dir_var'        => null,
		'remove_indices' => array(),
	);

	$count = count( $emitted_lines );

	// Search backwards for the opendir line.
	for ( $i = $count - 1; $i >= 0 && $i >= $count - 15; $i-- ) {
		$line = $emitted_lines[ $i ];

		// Match: $handle = opendir( $dir )
		if ( preg_match( '/' . preg_quote( $handle_var, '/' ) . '\s*=\s*opendir\s*\(\s*(.+?)\s*\)\s*;/', $line, $m ) ) {
			$result['dir_var']          = $m[1];
			$result['remove_indices'][] = $i;

			// Look for the guard: if ( ! $handle ) { continue; } or { return ...; }
			// It's typically the next 2-3 lines after opendir in the emitted output.
			for ( $j = $i + 1; $j < $count && $j <= $i + 5; $j++ ) {
				$guard_line = trim( $emitted_lines[ $j ] );
				if ( $guard_line === '' ) {
					$result['remove_indices'][] = $j;
					continue;
				}
				// Guard block: if ( ! $handle ) { ... }
				if ( preg_match( '/if\s*\(\s*!\s*' . preg_quote( $handle_var, '/' ) . '\s*\)/', $guard_line ) ) {
					$result['remove_indices'][] = $j;
					// Collect the rest of the guard block.
					$guard_depth = substr_count( $emitted_lines[ $j ], '{' ) - substr_count( $emitted_lines[ $j ], '}' );
					$k           = $j + 1;
					while ( $k < $count && $guard_depth > 0 ) {
						$guard_depth += substr_count( $emitted_lines[ $k ], '{' ) - substr_count( $emitted_lines[ $k ], '}' );
						$result['remove_indices'][] = $k;
						$k++;
					}
					break;
				}
				break;
			}

			// Also remove blank line before opendir if present.
			if ( $i > 0 && trim( $emitted_lines[ $i - 1 ] ) === '' ) {
				// Don't remove blank lines — they might be meaningful.
			}

			break;
		}
	}

	return $result;
}

/**
 * Filter out the dot-entry skip from loop body lines.
 *
 * The readdir pattern always has:
 *   if ( '.' === $entry || '..' === $entry ) { continue; }
 * or similar. Since scandir + array_diff already excludes these, remove it.
 *
 * Also handles the 'index.php' skip that sometimes follows on the same line.
 */
function filter_dot_skip( array $body_lines, string $entry_var ): array {
	$filtered    = array();
	$skip_count  = 0;
	$i           = 0;
	$count       = count( $body_lines );
	$entry_esc   = preg_quote( $entry_var, '/' );

	while ( $i < $count ) {
		$line    = $body_lines[ $i ];
		$trimmed = trim( $line );

		// Match: if ( '.' === $entry || '..' === $entry ) { continue; }
		// Also with optional additional conditions like || 'index.php' === $entry
		if ( preg_match( '/^if\s*\(.*[\'"]\.[\'"]\s*===\s*' . $entry_esc . '.*[\'"]\.\.[\'"]\s*===\s*' . $entry_esc . '/', $trimmed ) ||
			preg_match( '/^if\s*\(.*' . $entry_esc . '\s*===\s*[\'"]\.[\'"].*' . $entry_esc . '\s*===\s*[\'"]\.\./', $trimmed ) ) {

			// Check if the entire if-continue is on one line.
			if ( preg_match( '/continue\s*;\s*\}/', $trimmed ) ) {
				// Single-line if-continue — but check for extra conditions.
				if ( preg_match( '/index\.php/', $trimmed ) ) {
					// Has index.php skip too — rewrite to just check index.php.
					preg_match( '/^(\s*)/', $line, $indent_m );
					$indent = $indent_m[1];
					$filtered[] = "{$indent}if ( 'index.php' === {$entry_var} ) {\n";
					$filtered[] = "{$indent}\tcontinue;\n";
					$filtered[] = "{$indent}}\n";
					$filtered[] = "\n";
				}
				// Otherwise, just skip the line entirely.
				$i++;

				// Skip blank line after if present.
				if ( $i < $count && trim( $body_lines[ $i ] ) === '' ) {
					$i++;
				}
				continue;
			}

			// Multi-line if block — collect it to check for extra conditions.
			$if_block = $line;
			$depth    = substr_count( $line, '{' ) - substr_count( $line, '}' );
			$i++;
			while ( $i < $count && $depth > 0 ) {
				$if_block .= $body_lines[ $i ];
				$depth    += substr_count( $body_lines[ $i ], '{' ) - substr_count( $body_lines[ $i ], '}' );
				$i++;
			}

			// Check if the block also skips 'index.php' — preserve that check.
			if ( preg_match( '/index\.php/', $if_block ) ) {
				preg_match( '/^(\s*)/', $line, $indent_m );
				$indent     = $indent_m[1];
				$filtered[] = "{$indent}if ( 'index.php' === {$entry_var} ) {\n";
				$filtered[] = "{$indent}\tcontinue;\n";
				$filtered[] = "{$indent}}\n";
				$filtered[] = "\n";
			}

			// Skip blank line after.
			if ( $i < $count && trim( $body_lines[ $i ] ) === '' ) {
				$i++;
			}
			continue;
		}

		$filtered[] = $line;
		$i++;
	}

	return $filtered;
}
