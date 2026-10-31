<?php
/**
 * Lightweight smoke coverage for the WordPress DB query profiler helper.
 */

require_once __DIR__ . '/lib/wordpress-db-query-profiler.php';

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

// phpcs:ignore WordPress.WP.GlobalVariablesOverride.Prohibited -- The smoke uses a fake wpdb outside WordPress.
$GLOBALS['wpdb'] = new class() {
	public string $prefix   = 'wp_';
	public string $posts    = 'wp_posts';
	public string $postmeta = 'wp_postmeta';
	public string $options  = 'wp_options';
	public int $num_queries = 0;

	public function prepare( string $query, ...$args ): string {
		return vsprintf( str_replace( '%s', "'%s'", $query ), $args );
	}

	public function get_var( string $query ) {
		unset( $query );
		++$this->num_queries;
		return 'wp_posts';
	}
};

homeboy_wordpress_bench_query_profiler_start( 'smoke', array( 'top' => 10 ) );

$queries = array(
	"SELECT option_value FROM wp_options WHERE option_name = '_transient_smoke' LIMIT 1",
	"SELECT meta_id FROM wp_postmeta WHERE post_id = 123 AND meta_key = '_smoke_key' LIMIT 1",
	"INSERT INTO wp_postmeta (post_id, meta_key, meta_value) VALUES (123, '_smoke_key', 'one')",
	"UPDATE wp_postmeta SET meta_value = 'two' WHERE post_id = 123 AND meta_key = '_smoke_key'",
	"SELECT ID FROM wp_posts WHERE post_name = 'smoke' LIMIT 1",
);

foreach ( $queries as $query ) {
	++$GLOBALS['wpdb']->num_queries;
	homeboy_wordpress_bench_query_profiler_record_query( $query );
}

$profile = homeboy_wordpress_bench_query_profiler_stop();

$assert = static function ( bool $condition, string $message ) use ( $profile ): void {
	if ( $condition ) {
		return;
	}
	error_log( "ERROR: {$message}\n" . wp_json_encode( $profile, JSON_PRETTY_PRINT ) ); // phpcs:ignore WordPress.PHP.DevelopmentFunctions.error_log_error_log -- CLI smoke assertion output.
	exit( 1 );
};

$assert( 'smoke' === $profile['label'], 'profile label was not preserved' );
$assert( 5 === (int) $profile['query_count'], 'query_count did not match recorded queries' );
$assert( 3 === homeboy_wordpress_bench_query_profiler_metric( $profile, 'tables', 'postmeta' ), 'postmeta table count missing' );
$assert( 1 === homeboy_wordpress_bench_query_profiler_metric( $profile, 'categories', 'transient_option' ), 'transient category missing' );
$assert( 1 === homeboy_wordpress_bench_query_profiler_metric( $profile, 'meta_key_operations', 'exists:_smoke_key' ), 'meta exists operation missing' );
$assert( 1 === homeboy_wordpress_bench_query_profiler_metric( $profile, 'meta_key_operations', 'insert:_smoke_key' ), 'meta insert operation missing' );
$assert( 1 === homeboy_wordpress_bench_query_profiler_metric( $profile, 'meta_key_operations', 'update:_smoke_key' ), 'meta update operation missing' );

echo "WordPress DB query profiler smoke passed\n";
