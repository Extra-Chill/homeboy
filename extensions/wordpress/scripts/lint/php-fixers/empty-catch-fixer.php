#!/usr/bin/env php
<?php
/**
 * Empty Catch Fixer
 *
 * Inserts error_log() calls into empty catch blocks.
 *
 * PHPCS flags empty catch blocks (Generic.CodeAnalysis.EmptyStatement.DetectedCatch).
 * Instead of suppressing, we add a meaningful log line. If the catch block has a
 * comment explaining why it's empty, we preserve the comment and still add the log.
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
 * Process a single PHP file using line-based approach.
 *
 * Detects catch blocks where the body is empty or contains only comments,
 * then inserts an error_log() call using the caught exception variable.
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

		// Look for: } catch ( ... $variable ) {
		if ( preg_match( '/\}\s*catch\s*\(\s*\\\\?[\w\\\\]+\s+(\$\w+)\s*\)\s*\{/', $trimmed, $m ) ) {
			$exception_var = $m[1];
			$new_lines[]   = $line;
			$i++;

			// Collect lines inside the catch block until closing brace.
			$body_lines    = array();
			$brace_depth   = 1;
			$catch_start   = $i;

			while ( $i < $count && $brace_depth > 0 ) {
				$inner   = $lines[ $i ];
				$brace_depth += substr_count( $inner, '{' ) - substr_count( $inner, '}' );

				if ( $brace_depth > 0 ) {
					$body_lines[] = $inner;
					$i++;
				}
			}

			// Check if body is empty or comment-only.
			$has_code = false;
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
				$has_code = true;
				break;
			}

			if ( ! $has_code ) {
				// Detect indent from the catch line.
				preg_match( '/^(\s*)/', $lines[ $catch_start - 1 ], $indent_match );
				$catch_indent = $indent_match[1];
				$body_indent  = $catch_indent . "\t";

				// Preserve existing comments.
				foreach ( $body_lines as $body_line ) {
					$body_trimmed = trim( $body_line );
					if ( $body_trimmed !== '' ) {
						$new_lines[] = $body_line;
					}
				}

				// Derive a context label from the function/method containing this catch.
				$context = derive_catch_context( $lines, $catch_start - 1 );

				// Insert wp_trigger_error() — WordPress-approved error reporting (WP 6.4+).
				// Unlike error_log(), PHPCS does not flag this as a development function.
				$new_lines[] = "{$body_indent}wp_trigger_error( __FUNCTION__, '{$context}: ' . {$exception_var}->getMessage(), E_USER_NOTICE );\n";
				$fixes++;
			} else {
				// Body has real code — emit as-is.
				foreach ( $body_lines as $body_line ) {
					$new_lines[] = $body_line;
				}
			}

			// Emit the closing brace line (still at $i).
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
 * Derive a context label from the enclosing function/method.
 *
 * Scans backwards from the catch line to find the nearest function declaration.
 * Returns something like "ClassName::methodName catch" or "function_name catch".
 */
function derive_catch_context( array $lines, int $from_line ): string {
	$class_name    = '';
	$function_name = '';

	for ( $i = $from_line; $i >= 0; $i-- ) {
		$line = $lines[ $i ];

		// Find enclosing function.
		if ( $function_name === '' && preg_match( '/function\s+(\w+)\s*\(/', $line, $m ) ) {
			$function_name = $m[1];
		}

		// Find enclosing class.
		if ( $class_name === '' && preg_match( '/^\s*(?:class|trait)\s+(\w+)/', $line, $m ) ) {
			$class_name = $m[1];
		}

		// Stop once we have both.
		if ( $function_name !== '' && $class_name !== '' ) {
			break;
		}
	}

	if ( $class_name !== '' && $function_name !== '' ) {
		return "{$class_name}::{$function_name} catch";
	}
	if ( $function_name !== '' ) {
		return "{$function_name} catch";
	}

	return 'catch block';
}
