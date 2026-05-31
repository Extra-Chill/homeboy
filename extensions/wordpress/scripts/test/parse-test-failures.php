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
 * Usage: php parse-test-failures.php <phpunit_output_file|wp-codebox-artifact-dir|wp-codebox-test-results.json> [component_path]
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

if ( is_dir( $output_file ) && file_exists( $output_file . '/files/test-results.json' ) ) {
	$output_file = $output_file . '/files/test-results.json';
}

if ( ! file_exists( $output_file ) ) {
	fwrite( STDERR, "File not found: $output_file\n" );
	exit( 1 );
}

$raw   = file_get_contents( $output_file );
$lines = explode( "\n", $raw );

$wp_codebox_payload = json_decode( $raw, true );
if ( is_array( $wp_codebox_payload ) && ( $wp_codebox_payload['schema'] ?? '' ) === 'wp-codebox/test-results/v1' ) {
	echo json_encode(
		parse_wp_codebox_test_results( $wp_codebox_payload, $output_file, $component_path ),
		JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES
	) . "\n";
	exit( 0 );
}

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

/**
 * Normalize WP Codebox test artifacts into Homeboy's WordPress failure sidecar.
 */
function parse_wp_codebox_test_results( array $payload, string $test_results_path, string $component_path ): array {
	$summary = is_array( $payload['summary'] ?? null ) ? $payload['summary'] : [];
	$total   = (int) ( $summary['total'] ?? 0 );
	$passed  = (int) ( $summary['passed'] ?? 0 );

	$artifact_root = dirname( dirname( $test_results_path ) );
	$raw_excerpt   = make_wp_codebox_raw_log_excerpt( $payload, $artifact_root );
	$failures      = [];
	$suites        = is_array( $payload['suites'] ?? null ) ? $payload['suites'] : [];

	foreach ( $suites as $suite ) {
		if ( ! is_array( $suite ) ) {
			continue;
		}

		$suite_name = (string) ( $suite['name'] ?? 'wp-codebox' );
		foreach ( wp_codebox_failure_entries( $suite ) as $entry ) {
			if ( ! is_array( $entry ) ) {
				continue;
			}

			$failures[] = normalize_wp_codebox_failure_entry( $entry, $suite_name, $component_path, $raw_excerpt );
		}
	}

	$failures = array_merge( $failures, wp_codebox_diagnostic_failures( $artifact_root, $component_path, $raw_excerpt ) );

	return [
		'failures' => $failures,
		'total'    => $total,
		'passed'   => $passed,
	];
}

/**
 * Convert generic WP Codebox artifact diagnostics into Homeboy failure entries.
 */
function wp_codebox_diagnostic_failures( string $artifact_root, string $component_path, string $raw_excerpt ): array {
	$diagnostics_path = $artifact_root . '/files/diagnostics.json';
	if ( ! is_file( $diagnostics_path ) ) {
		return [];
	}

	$payload = json_decode( (string) file_get_contents( $diagnostics_path ), true );
	if ( ! is_array( $payload ) || ( $payload['schema'] ?? '' ) !== 'wp-codebox/artifact-diagnostics/v1' ) {
		return [];
	}

	$entries     = [];
	$diagnostics = is_array( $payload['diagnostics'] ?? null ) ? $payload['diagnostics'] : [];
	foreach ( $diagnostics as $diagnostic ) {
		if ( ! is_array( $diagnostic ) ) {
			continue;
		}

		$severity = strtolower( (string) ( $diagnostic['severity'] ?? 'warning' ) );
		if ( ! in_array( $severity, [ 'error', 'warning' ], true ) ) {
			continue;
		}

		$diagnostic_id = (string) ( $diagnostic['id'] ?? $diagnostic['type'] ?? 'wp-codebox diagnostic' );
		$message       = (string) ( $diagnostic['message'] ?? $diagnostic['type'] ?? $diagnostic_id );
		$file          = wp_codebox_normalize_component_path( (string) ( $diagnostic['path'] ?? '' ), $component_path );
		$error_type    = 'WPCodebox' . ucfirst( $severity ) . 'Diagnostic';

		$entries[] = [
			'test_name'      => $diagnostic_id,
			'test_file'      => '',
			'error_type'     => $error_type,
			'message'        => $message,
			'source_file'    => $file,
			'source_line'    => 0,
			'test_id'        => $diagnostic_id,
			'suite'          => 'wp-codebox-diagnostics',
			'file'           => $file,
			'line'           => 0,
			'failure_type'   => $error_type,
			'fingerprint'    => make_failure_fingerprint( $diagnostic_id, $file, 0, $error_type, $message ),
			'stdout_excerpt' => make_output_excerpt( explode( "\n", $raw_excerpt ) ),
			'stderr_excerpt' => '',
			'diagnostic'     => $diagnostic,
		];
	}

	return $entries;
}

/**
 * Extract failed test entries from the flexible WP Codebox suite shape.
 */
