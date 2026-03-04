<?php
/**
 * Shared error type classification for PHPUnit test failures.
 *
 * Given an error message, returns a category string that homeboy core
 * uses to cluster failures by root cause.
 *
 * Categories: FatalError, AssertionFailedError, TypeError, MockError, Error
 */

/**
 * Classify an error message into a type category.
 *
 * @param string $message The error message from a test failure.
 * @return string The error type category.
 */
function classify_error_type( string $message ): string {
	// Fatal errors
	if ( preg_match( '/^(Fatal error|PHP Fatal error):/i', $message ) ) {
		return 'FatalError';
	}
	if ( preg_match( '/^Cannot redeclare\b/', $message ) ) {
		return 'FatalError';
	}

	// PHPUnit assertion failures
	if ( preg_match( '/^Failed asserting/', $message ) ) {
		return 'AssertionFailedError';
	}

	// Type errors
	if ( preg_match( '/must be of type/', $message ) ) {
		return 'TypeError';
	}
	if ( preg_match( '/Return value .* must be of type/', $message ) ) {
		return 'TypeError';
	}

	// Mock configuration errors
	if ( preg_match( '/Trying to configure method .* which cannot be configured/', $message ) ) {
		return 'MockError';
	}
	if ( preg_match( '/does not allow named arguments/', $message ) ) {
		return 'MockError';
	}

	// Undefined method/function
	if ( preg_match( '/^Call to undefined method/', $message ) ) {
		return 'Error';
	}
	if ( preg_match( '/^Call to undefined function/', $message ) ) {
		return 'Error';
	}

	// Class not found
	if ( preg_match( '/^Class .* not found/', $message ) ) {
		return 'Error';
	}

	// Explicit exception class in message (e.g. "ErrorException: ...")
	if ( preg_match( '/^(\w+(?:\\\\\w+)*(?:Error|Exception)):\s/', $message, $em ) ) {
		return $em[1];
	}

	// Unexpected notice / _doing_it_wrong
	if ( preg_match( '/^Unexpected .* notice/', $message ) ) {
		return 'UnexpectedNotice';
	}

	return 'Error';
}

/**
 * Extract source file and test file from stack trace lines.
 *
 * @param array  $trace_lines     Array of trace line strings (e.g. "/path/to/file.php:42").
 * @param string $component_path  Component path prefix to strip for relative paths.
 * @return array{source_file: string, source_line: int, test_file: string}
 */
function extract_source_from_trace( array $trace_lines, string $component_path = '' ): array {
	$source_file = '';
	$source_line = 0;
	$test_file   = '';

	foreach ( $trace_lines as $tline ) {
		if ( preg_match( '#^(/[^\s:]+\.php):(\d+)#', $tline, $tm ) ) {
			$file     = $tm[1];
			$line_num = (int) $tm[2];

			// Strip component_path prefix for relative paths
			$rel_file = $file;
			if ( $component_path && strpos( $file, $component_path ) === 0 ) {
				$rel_file = substr( $file, strlen( $component_path ) );
			}

			// Test files contain /tests/ in their path
			if ( strpos( $file, '/tests/' ) !== false || strpos( $file, 'Test.php' ) !== false ) {
				if ( empty( $test_file ) ) {
					$test_file = $rel_file;
				}
			} else {
				// Non-test file — deepest (first) non-test frame
				if ( empty( $source_file ) ) {
					$source_file = $rel_file;
					$source_line = $line_num;
				}
			}
		}
	}

	return [
		'source_file' => $source_file,
		'source_line' => $source_line,
		'test_file'   => $test_file,
	];
}

/**
 * Try to extract source file info from the error message itself.
 * Useful for fatal errors: "Cannot redeclare func() (previously declared in /path:42)"
 *
 * @param string $message        The error message.
 * @param string $component_path Component path prefix.
 * @return array{source_file: string, source_line: int}|null Null if nothing found.
 */
function extract_source_from_message( string $message, string $component_path = '' ): ?array {
	if ( preg_match( '#in (/[^\s:]+\.php):(\d+)#', $message, $mm ) ) {
		$file     = $mm[1];
		$line_num = (int) $mm[2];
		$rel_file = $file;
		if ( $component_path && strpos( $file, $component_path ) === 0 ) {
			$rel_file = substr( $file, strlen( $component_path ) );
		}
		if ( strpos( $file, '/tests/' ) === false && strpos( $file, 'Test.php' ) === false ) {
			return [
				'source_file' => $rel_file,
				'source_line' => $line_num,
			];
		}
	}
	return null;
}

/**
 * Guess test file path from a fully qualified test name.
 *
 * @param string $test_name e.g. "DataMachine\Tests\Unit\Abilities\FileAbilities::test_method"
 * @return string e.g. "tests/DataMachine/Tests/Unit/Abilities/FileAbilities.php"
 */
function guess_test_file_from_name( string $test_name ): string {
	if ( preg_match( '/^(.+)::/', $test_name, $nm ) ) {
		$class_fqn = $nm[1];
		return 'tests/' . str_replace( '\\', '/', $class_fqn ) . '.php';
	}
	return '';
}
