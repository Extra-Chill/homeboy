<?php
/**
 * Unit test for HomeboyWordPress.Commenting.MultiLineInlineComment.
 *
 * Drives PHPCS as a subprocess against the .inc fixture, asserts the JSON
 * report shape, then runs phpcbf and diffs the output against the .fixed
 * fixture.
 *
 * Why a subprocess test instead of `AbstractSniffUnitTest`:
 * the repo's PHPUnit harness runs inside WordPress Playground (PHP-WASM) where
 * `proc_open` is available but PHPCS' bundled test bootstrap is not loaded.
 * Driving the installed `vendor/bin/phpcs` binary gives identical fidelity
 * with much less coupling.
 *
 * @package HomeboyWordPress
 */

declare( strict_types=1 );

namespace HomeboyWordPress\Tests\Commenting;

use PHPUnit\Framework\TestCase;

/**
 * @covers \HomeboyWordPress\Sniffs\Commenting\MultiLineInlineCommentSniff
 */
final class MultiLineInlineCommentUnitTest extends TestCase {

	/**
	 * Path to the fixture under test.
	 *
	 * @var string
	 */
	private $fixture;

	/**
	 * Path to the expected post-fixer output.
	 *
	 * @var string
	 */
	private $fixtureFixed;

	/**
	 * Path to the phpcs binary in the extension's vendor dir.
	 *
	 * @var string
	 */
	private $phpcsBin;

	/**
	 * Path to the phpcbf binary in the extension's vendor dir.
	 *
	 * @var string
	 */
	private $phpcbfBin;

	/**
	 * Path to the standard's ruleset.
	 *
	 * @var string
	 */
	private $standardPath;

	protected function setUp(): void {
		// Tests/Commenting -> Tests -> HomeboyWordPress -> wordpress.
		$root               = dirname( __DIR__, 3 );
		$standardRoot       = dirname( __DIR__, 2 );
		$this->fixture      = __DIR__ . '/MultiLineInlineCommentUnitTest.inc';
		$this->fixtureFixed = __DIR__ . '/MultiLineInlineCommentUnitTest.inc.fixed';
		$this->phpcsBin     = $root . '/vendor/bin/phpcs';
		$this->phpcbfBin    = $root . '/vendor/bin/phpcbf';
		$this->standardPath = $standardRoot;

		if ( ! is_executable( $this->phpcsBin ) ) {
			$this->markTestSkipped( 'phpcs binary not found at ' . $this->phpcsBin );
		}
	}

	/**
	 * Detection A — both runs (2-line and 4-line) report on their first line,
	 * with one error per run. Detection B — three findings inside the function
	 * body. No findings on annotation lines, single `//`, EOL trailing
	 * comments, or the legitimate function DocBlock.
	 */
	public function test_violation_lines_match_expectations(): void {
		$report = $this->runPhpcsJson( $this->fixture );

		$findings = $this->extractFindings( $report );

		$expected = array(
			26 => 'ConsecutiveSingleLine',
			31 => 'ConsecutiveSingleLine',
			47 => 'DoubleAsteriskNonDocBlock',
			50 => 'DoubleAsteriskNonDocBlock',
		);

		$actualLines = array_keys( $findings );
		sort( $actualLines );
		$expectedLines = array_keys( $expected );
		sort( $expectedLines );

		$this->assertSame(
			$expectedLines,
			$actualLines,
			'Violation lines do not match. Got: ' . json_encode( $findings )
		);

		foreach ( $expected as $line => $code ) {
			$this->assertStringContainsString(
				$code,
				$findings[ $line ],
				sprintf( 'Line %d should report %s, got %s', $line, $code, $findings[ $line ] )
			);
		}
	}

	/**
	 * Auto-fixer (phpcbf) must produce byte-identical output to the
	 * `.inc.fixed` fixture.
	 */
	public function test_fixer_produces_expected_output(): void {
		// `tempnam` creates a file with no extension, but PHPCS filters by
		// extension (`--extensions=inc,php`). Rename to `.php` so the file is
		// scanned regardless of extension overrides.
		$rawTmp = tempnam( sys_get_temp_dir(), 'mlinline-fix-' );
		$tmp    = $rawTmp . '.php';
		rename( $rawTmp, $tmp );
		copy( $this->fixture, $tmp );

		// Force `inc` extension so the file is scanned even when a project
		// `phpcs.xml.dist` restricts the default to `.php`.
		$cmd = sprintf(
			'%s --standard=%s --sniffs=HomeboyWordPress.Commenting.MultiLineInlineComment --extensions=inc,php %s 2>&1',
			escapeshellarg( $this->phpcbfBin ),
			escapeshellarg( $this->standardPath ),
			escapeshellarg( $tmp )
		);

		// phpcbf exit codes: 0 = clean (or `--no-cache` reported as fixed), 1
		// = files fixed (the expected case), 2+ = config/runtime error.
		exec( $cmd, $out, $rc );

		$this->assertLessThan(
			2,
			$rc,
			"phpcbf failed (rc=$rc):\n" . implode( "\n", $out ) . "\nCMD: $cmd"
		);

		$got      = file_get_contents( $tmp );
		$expected = file_get_contents( $this->fixtureFixed );

		unlink( $tmp );

		$this->assertSame(
			$expected,
			$got,
			"Fixer output differs from .inc.fixed fixture.\nCMD: $cmd\nphpcbf rc=$rc\nphpcbf output:\n" . implode( "\n", $out )
		);
	}

	/**
	 * @return array<int,array<string,mixed>>
	 */
	private function runPhpcsJson( string $file ): array {
		$cmd = sprintf(
			'%s --standard=%s --sniffs=HomeboyWordPress.Commenting.MultiLineInlineComment --extensions=inc,php --report=json %s',
			escapeshellarg( $this->phpcsBin ),
			escapeshellarg( $this->standardPath ),
			escapeshellarg( $file )
		);

		exec( $cmd, $out, $rc );
		// phpcs exit codes: 0 = clean, 1 = warnings, 2 = errors found (the
		// expected case for this fixture). 3 = config/runtime error.
		$this->assertLessThan( 3, $rc, "phpcs failed (rc=$rc): \n" . implode( "\n", $out ) );

		$json = json_decode( implode( "\n", $out ), true );
		$this->assertIsArray( $json, 'phpcs --report=json returned non-JSON output: ' . implode( "\n", $out ) );

		return $json;
	}

	/**
	 * Flatten phpcs JSON report into [ line => "Source: message" ].
	 *
	 * @param array<string,mixed> $report
	 *
	 * @return array<int,string>
	 */
	private function extractFindings( array $report ): array {
		$out = array();
		foreach ( $report['files'] ?? array() as $file ) {
			foreach ( $file['messages'] ?? array() as $message ) {
				$out[ (int) $message['line'] ] = $message['source'] . ': ' . $message['message'];
			}
		}
		ksort( $out );
		return $out;
	}
}
