#!/usr/bin/env php
<?php
/**
 * WP Filesystem Fixer
 *
 * Replaces raw PHP filesystem functions with WP_Filesystem equivalents:
 *
 *   file_get_contents($path)                    → $wp_filesystem->get_contents($path)
 *   file_put_contents($path, $content)          → $wp_filesystem->put_contents($path, $content)
 *   file_put_contents($path, $content, FILE_APPEND) → read-concat-write via $wp_filesystem
 *   is_writable($path)                          → $wp_filesystem->is_writable($path)
 *
 * The fixer uses a WP_Filesystem helper variable ($fs) initialized at the top of each
 * affected method. If the file's namespace contains 'DataMachine\Core\FilesRepository',
 * it uses the existing FilesystemHelper::get() pattern. Otherwise it uses the global
 * $wp_filesystem directly.
 *
 * All fixes are real code changes — NO phpcs:ignore comments.
 *
 * Usage: php wp-filesystem-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ( $argc < 2 ) {
	echo "Usage: php wp-filesystem-fixer.php <path>\n";
	exit( 1 );
}

$path = $argv[1];

if ( ! file_exists( $path ) ) {
	echo "Error: Path not found: $path\n";
	exit( 1 );
}

$result = fixer_process_path( $path, 'process_file' );

if ( $result['total_fixes'] > 0 ) {
	echo "WP Filesystem fixer: Fixed {$result['total_fixes']} call(s) in {$result['files_fixed']} file(s)\n";
} else {
	echo "WP Filesystem fixer: No fixable patterns found\n";
}

exit( 0 );

/**
 * Process a single PHP file.
 *
 * Strategy:
 * 1. Scan for file_get_contents/file_put_contents/is_writable calls.
 * 2. For each call, determine the enclosing method.
 * 3. Track which methods need a $fs variable injection.
 * 4. Replace calls and inject $fs initialization.
 */