function wp_codebox_failure_entries( array $suite ): array {
	$entries = [];

	foreach ( [ 'failures', 'errors' ] as $key ) {
		if ( is_array( $suite[ $key ] ?? null ) ) {
			$entries = array_merge( $entries, $suite[ $key ] );
		}
	}

	$test_entries = [];
	foreach ( [ 'testCases', 'cases', 'tests' ] as $key ) {
		if ( is_array( $suite[ $key ] ?? null ) ) {
			$test_entries = $suite[ $key ];
			break;
		}
	}

	foreach ( $test_entries as $test ) {
		if ( is_array( $test ) && in_array( $test['status'] ?? '', [ 'failed', 'error' ], true ) ) {
			$entries[] = $test;
		}
	}

	return $entries;
}

/**
 * Convert one WP Codebox failure entry into the existing TestFailure shape.
 */
function normalize_wp_codebox_failure_entry( array $entry, string $suite_name, string $component_path, string $raw_excerpt ): array {
	$test_name = (string) ( $entry['test_name'] ?? $entry['testName'] ?? $entry['test_id'] ?? $entry['id'] ?? $entry['name'] ?? 'wp-codebox failure' );
	$message   = (string) ( $entry['message'] ?? $entry['failureMessage'] ?? $entry['error'] ?? '' );

	if ( $message === '' && is_array( $entry['failure'] ?? null ) ) {
		$message = (string) ( $entry['failure']['message'] ?? $entry['failure']['error'] ?? '' );
	}

	$error_type = (string) ( $entry['error_type'] ?? $entry['errorType'] ?? $entry['failure_type'] ?? '' );
	if ( $error_type === '' && is_array( $entry['failure'] ?? null ) ) {
		$error_type = (string) ( $entry['failure']['type'] ?? $entry['failure']['errorType'] ?? '' );
	}
	if ( $error_type === '' ) {
		$error_type = classify_error_type( $message );
	}

	$test_file   = wp_codebox_normalize_component_path( (string) ( $entry['test_file'] ?? $entry['testFile'] ?? '' ), $component_path );
	$source_file = wp_codebox_normalize_component_path( (string) ( $entry['source_file'] ?? $entry['sourceFile'] ?? $entry['file'] ?? '' ), $component_path );
	$source_line = (int) ( $entry['source_line'] ?? $entry['sourceLine'] ?? $entry['line'] ?? 0 );

	if ( $test_file === '' && $source_file !== '' && ( strpos( $source_file, '/tests/' ) !== false || strpos( $source_file, 'Test.php' ) !== false ) ) {
		$test_file   = $source_file;
		$source_file = '';
		$source_line = 0;
	}

	if ( $test_file === '' ) {
		$test_file = guess_test_file_from_name( $test_name );
	}

	$file           = $source_file ?: $test_file;
	$line           = $source_line;
	$stdout_excerpt = (string) ( $entry['stdout_excerpt'] ?? $entry['stdout'] ?? $entry['output'] ?? '' );
	$stderr_excerpt = (string) ( $entry['stderr_excerpt'] ?? $entry['stderr'] ?? '' );

	if ( $stdout_excerpt === '' ) {
		$stdout_excerpt = $raw_excerpt;
	}

	$fingerprint = (string) ( $entry['fingerprint'] ?? '' );
	if ( $fingerprint === '' ) {
		$fingerprint = make_failure_fingerprint( $test_name, $file, $line, $error_type, $message );
	}

	return [
		'test_name'      => $test_name,
		'test_file'      => $test_file,
		'error_type'     => $error_type,
		'message'        => $message,
		'source_file'    => $source_file,
		'source_line'    => $source_line,
		'test_id'        => $test_name,
		'suite'          => $suite_name ?: 'wp-codebox',
		'file'           => $file,
		'line'           => $line,
		'failure_type'   => $error_type,
		'fingerprint'    => $fingerprint,
		'stdout_excerpt' => make_output_excerpt( explode( "\n", $stdout_excerpt ) ),
		'stderr_excerpt' => make_output_excerpt( explode( "\n", $stderr_excerpt ) ),
	];
}

/**
 * Make artifact paths relative to the tested component when possible.
 */
function wp_codebox_normalize_component_path( string $file, string $component_path ): string {
	if ( $file === '' ) {
		return '';
	}

	if ( $component_path && strpos( $file, $component_path ) === 0 ) {
		return substr( $file, strlen( $component_path ) );
	}

	return $file;
}

/**
 * Preserve nearby command/runtime logs for debugging artifact-only failures.
 */
function make_wp_codebox_raw_log_excerpt( array $payload, string $artifact_root ): string {
	$references = is_array( $payload['rawLogReferences'] ?? null ) ? $payload['rawLogReferences'] : [];
	$chunks     = [];

	foreach ( $references as $reference ) {
		if ( ! is_array( $reference ) || empty( $reference['path'] ) ) {
			continue;
		}

		$path = (string) $reference['path'];
		if ( substr( $path, 0, 1 ) === '/' ) {
			$log_path = $path;
		} else {
			$log_path = $artifact_root . '/' . $path;
		}

		if ( ! is_file( $log_path ) ) {
			continue;
		}

		$contents = trim( (string) file_get_contents( $log_path ) );
		if ( $contents === '' ) {
			continue;
		}

		$chunks[] = $path . "\n" . $contents;
	}

	return make_output_excerpt( explode( "\n", implode( "\n\n", $chunks ) ) );
}
