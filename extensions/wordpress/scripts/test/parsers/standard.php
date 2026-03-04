<?php
/**
 * Standard PHPUnit output parser.
 *
 * Parses the numbered failure block format:
 *
 *   There was 1 failure:
 *
 *   1) Namespace\ClassTest::testMethod
 *   Failed asserting that ...
 *
 *   /path/to/file.php:42
 *   /path/to/other.php:10
 *
 *   FAILURES!
 *   Tests: 10, Assertions: 20, Failures: 1.
 *
 * Returns an array of raw blocks with 'header' and 'body_lines' keys.
 * The orchestrator handles converting blocks into TestFailure structures.
 */

require_once __DIR__ . '/classify-error.php';

/**
 * Parse standard PHPUnit output for failure blocks.
 *
 * @param array $lines Array of output lines.
 * @return array Array of failure blocks, each with 'header' (string) and 'body_lines' (array).
 */
function parse_standard_blocks( array $lines ): array {
	$in_failure_section = false;
	$current_block      = null;
	$blocks             = [];

	for ( $i = 0; $i < count( $lines ); $i++ ) {
		$line = $lines[ $i ];

		// Detect start of failure/error listing sections
		if ( preg_match( '/^There (?:was|were) \d+ (?:error|failure)/i', $line ) ) {
			$in_failure_section = true;
			continue;
		}

		// Detect end markers
		if ( preg_match( '/^(ERRORS!|FAILURES!)/', $line ) ) {
			$in_failure_section = false;
			if ( $current_block !== null ) {
				$blocks[]      = $current_block;
				$current_block = null;
			}
			continue;
		}

		// Summary line ends all blocks
		if ( preg_match( '/^Tests:\s*\d+/', $line ) ) {
			if ( $current_block !== null ) {
				$blocks[]      = $current_block;
				$current_block = null;
			}
			$in_failure_section = false;
			continue;
		}

		if ( ! $in_failure_section ) {
			continue;
		}

		// New numbered block: "N) Namespace\ClassTest::testMethod"
		if ( preg_match( '/^\d+\)\s+(.+)$/', $line, $m ) ) {
			if ( $current_block !== null ) {
				$blocks[] = $current_block;
			}
			$current_block = [
				'header'     => trim( $m[1] ),
				'body_lines' => [],
			];
			continue;
		}

		// Accumulate body lines for current block
		if ( $current_block !== null ) {
			$current_block['body_lines'][] = $line;
		}
	}

	// Don't forget last block
	if ( $current_block !== null ) {
		$blocks[] = $current_block;
	}

	return $blocks;
}
