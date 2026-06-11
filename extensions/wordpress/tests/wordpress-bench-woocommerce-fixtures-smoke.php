<?php
/** Smoke test for reusable WooCommerce bench fixture profiles. */

require_once __DIR__ . '/../scripts/bench/lib/woocommerce-fixtures.php';

$assert = static function ( bool $condition, string $message ): void {
	if ( $condition ) {
		return;
	}

	fwrite( STDERR, $message . "\n" );
	exit( 1 );
};

$small = homeboy_wordpress_bench_wc_fixture_profile_defaults( 'small-shortcode-checkout' );
$assert( 'small-shortcode-checkout' === $small['profile'], 'Expected small checkout profile id.' );
$assert( false === $small['hpos'], 'Expected small checkout profile to disable HPOS.' );
$assert( 'shortcode' === $small['checkout'], 'Expected small checkout profile to use shortcode checkout.' );
$assert( 150 === $small['product_count'], 'Expected small checkout product count.' );
$assert( 125 === $small['variable_product_count'] * $small['variations_per_product'], 'Expected small checkout variation shape.' );

$shipping = homeboy_wordpress_bench_wc_fixture_profile_defaults( 'shipping-package-matrix' );
$assert( 1.0 === $shipping['physical_product_ratio'], 'Expected shipping matrix profile to use physical products.' );
$assert( in_array( 'local_pickup', $shipping['shipping_methods_per_zone'], true ), 'Expected shipping matrix local pickup method.' );

$payload = homeboy_wordpress_bench_wc_apply_fixture_profile(
	'small-shortcode-checkout',
	array(
		'run_id'                 => 'Issue 1250 Smoke Run',
		'product_count'          => 0,
		'variable_product_count' => 0,
		'customer_count'         => 0,
		'guest_order_count'      => 0,
	)
);

$fixture = $payload['metadata']['woocommerce_fixture'] ?? array();
$assert( 0 === $payload['metrics']['woocommerce_available'], 'Expected unavailable WooCommerce metric outside Woo runtime.' );
$assert( 1 === $payload['metrics']['fixture_failures'], 'Expected one unavailable WooCommerce failure.' );
$assert( 'homeboy/wordpress-bench-woocommerce-fixture/v1' === $fixture['schema'], 'Expected fixture metadata schema.' );
$assert( 'issue-1250-smoke-run' === $fixture['run_id'], 'Expected normalized run id.' );
$assert( str_starts_with( $fixture['prefix'], 'hb-' ), 'Expected deterministic fixture prefix.' );

$rig_workload = static function (): array {
	return homeboy_wordpress_bench_wc_apply_fixture_profile(
		'shipping-package-matrix',
		array(
			'run_id'                => 'rig-consumer',
			'product_count'         => 0,
			'customer_count'        => 0,
			'guest_order_count'     => 0,
			'shipping_zone_count'   => 0,
			'package_count'         => 2,
			'items_per_package'     => 3,
			'fixture_consumer_note' => 'rig workload can consume helper payloads directly',
		)
	);
};

$workload_payload = $rig_workload();
$assert( isset( $workload_payload['metrics'], $workload_payload['metadata']['woocommerce_fixture'] ), 'Expected rig workload payload shape.' );
$assert( 'shipping-package-matrix' === $workload_payload['metadata']['woocommerce_fixture']['profile'], 'Expected rig workload profile metadata.' );
$assert( 2 === $workload_payload['metadata']['woocommerce_fixture']['config']['package_count'], 'Expected rig workload overrides to be preserved.' );

echo "wordpress bench WooCommerce fixtures smoke passed\n";
