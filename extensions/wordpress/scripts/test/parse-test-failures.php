<?php
/**
 * Parse PHPUnit output into structured failure data for homeboy test --analyze.
 *
 * Orchestrator that delegates to format-specific parsers (standard, testdox)
 * and uses shared error classification. Each parser returns raw blocks with
 * 'header' and 'body_lines' keys. This file converts them into TestFailure
 * structures matching homeboy's TestAnalysisInput schema.
 *
 * Output fields per failure preserve the legacy Homeboy analysis keys and add
 * normalized sidecar keys consumed by cross-runner tooling:
 * - test_name/test_id: fully qualified test method name
 * - test_file: test file path (from stack trace)
 * - error_type/failure_type: exception/error class name
 * - message: error message
 * - source_file/source_line: deepest non-test frame source location
 * - suite: test framework/suite label
 * - file/line: normalized primary failure location
 * - fingerprint: stable hash for grouping equivalent failures
 * - stdout_excerpt/stderr_excerpt: bounded captured output excerpts
 *
 * Usage: php parse-test-failures.php <phpunit_output_file> [component_path]
 */

require_once __DIR__ . '/parsers/classify-error.php';
require_once __DIR__ . '/parsers/standard.php';
require_once __DIR__ . '/parsers/testdox.php';

if ( $argc < 2 ) {
	fwrite( STDERR, "Usage: php parse-test-failures.php <phpunit_output_file> [component_path]\n" );
	exit( 1 );
}

$output_file    = $argv[1];
$component_path = $argc >= 3 ? rtrim( $argv[2], '/' ) . '/' : '';

if ( ! file_exists( $output_file ) ) {
	fwrite( STDERR, "File not found: $output_file\n" );
	exit( 1 );
}

$raw   = file_get_contents( $output_file );
$lines = explode( "\n", $raw );

// ============================================================================
// Phase 1: Extract test counts from summary line
// ============================================================================

$total  = 0;
$passed = 0;

// Success: "OK (N tests, N assertions)"
if ( preg_match( '/OK \((\d+) tests?/', $raw, $m ) ) {
	$total  = (int) $m[1];
	$passed = $total;
}
// Failure: "Tests: N, Assertions: N, Errors: N, Failures: N, Skipped: N."
elseif ( preg_match( '/^Tests:\s*(\d+)/m', $raw, $m ) ) {
	$total        = (int) $m[1];
	$errors       = 0;
	$failed_count = 0;
	$skipped      = 0;
	if ( preg_match( '/Errors:\s*(\d+)/', $raw, $em ) ) {
		$errors = (int) $em[1];
	}
	if ( preg_match( '/Failures:\s*(\d+)/', $raw, $fm ) ) {
		$failed_count = (int) $fm[1];
	}
	if ( preg_match( '/Skipped:\s*(\d+)/', $raw, $sm ) ) {
		$skipped = (int) $sm[1];
	}
	$passed = max( 0, $total - $errors - $failed_count - $skipped );
}

// ============================================================================
// Phase 2: Parse failure blocks — try each parser until one produces results
// ============================================================================

$blocks = parse_standard_blocks( $lines );

if ( empty( $blocks ) ) {
	$blocks = parse_testdox_blocks( $lines );
}

// ============================================================================
// Phase 3: Convert raw blocks into TestFailure structures
// ============================================================================

$failures = [];

foreach ( $blocks as $block ) {
	$header = $block['header'];
	$body   = $block['body_lines'];

	// Parse test name from header
	// Format: "Namespace\ClassTest::testMethod" or with " with data set #0"
	$test_name = $header;
	if ( strpos( $test_name, ' with data set' ) !== false ) {
		$test_name = substr( $test_name, 0, strpos( $test_name, ' with data set' ) );
	}

	// Separate message lines from trace lines
	$message_lines = [];
	$trace_lines   = [];
	$in_trace      = false;

	foreach ( $body as $bline ) {
		$trimmed = trim( $bline );

		if ( $trimmed === '' ) {
			continue;
		}

		// Stack trace lines: "/path/to/file.php:42"
		if ( preg_match( '#^(/[^\s:]+\.php):(\d+)$#', $trimmed ) ||
			preg_match( '#^(/[^\s:]+\.php:\d+)$#', $trimmed ) ) {
			$in_trace      = true;
			$trace_lines[] = $trimmed;
			continue;
		}

		// Indented trace: "at /path/to/file.php:42"
		if ( preg_match( '#^\s*at\s+(/[^\s:]+\.php):(\d+)#', $bline ) ) {
			$in_trace      = true;
			$trace_lines[] = $trimmed;
			continue;
		}

		if ( ! $in_trace ) {
			$message_lines[] = $trimmed;
		}
	}

	$message = rtrim( implode( "\n", $message_lines ) );

	// Classify error type using shared logic
	$error_type = classify_error_type( $message );

	// Extract source/test files from trace
	$source_info = extract_source_from_trace( $trace_lines, $component_path );
	$source_file = $source_info['source_file'];
	$source_line = $source_info['source_line'];
	$test_file   = $source_info['test_file'];

	// Fallback: guess test file from test name
	if ( empty( $test_file ) ) {
		$test_file = guess_test_file_from_name( $test_name );
	}

	// Fallback: extract source from message (fatal errors)
	if ( empty( $source_file ) ) {
		$msg_source = extract_source_from_message( $message, $component_path );
		if ( $msg_source ) {
			$source_file = $msg_source['source_file'];
			$source_line = $msg_source['source_line'];
		}
	}

	$file           = $source_file ?: $test_file;
	$line           = $source_line ?: 0;
	$stdout_excerpt = make_output_excerpt( array_merge( [ $header ], $body ) );
	$fingerprint    = make_failure_fingerprint( $test_name, $file, $line, $error_type, $message );

	$failures[] = [
		'test_name'      => $test_name,
		'test_file'      => $test_file,
		'error_type'     => $error_type,
		'message'        => $message,
		'source_file'    => $source_file,
		'source_line'    => $source_line,
		'test_id'        => $test_name,
		'suite'          => 'phpunit',
		'file'           => $file,
		'line'           => $line,
		'failure_type'   => $error_type,
		'fingerprint'    => $fingerprint,
		'stdout_excerpt' => $stdout_excerpt,
		'stderr_excerpt' => '',
	];
}

/**
 * Create a stable grouping key from the normalized failure identity.
 */
function make_failure_fingerprint( string $test_name, string $file, int $line, string $error_type, string $message ): string {
	$first_message_line = strtok( $message, "\n" );
	if ( $first_message_line === false ) {
		$first_message_line = '';
	}

	return hash( 'sha256', implode( "\0", [ $test_name, $file, (string) $line, $error_type, $first_message_line ] ) );
}

/**
 * Keep failure excerpts compact enough for sidecar consumers and PR comments.
 *
 * @param array $lines Raw output lines for one failure block.
 */
function make_output_excerpt( array $lines ): string {
	$excerpt = trim( implode( "\n", array_slice( $lines, 0, 40 ) ) );

	if ( strlen( $excerpt ) > 4000 ) {
		$excerpt = substr( $excerpt, 0, 3997 ) . '...';
	}

	return $excerpt;
}

// ============================================================================
// Output
// ============================================================================

$output = [
	'failures' => $failures,
	'total'    => $total,
	'passed'   => $passed,
];

echo json_encode( $output, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES ) . "\n";
