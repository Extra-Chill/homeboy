#!/usr/bin/env php
<?php
/**
 * Minimal regression checks for WordPress PHP fixers.
 */

$repo_root = dirname( __DIR__, 2 );
$fixer_dir = $repo_root . '/scripts/lint/php-fixers';

$tmp_base = sys_get_temp_dir() . '/homeboy-ext-fixer-regression-' . getmypid() . '-' . uniqid();
if ( ! mkdir( $tmp_base, 0777, true ) && ! is_dir( $tmp_base ) ) {
	fwrite( STDERR, "Failed to create temp dir: {$tmp_base}\n" );
	exit( 1 );
}

register_shutdown_function(
	static function () use ( $tmp_base ) {
		delete_tree( $tmp_base );
	}
);

$failures = [];

// -------------------------------------------------------------------------
// WP filesystem fixer should skip special stream/wrapper literals.
// -------------------------------------------------------------------------
$stream_fixture = <<<'PHP'
<?php
function read_input() {
	$input = file_get_contents( 'php://stdin' );
	$file  = file_get_contents( $path );
	return [ $input, $file ];
}
PHP;

$stream_file = $tmp_base . '/stream-fixture.php';
file_put_contents( $stream_file, $stream_fixture );

$filesystem_command = sprintf(
	'php %s %s',
	escapeshellarg( $fixer_dir . '/wp-filesystem-fixer.php' ),
	escapeshellarg( $stream_file )
);
exec( $filesystem_command, $filesystem_output, $filesystem_exit );

if ( 0 !== $filesystem_exit ) {
	$failures[] = 'wp-filesystem-fixer exited non-zero: ' . implode( "\n", $filesystem_output );
} else {
	$stream_result = file_get_contents( $stream_file );
	if ( false === $stream_result ) {
		$failures[] = 'Failed to read wp-filesystem regression fixture';
	} else {
		if ( false === strpos( $stream_result, "file_get_contents( 'php://stdin' )" ) ) {
			$failures[] = 'wp-filesystem-fixer rewrote php://stdin stream literal';
		}
		if ( false === strpos( $stream_result, '->get_contents( $path )' ) ) {
			$failures[] = 'wp-filesystem-fixer failed to rewrite normal filesystem read';
		}
	}
}

// -------------------------------------------------------------------------
// Unused param override detection should be scoped to the actual class.
// -------------------------------------------------------------------------
$unused_fixture = <<<'PHP'
<?php
class BaseController extends FrameworkBase {
	public function inherited( $request ) {
		return true;
	}
}

class PlainController {
	public function local_handler( $request ) {
		return true;
	}
}
PHP;

$unused_file = $tmp_base . '/unused-param-fixture.php';
file_put_contents( $unused_file, $unused_fixture );

$unused_command = sprintf(
	'php %s %s --phpcs-binary=%s --phpcs-standard=%s',
	escapeshellarg( $fixer_dir . '/unused-param-fixer.php' ),
	escapeshellarg( $unused_file ),
	escapeshellarg( $repo_root . '/vendor/bin/phpcs' ),
	escapeshellarg( $repo_root . '/phpcs.xml.dist' )
);
exec( $unused_command, $unused_output, $unused_exit );

if ( 0 !== $unused_exit ) {
	$failures[] = 'unused-param-fixer exited non-zero: ' . implode( "\n", $unused_output );
} else {
	$unused_result = file_get_contents( $unused_file );
	if ( false === $unused_result ) {
		$failures[] = 'Failed to read unused-param regression fixture';
	} else {
		if ( false === strpos( $unused_result, 'public function inherited( $request )' ) ) {
			$failures[] = 'Expected inherited() override parameter to remain in place';
		}
		if ( false !== strpos( $unused_result, 'public function local_handler( $request )' ) ) {
			$failures[] = 'Expected local_handler() param to be removed in non-extending class';
		}
	}
}

if ( ! empty( $failures ) ) {
	fwrite( STDERR, "PHP fixer regression check failed:\n" );
	foreach ( $failures as $failure ) {
		fwrite( STDERR, ' - ' . $failure . "\n" );
	}
	exit( 1 );
}

fwrite( STDOUT, "PHP fixer regression checks passed\n" );
exit( 0 );

/**
 * Recursively delete a directory tree.
 *
 * @param string $path Directory path.
 * @return void
 */
function delete_tree( $path ) {
	if ( ! is_dir( $path ) ) {
		return;
	}

	$items = scandir( $path );
	if ( false === $items ) {
		return;
	}

	foreach ( $items as $item ) {
		if ( '.' === $item || '..' === $item ) {
			continue;
		}

		$child = $path . DIRECTORY_SEPARATOR . $item;
		if ( is_dir( $child ) ) {
			delete_tree( $child );
		} else {
			@unlink( $child );
		}
	}

	@rmdir( $path );
}
