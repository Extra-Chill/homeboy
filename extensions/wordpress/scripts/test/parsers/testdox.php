<?php
/**
 * PHPUnit testdox output parser.
 *
 * Parses the --testdox inline failure format:
 *
 *   File Abilities (DataMachine\Tests\Unit\Abilities\FileAbilities)
 *    ✔ List files ability registered
 *    ✘ List files rejects both scopes
 *      │
 *      │ Failed asserting that 'Flow step 8-6 not found' contains "Cannot provide both".
 *      │
 *      │ /path/to/tests/FileAbilitiesTest.php:94
 *      │
 *
 * Returns blocks in the same format as the standard parser: array of
 * {'header': 'FQCN::method', 'body_lines': [...]} for the orchestrator.
 */

require_once __DIR__ . '/classify-error.php';

/**
 * Parse testdox-formatted PHPUnit output for failure blocks.
 *
 * @param array $lines Array of output lines.
 * @return array Array of failure blocks, each with 'header' (string) and 'body_lines' (array).
 */
function parse_testdox_blocks( array $lines ): array {
	$blocks             = [];
	$current_class_fqcn = '';
	$current_failure    = null;
	$seen_summary       = false;

	for ( $i = 0; $i < count( $lines ); $i++ ) {
		$line = $lines[ $i ];

		// Stop parsing once we hit any summary/repeat section.
		// PHPUnit 9 testdox prints a "Summary of non-successful tests:" section
		// that repeats all failures. The test runner shell script may also repeat
		// output in its "Error details:" section. We must stop at the first
		// boundary to avoid duplicates.
		if ( $seen_summary ) {
			break;
		}

		// PHPUnit's testdox summary section (repeats failures)
		if ( preg_match( '/^Summary of non-successful tests:/', $line ) ) {
			if ( $current_failure !== null ) {
				$blocks[]        = $current_failure;
				$current_failure = null;
			}
			$seen_summary = true;
			continue;
		}

		// "Time: ..." line marks end of the main test output
		if ( preg_match( '/^Time:\s/', $line ) ) {
			if ( $current_failure !== null ) {
				$blocks[]        = $current_failure;
				$current_failure = null;
			}
			$seen_summary = true;
			continue;
		}

		// FAILURES!/ERRORS! banners
		if ( preg_match( '/^(FAILURES!|ERRORS!)$/', $line ) ) {
			if ( $current_failure !== null ) {
				$blocks[]        = $current_failure;
				$current_failure = null;
			}
			$seen_summary = true;
			continue;
		}

		// Tests: summary line
		if ( preg_match( '/^Tests:\s*\d+/', $line ) ) {
			if ( $current_failure !== null ) {
				$blocks[]        = $current_failure;
				$current_failure = null;
			}
			$seen_summary = true;
			continue;
		}

		// Class header: "ClassName (Namespace\ClassName)"
		// e.g. "File Abilities (DataMachine\Tests\Unit\Abilities\FileAbilities)"
		if ( preg_match( '/^(\S.*?)\s+\(([A-Z][\w\\\\]+)\)\s*$/', $line, $m ) ) {
			if ( $current_failure !== null ) {
				$blocks[]        = $current_failure;
				$current_failure = null;
			}
			$current_class_fqcn = $m[2];
			continue;
		}

		// Failed test line: " ✘ Test method name" (Unicode cross mark)
		if ( preg_match( '/^\s+[\x{2718}\x{2717}x]\s+(.+)$/u', $line, $m ) ) {
			if ( $current_failure !== null ) {
				$blocks[] = $current_failure;
			}

			$test_label = trim( $m[1] );

			// Convert testdox label to method name:
			// "List files rejects both scopes" → "test_list_files_rejects_both_scopes"
			$method_name = 'test_' . preg_replace( '/\s+/', '_', strtolower( $test_label ) );

			$header = $current_class_fqcn
				? $current_class_fqcn . '::' . $method_name
				: $method_name;

			$current_failure = [
				'header'     => $header,
				'body_lines' => [],
			];
			continue;
		}

		// Accumulate body lines for current failure (lines with │ prefix)
		if ( $current_failure !== null ) {
			if ( preg_match( '/^\s+\x{2502}\s?(.*)$/u', $line, $m ) ) {
				$current_failure['body_lines'][] = $m[1];
				continue;
			}

			// Passing test (✔) or new class header ends current failure
			if ( preg_match( '/^\s+[\x{2714}\x{2713}]/u', $line ) ||
				preg_match( '/^(\S.*?)\s+\(([A-Z][\w\\\\]+)\)\s*$/', $line ) ) {
				$blocks[]        = $current_failure;
				$current_failure = null;
				// Re-process this line
				$i--;
				continue;
			}
		}
	}

	// Don't forget last failure
	if ( $current_failure !== null ) {
		$blocks[] = $current_failure;
	}

	return $blocks;
}
