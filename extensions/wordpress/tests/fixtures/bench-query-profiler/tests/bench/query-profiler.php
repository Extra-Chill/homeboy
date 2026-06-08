<?php
/** Workload that exercises the generic WordPress DB query profiler helper. */
require_once '/homeboy-extension/scripts/bench/lib/wordpress-db-query-profiler.php';

return function (): array {
	global $wpdb;

	$run_id = 'homeboy-query-profiler-' . getmypid() . '-' . time();
	$tables_before = homeboy_wordpress_bench_query_profiler_table_counts(
		array(
			'posts'    => $wpdb->posts,
			'postmeta' => $wpdb->postmeta,
			'options'  => $wpdb->options,
		)
	);

	$profiled = homeboy_wordpress_bench_query_profiler_profile_call(
		'wp-core-write-read',
		static function () use ( $run_id ): array {
			update_option( $run_id . '_option', 'one', false );
			set_transient( $run_id . '_transient', 'two', 60 );

			$post_id = wp_insert_post(
				array(
					'post_title'   => 'Homeboy Query Profiler ' . $run_id,
					'post_name'    => $run_id,
					'post_status'  => 'publish',
					'post_type'    => 'post',
					'post_content' => 'Query profiler fixture.',
				),
				true
			);
			if ( is_wp_error( $post_id ) ) {
				throw new RuntimeException( 'Failed to create profiler fixture post: ' . $post_id->get_error_message() );
			}

			add_post_meta( (int) $post_id, '_homeboy_profiler_key', 'first', true );
			update_post_meta( (int) $post_id, '_homeboy_profiler_key', 'second' );
			get_post_meta( (int) $post_id, '_homeboy_profiler_key', true );

			return array( 'post_id' => (int) $post_id );
		},
		array( 'top' => 50 )
	);

	$profile      = $profiled['profile'];
	$tables_after = homeboy_wordpress_bench_query_profiler_table_counts(
		array(
			'posts'    => $wpdb->posts,
			'postmeta' => $wpdb->postmeta,
			'options'  => $wpdb->options,
		)
	);
	$deltas = homeboy_wordpress_bench_query_profiler_count_deltas( $tables_before, $tables_after );

	$invariant_failures = array_values(
		array_filter(
			array(
				homeboy_wordpress_bench_query_profiler_invariant( 'profile_saw_queries', (int) $profile['query_count'] > 0 ),
				homeboy_wordpress_bench_query_profiler_invariant( 'post_row_created', (int) ( $deltas['posts'] ?? 0 ) >= 1, array( 'deltas' => $deltas ) ),
				homeboy_wordpress_bench_query_profiler_invariant( 'postmeta_seen', homeboy_wordpress_bench_query_profiler_metric( $profile, 'tables', 'postmeta' ) > 0, array( 'tables' => $profile['tables'] ?? array() ) ),
			)
		)
	);

	return array(
		'metrics'  => array(
			'query_count'             => (int) $profile['query_count'],
			'operation_select_queries'=> homeboy_wordpress_bench_query_profiler_metric( $profile, 'operations', 'select' ),
			'postmeta_queries'        => homeboy_wordpress_bench_query_profiler_metric( $profile, 'tables', 'postmeta' ),
			'option_queries'          => homeboy_wordpress_bench_query_profiler_metric( $profile, 'tables', 'options' ),
			'transient_option_queries'=> homeboy_wordpress_bench_query_profiler_metric( $profile, 'categories', 'transient_option' ),
			'invariant_failure_count' => count( $invariant_failures ),
		),
		'metadata' => array(
			'profile'            => $profile,
			'table_count_deltas' => $deltas,
			'invariant_failures' => $invariant_failures,
		),
	);
};