function process_file( $filepath ) {
	$source = file_get_contents( $filepath );
	if ( false === $source ) {
		return 0;
	}

	// Skip the fixer files themselves.
	if ( false !== strpos( $filepath, 'php-fixers' ) ) {
		return 0;
	}

	$lines      = explode( "\n", $source );
	$fixes      = 0;
	$changed    = false;
	$has_helper = false;

	// Detect if this file has access to FilesystemHelper.
	$namespace = '';
	foreach ( $lines as $line ) {
		if ( preg_match( '/^\s*namespace\s+([\w\\\\]+)/', $line, $m ) ) {
			$namespace = $m[1];
			break;
		}
	}

	// Check for existing FilesystemHelper use/import, or same namespace.
	if ( false !== strpos( $namespace, 'DataMachine\\Core\\FilesRepository' ) ) {
		$has_helper = true;
	} else {
		foreach ( $lines as $line ) {
			if ( false !== strpos( $line, 'FilesystemHelper' ) ) {
				$has_helper = true;
				break;
			}
		}
	}

	// Track which method bodies need $fs injection.
	// Key: line number of method opening brace, Value: indent string.
	$methods_needing_fs = array();

	// First pass: identify lines that need fixing and their enclosing methods.
	$target_lines = array();
	foreach ( $lines as $idx => $line ) {
		$trimmed = ltrim( $line );

		// Skip comment lines (docblocks, single-line comments).
		if ( str_starts_with( $trimmed, '*' ) || str_starts_with( $trimmed, '//' ) || str_starts_with( $trimmed, '/*' ) || str_starts_with( $trimmed, '#' ) ) {
			continue;
		}

		// Skip lines already suppressed.
		if ( false !== strpos( $line, 'phpcs:ignore' ) ) {
			continue;
		}
		if ( $idx > 0 && false !== strpos( $lines[ $idx - 1 ], 'phpcs:ignore' ) ) {
			continue;
		}

		$needs_fix = false;

		// file_get_contents — but skip URL arguments (those should use wp_remote_get).
		// Also skip if already replaced (->get_contents).
		if ( preg_match( '/\bfile_get_contents\s*\(/', $line ) && ! preg_match( '/->get_contents\s*\(/', $line ) ) {
			// Skip if argument is a URL string.
			if ( preg_match( '/file_get_contents\s*\(\s*[\'"]https?:/', $line ) ) {
				continue;
			}
			$needs_fix = true;
		}

		// file_put_contents — skip if already replaced (->put_contents).
		if ( preg_match( '/\bfile_put_contents\s*\(/', $line ) && ! preg_match( '/->put_contents\s*\(/', $line ) ) {
			$needs_fix = true;
		}

		// is_writable — but skip if already a method call (->is_writable).
		if ( preg_match( '/\bis_writable\s*\(/', $line ) && ! preg_match( '/->is_writable\s*\(/', $line ) ) {
			$needs_fix = true;
		}

		if ( $needs_fix ) {
			$target_lines[] = $idx;

			// Find enclosing method's opening brace.
			$method_brace = find_enclosing_method_brace( $lines, $idx );
			if ( null !== $method_brace ) {
				$methods_needing_fs[ $method_brace ] = true;
			}
		}
	}

	if ( empty( $target_lines ) ) {
		return 0;
	}

	// Second pass: inject $fs initialization at the top of each method that needs it.
	// We do this in reverse order so line number offsets don't shift.
	$method_braces = array_keys( $methods_needing_fs );
	rsort( $method_braces );

	foreach ( $method_braces as $brace_line ) {
		// Check if $fs or $wp_filesystem is already initialized in this method body.
		$method_end   = find_matching_brace( $lines, $brace_line );
		$already_init = false;
		for ( $i = $brace_line; $i <= min( $method_end, $brace_line + 5 ); $i++ ) {
			if ( preg_match( '/\$fs\s*=/', $lines[ $i ] ) || preg_match( '/global\s+\$wp_filesystem/', $lines[ $i ] ) ) {
				$already_init = true;
				break;
			}
		}

		if ( ! $already_init ) {
			// Detect indent from the line after the opening brace.
			$body_indent = "\t\t";
			if ( isset( $lines[ $brace_line + 1 ] ) && preg_match( '/^(\s+)/', $lines[ $brace_line + 1 ], $m ) ) {
				$body_indent = $m[1];
			}

			if ( $has_helper ) {
				$init_line = $body_indent . '$fs = FilesystemHelper::get();';
			} else {
				$init_line = $body_indent . 'global $wp_filesystem;';
			}

			// Insert after the opening brace line.
			array_splice( $lines, $brace_line + 1, 0, array( $init_line ) );

			// Adjust target_lines that come after this insertion point.
			foreach ( $target_lines as &$tl ) {
				if ( $tl > $brace_line ) {
					$tl++;
				}
			}
			unset( $tl );

			$changed = true;
		}
	}

	// Third pass: replace function calls.
	foreach ( $target_lines as $idx ) {
		$line = &$lines[ $idx ];

		// Determine which filesystem variable to use.
		$fs_var = $has_helper ? '$fs' : '$wp_filesystem';

		// file_get_contents($path) → $fs->get_contents($path)
		if ( preg_match( '/\bfile_get_contents\s*\(/', $line ) ) {
			$line    = preg_replace(
				'/\bfile_get_contents\s*\(\s*(.+?)\s*\)/',
				$fs_var . '->get_contents( $1 )',
				$line
			);
			$fixes++;
			$changed = true;
			continue;
		}

		// file_put_contents with FILE_APPEND → read-concat-write.
		if ( preg_match( '/\bfile_put_contents\s*\(.*FILE_APPEND/', $line ) ) {
			// Parse: file_put_contents( $path, $content, FILE_APPEND )
			// or: $var = file_put_contents( $path, $content, FILE_APPEND );
			if ( preg_match( '/^(\s*)(\$\w+\s*=\s*)?file_put_contents\s*\(\s*(.+?)\s*,\s*(.+?)\s*,\s*FILE_APPEND\s*\)\s*;/', $line, $m ) ) {
				$indent      = $m[1];
				$assign      = $m[2] ? trim( $m[2], ' =' ) : '';
				$path_arg    = $m[3];
				$content_arg = $m[4];

				// Build read-concat-write replacement.
				$replacement  = $indent . '$_existing_content = ' . $fs_var . '->get_contents( ' . $path_arg . ' );' . "\n";
				$replacement .= $indent . '$_existing_content = ( false !== $_existing_content ) ? $_existing_content : \'\';';

				if ( $assign ) {
					$replacement .= "\n" . $indent . $assign . ' = ' . $fs_var . '->put_contents( ' . $path_arg . ', $_existing_content . ' . $content_arg . ' );';
				} else {
					$replacement .= "\n" . $indent . $fs_var . '->put_contents( ' . $path_arg . ', $_existing_content . ' . $content_arg . ' );';
				}

				$line    = $replacement;
				$fixes++;
				$changed = true;
			}
			continue;
		}

		// file_put_contents($path, $content) → $fs->put_contents($path, $content)
		if ( preg_match( '/\bfile_put_contents\s*\(/', $line ) ) {
			$line    = preg_replace(
				'/\bfile_put_contents\s*\(\s*(.+?)\s*,\s*(.+?)\s*\)/',
				$fs_var . '->put_contents( $1, $2 )',
				$line
			);
			$fixes++;
			$changed = true;
			continue;
		}

		// is_writable($path) → $fs->is_writable($path)
		// Guard against re-replacing already-fixed ->is_writable() calls.
		if ( preg_match( '/\bis_writable\s*\(/', $line ) && ! preg_match( '/->is_writable\s*\(/', $line ) ) {
			$line    = preg_replace(
				'/\bis_writable\s*\(\s*(.+?)\s*\)/',
				$fs_var . '->is_writable( $1 )',
				$line
			);
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

/**
 * Find the line number of the opening brace of the method/function enclosing a given line.
 *
 * Walks backward tracking brace depth. Each time depth goes negative (we exit a scope),
 * we check if that scope is a function. We keep going until we find a function scope,
 * ignoring intermediate scopes (if/else/for/foreach/while/try/catch/class).
 *
 * @param array $lines All lines.
 * @param int   $idx   Line index to search from.
 * @return int|null Line index of the function's opening brace, or null.
 */
function find_enclosing_method_brace( $lines, $idx ) {
	$depth = 0;

	for ( $i = $idx; $i >= 0; $i-- ) {
		$line = $lines[ $i ];

		// Count braces going backward (reversed: } increases, { decreases).
		$depth += substr_count( $line, '}' ) - substr_count( $line, '{' );

		// When depth goes negative, we've exited an enclosing scope.
		if ( $depth < 0 ) {
			// Check if this line or nearby lines have a function declaration.
			for ( $look = $i; $look >= max( 0, $i - 5 ); $look-- ) {
				if ( preg_match( '/\bfunction\s+\w+\s*\(/', $lines[ $look ] ) ) {
					return $i;
				}
				// Also match anonymous functions / closures.
				if ( preg_match( '/\bfunction\s*\(/', $lines[ $look ] ) ) {
					return $i;
				}
			}
			// Not a function scope — it's an if/else/for/class scope.
			// Reset depth to 0 and keep looking for the actual function.
			$depth = 0;
		}
	}

	return null;
}

/**
 * Find the line of the matching closing brace for an opening brace.
 *
 * @param array $lines      All lines.
 * @param int   $brace_line Line index of the opening brace.
 * @return int Line index of the closing brace.
 */
function find_matching_brace( $lines, $brace_line ) {
	$depth = 0;
	for ( $i = $brace_line; $i < count( $lines ); $i++ ) {
		$depth += substr_count( $lines[ $i ], '{' ) - substr_count( $lines[ $i ], '}' );
		if ( 0 === $depth && $i > $brace_line ) {
			return $i;
		}
	}
	return count( $lines ) - 1;
}
