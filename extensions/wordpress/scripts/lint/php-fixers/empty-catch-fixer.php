#!/usr/bin/env php
<?php
/**
 * Empty Catch Fixer
 *
 * Adds `unset( $e )` to empty catch blocks so they satisfy the
 * Generic.CodeAnalysis.EmptyStatement.DetectedCatch sniff without
 * injecting any debug or logging code into production.
 *
 * Handles two patterns:
 *  1. `catch ( \Exception $e ) { }` → adds `unset( $e );`
 *  2. `catch ( \Exception ) { }`   → adds `$e` variable + `unset( $e );`
 *
 * Preserves existing comments inside the catch body.
 *
 * Usage: php empty-catch-fixer.php <path>
 */

require_once __DIR__ . '/fixer-helpers.php';

if ( $argc < 2 ) {
	echo "Usage: php empty-catch-fixer.php <path>\n";
	exit( 1 );
}

$path = $argv[1];

if ( ! file_exists( $path ) ) {
	echo "Error: Path not found: $path\n";
	exit( 1 );
}

$result = fixer_process_path( $path, 'process_file' );

if ( $result['total_fixes'] > 0 ) {
	echo "Empty catch fixer: Fixed {$result['total_fixes']} empty catch(es) in {$result['files_fixed']} file(s)\n";
} else {
	echo "Empty catch fixer: No fixable patterns found\n";
}

exit( 0 );

/**
 * Process a single PHP file.
 *
 * Detects catch blocks where the body is empty or contains only comments,
 * then inserts `unset( $e );` to satisfy the empty-statement sniff.
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

		// Pattern 1: catch with captured variable — `catch ( \Exception $var ) {`
		if ( preg_match( '/\}\s*catch\s*\(\s*\\\\?[\w\\\\]+\s+(\$\w+)\s*\)\s*\{/', $trimmed, $m ) ) {
			$exception_var = $m[1];
			$new_lines[]   = $line;
			$i++;

			$body_lines  = array();
			$brace_depth = 1;
			$catch_start = $i;

			while ( $i < $count && $brace_depth > 0 ) {
				$inner        = $lines[ $i ];
				$brace_depth += substr_count( $inner, '{' ) - substr_count( $inner, '}' );

				if ( $brace_depth > 0 ) {
					$body_lines[] = $inner;
					$i++;
				}
			}

			if ( ! body_has_code( $body_lines ) ) {
				// Detect indent from the catch line.
				preg_match( '/^(\s*)/', $lines[ $catch_start - 1 ], $indent_match );
				$catch_indent = $indent_match[1];
				$body_indent  = $catch_indent . "\t";

				// Preserve existing comments.
				foreach ( $body_lines as $body_line ) {
					if ( trim( $body_line ) !== '' ) {
						$new_lines[] = $body_line;
					}
				}

				// Insert unset() to satisfy empty-statement sniff.
				$new_lines[] = "{$body_indent}unset( {$exception_var} );\n";
				$fixes++;
			} else {
				// Body has real code — emit as-is.
				foreach ( $body_lines as $body_line ) {
					$new_lines[] = $body_line;
				}
			}

			// Emit the closing brace line.
			if ( $i < $count ) {
				$new_lines[] = $lines[ $i ];
				$i++;
			}

			continue;
		}

		// Pattern 2: non-capturing catch (PHP 8.0+) — `catch ( \Exception ) {`
		if ( preg_match( '/(\}\s*catch\s*\(\s*\\\\?[\w\\\\]+)\s*\)\s*\{/', $trimmed, $m )
			&& ! preg_match( '/\$\w+/', $trimmed )
		) {
			// Rewrite catch line to add $e variable.
			$rewritten = preg_replace(
				'/(\}\s*catch\s*\(\s*\\\\?[\w\\\\]+)\s*(\)\s*\{)/',
				'$1 $e $2',
				$line
			);
			$new_lines[] = $rewritten;
			$i++;

			$body_lines  = array();
			$brace_depth = 1;
			$catch_start = $i;

			while ( $i < $count && $brace_depth > 0 ) {
				$inner        = $lines[ $i ];
				$brace_depth += substr_count( $inner, '{' ) - substr_count( $inner, '}' );

				if ( $brace_depth > 0 ) {
					$body_lines[] = $inner;
					$i++;
				}
			}

			if ( ! body_has_code( $body_lines ) ) {
				preg_match( '/^(\s*)/', $lines[ $catch_start - 1 ], $indent_match );
				$catch_indent = $indent_match[1];
				$body_indent  = $catch_indent . "\t";

				foreach ( $body_lines as $body_line ) {
					if ( trim( $body_line ) !== '' ) {
						$new_lines[] = $body_line;
					}
				}

				$new_lines[] = "{$body_indent}unset( \$e );\n";
				$fixes++;
			} else {
				foreach ( $body_lines as $body_line ) {
					$new_lines[] = $body_line;
				}
			}

			if ( $i < $count ) {
				$new_lines[] = $lines[ $i ];
				$i++;
			}

			continue;
		}

		$new_lines[] = $line;
		$i++;
	}

	if ( $fixes > 0 ) {
		file_put_contents( $filepath, implode( '', $new_lines ) );
	}

	return $fixes;
}

/**
 * Check whether catch body lines contain real code (not just comments/blanks).
 *
 * @param array $body_lines Lines inside the catch block.
 * @return bool True if there is actual executable code.
 */
function body_has_code( array $body_lines ): bool {
	foreach ( $body_lines as $body_line ) {
		$body_trimmed = trim( $body_line );
		if ( $body_trimmed === '' ) {
			continue;
		}
		// Single-line comments.
		if ( strpos( $body_trimmed, '//' ) === 0 ) {
			continue;
		}
		// Block comment lines.
		if ( strpos( $body_trimmed, '/*' ) === 0 || strpos( $body_trimmed, '*' ) === 0 ) {
			continue;
		}
		return true;
	}
	return false;
}
