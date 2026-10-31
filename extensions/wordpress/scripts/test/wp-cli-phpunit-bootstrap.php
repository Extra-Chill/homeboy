<?php
/**
 * Load the WP-CLI API for managed WordPress PHPUnit suites.
 *
 * WP-CLI's dispatcher entrypoint is intentionally excluded: tests need its
 * public API, not command parsing or process termination behavior.
 */

$vendor_dir  = '/wp-codebox-vendor';
$wp_cli_root = $vendor_dir . '/wp-cli/wp-cli';

require_once $vendor_dir . '/autoload.php';

if ( ! defined( 'WP_CLI_ROOT' ) ) {
	define( 'WP_CLI_ROOT', $wp_cli_root );
}

if ( ! defined( 'WP_CLI_VENDOR_DIR' ) ) {
	define( 'WP_CLI_VENDOR_DIR', $vendor_dir );
}

foreach ( array( 'utils.php', 'dispatcher.php', 'utils-wp.php' ) as $file ) {
	require_once $wp_cli_root . '/php/' . $file;
}
