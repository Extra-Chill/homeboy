<?php
/** Smoke test for generic WordPress fuzz coverage helpers. */

if ( ! function_exists( 'add_filter' ) ) {
	function add_filter( $hook_name, $callback, $priority = 10, $accepted_args = 1 ) {
		unset( $hook_name, $callback, $priority, $accepted_args );
		return true;
	}
}

if ( ! function_exists( 'remove_filter' ) ) {
	function remove_filter( $hook_name, $callback, $priority = 10 ) {
		unset( $hook_name, $callback, $priority );
		return true;
	}
}

if ( ! function_exists( 'current_filter' ) ) {
	function current_filter() {
		return $GLOBALS['homeboy_wordpress_bench_coverage_smoke_current_hook'] ?? '';
	}
}

if ( ! function_exists( 'esc_sql' ) ) {
	function esc_sql( $value ) {
		return str_replace( '`', '', (string) $value );
	}
}

if ( ! function_exists( 'wp_json_encode' ) ) {
	function wp_json_encode( $value, $flags = 0 ) {
		return json_encode( $value, $flags ); // phpcs:ignore WordPress.WP.AlternativeFunctions.json_encode_json_encode -- WordPress is stubbed in this smoke.
	}
}

class Homeboy_WordPress_Bench_Coverage_WPDB {
	public string $prefix   = 'wp_';
	public string $posts    = 'wp_posts';
	public string $postmeta = 'wp_postmeta';
	public string $options  = 'wp_options';
	public int $num_queries = 0;

	/** @var array<string,int> */
	public array $counts = array(
		'wp_posts'    => 2,
		'wp_postmeta' => 4,
		'wp_options'  => 8,
	);

	public function prepare( string $query, ...$args ): string {
		foreach ( $args as $arg ) {
			$query = preg_replace( '/%s|%d/', "'" . (string) $arg . "'", $query, 1 );
		}

		return $query;
	}

	public function get_var( string $query ) {
		++$this->num_queries;
		if ( preg_match( "/SHOW TABLES LIKE '([^']+)'/", $query, $match ) ) {
			return array_key_exists( $match[1], $this->counts ) ? $match[1] : null;
		}
		if ( preg_match( '/SELECT COUNT\(\*\) FROM `([^`]+)`/', $query, $match ) ) {
			return (string) ( $this->counts[ $match[1] ] ?? 0 );
		}

		return null;
	}
}

function homeboy_wordpress_bench_coverage_smoke_fire_action( string $hook ): void {
	$GLOBALS['wp_actions'][ $hook ] = (int) ( $GLOBALS['wp_actions'][ $hook ] ?? 0 ) + 1;
	$GLOBALS['homeboy_wordpress_bench_coverage_smoke_current_hook'] = $hook;
	homeboy_wordpress_bench_coverage_record_hook();
	$GLOBALS['homeboy_wordpress_bench_coverage_smoke_current_hook'] = '';
}

function homeboy_wordpress_bench_coverage_smoke_apply_filter( string $hook ): void {
	$GLOBALS['homeboy_wordpress_bench_coverage_smoke_current_hook'] = $hook;
	homeboy_wordpress_bench_coverage_record_hook();
	$GLOBALS['homeboy_wordpress_bench_coverage_smoke_current_hook'] = '';
}

$GLOBALS['wpdb']      = new Homeboy_WordPress_Bench_Coverage_WPDB();
$GLOBALS['wp_actions'] = array( 'init' => 3 );

set_error_handler(
	static function () {
		return true;
	}
);

require_once __DIR__ . '/../scripts/bench/lib/wordpress-fuzz-coverage.php';

$profiled = homeboy_wordpress_bench_coverage_profile_call(
	'smoke-fuzz-batch',
	static function () {
		homeboy_wordpress_bench_coverage_smoke_fire_action( 'init' );
		homeboy_wordpress_bench_coverage_smoke_fire_action( 'save_post' );
		homeboy_wordpress_bench_coverage_smoke_apply_filter( 'the_content' );
		homeboy_wordpress_bench_coverage_smoke_apply_filter( 'the_content' );

		$queries = array(
			"SELECT ID FROM wp_posts WHERE post_name = 'sample' LIMIT 1",
			"INSERT INTO wp_posts (post_title, post_status) VALUES ('Sample', 'draft')",
			"UPDATE wp_options SET option_value = '1' WHERE option_name = 'sample_option'",
		);

		foreach ( $queries as $query ) {
			++$GLOBALS['wpdb']->num_queries;
			homeboy_wordpress_bench_query_profiler_record_query( $query );
		}

		$GLOBALS['wpdb']->counts['wp_posts'] = 3;
		trigger_error( 'coverage smoke notice', E_USER_NOTICE );

		return 'workload-result';
	},
	array(
		'top'             => 10,
		'mutation_tables' => array(
			'posts'   => 'wp_posts',
			'options' => 'wp_options',
		),
	)
);

restore_error_handler();

$coverage = $profiled['coverage'];

$assert = static function ( bool $condition, string $message ) use ( $coverage ): void {
	if ( $condition ) {
		return;
	}
	fwrite( STDERR, "ERROR: {$message}\n" . wp_json_encode( $coverage, JSON_PRETTY_PRINT ) . "\n" );
	exit( 1 );
};

$assert( 'workload-result' === $profiled['result'], 'profile_call did not return workload result' );
$assert( 'homeboy/wordpress-fuzz-coverage/v1' === $coverage['schema'], 'coverage schema missing' );
$assert( 1 === (int) $coverage['hooks']['actions']['init'], 'init action delta missing' );
$assert( 1 === (int) $coverage['hooks']['actions']['save_post'], 'save_post action missing' );
$assert( 2 === (int) $coverage['hooks']['filters']['the_content'], 'the_content inferred filter count missing' );
$assert( 3 === (int) $coverage['db']['query_count'], 'query count missing' );
$assert( 1 === (int) $coverage['db']['operations']['insert'], 'insert operation missing' );
$assert( 1 === (int) $coverage['mutations']['table_row_deltas']['posts'], 'posts row delta missing' );
$assert( 1 === (int) $coverage['mutations']['write_operations']['update'], 'update write operation missing' );
$assert( 1 === (int) $coverage['php_errors']['total'], 'PHP notice summary missing' );
$assert( 1 === (int) $coverage['php_errors']['by_kind']['user_notice'], 'PHP notice kind missing' );
$assert( ! empty( $coverage['coverage_gaps'] ), 'coverage gaps should be explicit' );

echo "wordpress fuzz coverage helper smoke passed\n";
