<?php
/**
 * Generic WordPress fuzz/workload coverage instrumentation helpers.
 *
 * Workloads can require this file from the mounted extension path:
 * /homeboy-extension/scripts/bench/lib/wordpress-fuzz-coverage.php
 */

require_once __DIR__ . '/wordpress-db-query-profiler.php';

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surfaces' ) ) {
	/**
	 * Discover bounded generic frontend URL candidates for fuzz planning.
	 *
	 * @param array<string,mixed> $options Discovery options.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_frontend_url_surfaces( array $options = array() ): array {
		$max_total       = homeboy_wordpress_bench_frontend_url_surface_limit( $options['max_total'] ?? 50, 1, 200 );
		$max_per_surface = homeboy_wordpress_bench_frontend_url_surface_limit( $options['max_per_surface'] ?? 10, 1, 50 );
		$state           = array(
			'candidates'  => array(),
			'seen'        => array(),
			'counts'      => array(),
			'skipped'     => array(),
			'max_total'   => $max_total,
			'max_surface' => $max_per_surface,
		);

		homeboy_wordpress_bench_frontend_url_surface_home_candidates( $state );
		homeboy_wordpress_bench_frontend_url_surface_front_candidates( $state );
		homeboy_wordpress_bench_frontend_url_surface_archive_candidates( $state, $options );
		homeboy_wordpress_bench_frontend_url_surface_sitemap_candidates( $state );
		homeboy_wordpress_bench_frontend_url_surface_permalink_candidates( $state, $options );

		return array(
			'schema'               => 'homeboy/wordpress-frontend-url-surfaces/v1',
			'generated_at_unix_ms' => (int) floor( microtime( true ) * 1000 ),
			'limits'               => array(
				'max_total'       => $max_total,
				'max_per_surface' => $max_per_surface,
			),
			'candidates'           => $state['candidates'],
			'counts'               => $state['counts'],
			'skipped'              => $state['skipped'],
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_home_candidates' ) ) {
	/** @param array<string,mixed> $state Discovery state. */
	function homeboy_wordpress_bench_frontend_url_surface_home_candidates( array &$state ): void {
		if ( ! function_exists( 'home_url' ) ) {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, 'home', 'missing_home_url' );
			return;
		}

		homeboy_wordpress_bench_frontend_url_surface_add(
			$state,
			array(
				'surface' => 'home',
				'url'     => home_url( '/' ),
				'source'  => 'home_url',
				'label'   => 'Site home',
			)
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_front_candidates' ) ) {
	/** @param array<string,mixed> $state Discovery state. */
	function homeboy_wordpress_bench_frontend_url_surface_front_candidates( array &$state ): void {
		if ( ! function_exists( 'get_option' ) ) {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, 'front', 'missing_get_option' );
			return;
		}

		$show_on_front = (string) get_option( 'show_on_front', 'posts' );
		$page_on_front = (int) get_option( 'page_on_front', 0 );
		if ( 'page' === $show_on_front && $page_on_front > 0 && function_exists( 'get_permalink' ) ) {
			homeboy_wordpress_bench_frontend_url_surface_add(
				$state,
				array(
					'surface'        => 'front',
					'url'            => get_permalink( $page_on_front ),
					'source'         => 'page_on_front',
					'label'          => 'Static front page',
					'object_type'    => 'post',
					'object_subtype' => 'page',
					'object_id'      => $page_on_front,
				)
			);
			return;
		}

		if ( function_exists( 'home_url' ) ) {
			homeboy_wordpress_bench_frontend_url_surface_add(
				$state,
				array(
					'surface' => 'front',
					'url'     => home_url( '/' ),
					'source'  => 'show_on_front:' . $show_on_front,
					'label'   => 'Posts front page',
				)
			);
			return;
		}

		homeboy_wordpress_bench_frontend_url_surface_skip( $state, 'front', 'missing_front_url' );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_archive_candidates' ) ) {
	/**
	 * @param array<string,mixed> $state Discovery state.
	 * @param array<string,mixed> $options Discovery options.
	 */
	function homeboy_wordpress_bench_frontend_url_surface_archive_candidates( array &$state, array $options ): void {
		if ( function_exists( 'get_post_type_archive_link' ) ) {
			$post_types = homeboy_wordpress_bench_frontend_url_surface_post_types( $options );
			foreach ( $post_types as $post_type ) {
				$link = get_post_type_archive_link( $post_type );
				if ( $link ) {
					homeboy_wordpress_bench_frontend_url_surface_add(
						$state,
						array(
							'surface'        => 'archive',
							'url'            => $link,
							'source'         => 'post_type_archive',
							'label'          => $post_type . ' archive',
							'object_type'    => 'post_type',
							'object_subtype' => $post_type,
						)
					);
				}
			}
		} else {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, 'archive', 'missing_get_post_type_archive_link' );
		}

		if ( function_exists( 'get_terms' ) && function_exists( 'get_term_link' ) ) {
			$taxonomies = homeboy_wordpress_bench_frontend_url_surface_taxonomies( $options );
			foreach ( $taxonomies as $taxonomy ) {
				$terms = get_terms(
					array(
						'taxonomy'   => $taxonomy,
						'hide_empty' => true,
						'number'     => (int) $state['max_surface'],
					)
				);
				if ( homeboy_wordpress_bench_frontend_url_surface_is_wp_error( $terms ) || ! is_array( $terms ) ) {
					continue;
				}
				foreach ( $terms as $term ) {
					$link = get_term_link( $term );
					if ( homeboy_wordpress_bench_frontend_url_surface_is_wp_error( $link ) || ! $link ) {
						continue;
					}
					homeboy_wordpress_bench_frontend_url_surface_add(
						$state,
						array(
							'surface'        => 'archive',
							'url'            => $link,
							'source'         => 'term_archive',
							'label'          => isset( $term->name ) ? (string) $term->name : $taxonomy . ' term',
							'object_type'    => 'term',
							'object_subtype' => $taxonomy,
							'object_id'      => isset( $term->term_id ) ? (int) $term->term_id : null,
						)
					);
				}
			}
		} else {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, 'archive', 'missing_term_archive_functions' );
		}
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_sitemap_candidates' ) ) {
	/** @param array<string,mixed> $state Discovery state. */
	function homeboy_wordpress_bench_frontend_url_surface_sitemap_candidates( array &$state ): void {
		if ( ! function_exists( 'home_url' ) ) {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, 'sitemap', 'missing_home_url' );
			return;
		}

		foreach ( array( '/wp-sitemap.xml', '/sitemap.xml' ) as $path ) {
			homeboy_wordpress_bench_frontend_url_surface_add(
				$state,
				array(
					'surface' => 'sitemap',
					'url'     => home_url( $path ),
					'source'  => 'conventional_sitemap',
					'label'   => ltrim( $path, '/' ),
				)
			);
		}
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_permalink_candidates' ) ) {
	/**
	 * @param array<string,mixed> $state Discovery state.
	 * @param array<string,mixed> $options Discovery options.
	 */
	function homeboy_wordpress_bench_frontend_url_surface_permalink_candidates( array &$state, array $options ): void {
		if ( ! function_exists( 'get_posts' ) || ! function_exists( 'get_permalink' ) ) {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, 'permalink', 'missing_permalink_functions' );
			return;
		}

		$post_types = homeboy_wordpress_bench_frontend_url_surface_post_types( $options );
		$posts      = get_posts(
			array(
				'post_type'      => empty( $post_types ) ? 'any' : $post_types,
				'post_status'    => 'publish',
				'posts_per_page' => (int) $state['max_surface'],
				'orderby'        => 'date',
				'order'          => 'DESC',
			)
		);

		if ( ! is_array( $posts ) ) {
			return;
		}

		foreach ( $posts as $post ) {
			$post_id = isset( $post->ID ) ? (int) $post->ID : 0;
			if ( $post_id <= 0 ) {
				continue;
			}
			$link = get_permalink( $post_id );
			if ( ! $link ) {
				continue;
			}
			homeboy_wordpress_bench_frontend_url_surface_add(
				$state,
				array(
					'surface'        => 'permalink',
					'url'            => $link,
					'source'         => 'recent_posts',
					'label'          => isset( $post->post_title ) ? (string) $post->post_title : 'Post ' . $post_id,
					'object_type'    => 'post',
					'object_subtype' => isset( $post->post_type ) ? (string) $post->post_type : '',
					'object_id'      => $post_id,
				)
			);
		}
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_post_types' ) ) {
	/**
	 * @param array<string,mixed> $options Discovery options.
	 * @return array<int,string>
	 */
	function homeboy_wordpress_bench_frontend_url_surface_post_types( array $options ): array {
		if ( isset( $options['post_types'] ) && is_array( $options['post_types'] ) ) {
			return array_values( array_filter( array_map( 'strval', $options['post_types'] ) ) );
		}

		if ( ! function_exists( 'get_post_types' ) ) {
			return array( 'post', 'page' );
		}

		$post_types = get_post_types( array( 'public' => true ), 'names' );
		return is_array( $post_types ) ? array_values( array_filter( array_map( 'strval', $post_types ) ) ) : array();
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_taxonomies' ) ) {
	/**
	 * @param array<string,mixed> $options Discovery options.
	 * @return array<int,string>
	 */
	function homeboy_wordpress_bench_frontend_url_surface_taxonomies( array $options ): array {
		if ( isset( $options['taxonomies'] ) && is_array( $options['taxonomies'] ) ) {
			return array_values( array_filter( array_map( 'strval', $options['taxonomies'] ) ) );
		}

		if ( ! function_exists( 'get_taxonomies' ) ) {
			return array( 'category', 'post_tag' );
		}

		$taxonomies = get_taxonomies( array( 'public' => true ), 'names' );
		return is_array( $taxonomies ) ? array_values( array_filter( array_map( 'strval', $taxonomies ) ) ) : array();
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_add' ) ) {
	/**
	 * @param array<string,mixed> $state Discovery state.
	 * @param array<string,mixed> $candidate Candidate row.
	 */
	function homeboy_wordpress_bench_frontend_url_surface_add( array &$state, array $candidate ): void {
		$surface = isset( $candidate['surface'] ) ? (string) $candidate['surface'] : '';
		$url     = homeboy_wordpress_bench_frontend_url_surface_url( $candidate['url'] ?? '' );
		if ( '' === $surface || '' === $url ) {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, '' === $surface ? 'unknown' : $surface, 'invalid_candidate' );
			return;
		}

		if ( count( $state['candidates'] ) >= (int) $state['max_total'] ) {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, $surface, 'max_total' );
			return;
		}

		$surface_count = (int) ( $state['counts'][ $surface ] ?? 0 );
		if ( $surface_count >= (int) $state['max_surface'] ) {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, $surface, 'max_per_surface' );
			return;
		}

		$key = $surface . ' ' . $url;
		if ( isset( $state['seen'][ $key ] ) ) {
			homeboy_wordpress_bench_frontend_url_surface_skip( $state, $surface, 'duplicate' );
			return;
		}

		$state['seen'][ $key ]       = true;
		$state['counts'][ $surface ] = $surface_count + 1;
		$candidate['url']            = $url;
		$state['candidates'][]       = homeboy_wordpress_bench_frontend_url_surface_compact( $candidate );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_skip' ) ) {
	/** @param array<string,mixed> $state Discovery state. */
	function homeboy_wordpress_bench_frontend_url_surface_skip( array &$state, string $surface, string $reason ): void {
		$key                      = '' === $surface ? $reason : $surface . ':' . $reason;
		$state['skipped'][ $key ] = (int) ( $state['skipped'][ $key ] ?? 0 ) + 1;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_limit' ) ) {
	function homeboy_wordpress_bench_frontend_url_surface_limit( $value, int $minimum, int $maximum ): int {
		$parsed = is_numeric( $value ) ? (int) $value : $minimum;
		return max( $minimum, min( $maximum, $parsed ) );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_is_wp_error' ) ) {
	function homeboy_wordpress_bench_frontend_url_surface_is_wp_error( $value ): bool {
		return function_exists( 'is_wp_error' ) && is_wp_error( $value );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_url' ) ) {
	function homeboy_wordpress_bench_frontend_url_surface_url( $url ): string {
		$url = is_string( $url ) ? trim( $url ) : '';
		if ( '' === $url || ! preg_match( '#^https?://#i', $url ) ) {
			return '';
		}

		return $url;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_frontend_url_surface_compact' ) ) {
	/**
	 * @param array<string,mixed> $candidate Candidate row.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_frontend_url_surface_compact( array $candidate ): array {
		return array_filter(
			$candidate,
			static function ( $value ): bool {
				return null !== $value && '' !== $value && array() !== $value;
			}
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_start' ) ) {
	/**
	 * Start collecting coverage counters for a workload/request batch.
	 *
	 * @param string              $label Coverage label.
	 * @param array<string,mixed> $options Coverage options.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_coverage_start( string $label = '', array $options = array() ): array {
		if ( isset( $GLOBALS['homeboy_wordpress_bench_coverage_state']['active'] ) ) {
			homeboy_wordpress_bench_coverage_stop();
		}

		$state = array(
			'label'                  => $label,
			'options'                => $options,
			'started_at_unix_ms'     => (int) floor( microtime( true ) * 1000 ),
			'started_at'             => microtime( true ),
			'all_hooks'              => array(),
			'actions_before'         => homeboy_wordpress_bench_coverage_wp_actions_snapshot(),
			'table_counts_before'    => homeboy_wordpress_bench_coverage_table_counts( $options ),
			'php_errors'             => array(),
			'previous_error_handler' => null,
		);

		$GLOBALS['homeboy_wordpress_bench_coverage_state'] = array( 'active' => $state );

		if ( function_exists( 'add_filter' ) ) {
			add_filter( 'all', 'homeboy_wordpress_bench_coverage_record_hook', PHP_INT_MIN, 0 );
		}

		$GLOBALS['homeboy_wordpress_bench_coverage_state']['active']['previous_error_handler'] = set_error_handler( 'homeboy_wordpress_bench_coverage_record_php_error' );
		homeboy_wordpress_bench_query_profiler_start( $label, $options );

		return array(
			'schema'             => 'homeboy/wordpress-fuzz-coverage/v1',
			'label'              => $label,
			'active'             => true,
			'started_at_unix_ms' => (int) $GLOBALS['homeboy_wordpress_bench_coverage_state']['active']['started_at_unix_ms'],
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_stop' ) ) {
	/**
	 * Stop collecting coverage counters and return the final snapshot.
	 *
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_coverage_stop(): array {
		$profile = homeboy_wordpress_bench_coverage_snapshot();
		homeboy_wordpress_bench_query_profiler_stop();

		if ( function_exists( 'remove_filter' ) ) {
			remove_filter( 'all', 'homeboy_wordpress_bench_coverage_record_hook', PHP_INT_MIN );
		}

		if ( isset( $GLOBALS['homeboy_wordpress_bench_coverage_state']['active'] ) ) {
			restore_error_handler();
		}

		unset( $GLOBALS['homeboy_wordpress_bench_coverage_state']['active'] );

		return $profile;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_profile_call' ) ) {
	/**
	 * Profile a callable and return its result with coverage evidence.
	 *
	 * @param string              $label Coverage label.
	 * @param callable            $callback Workload callback.
	 * @param array<string,mixed> $options Coverage options.
	 * @return array{result:mixed,coverage:array<string,mixed>}
	 */
	function homeboy_wordpress_bench_coverage_profile_call( string $label, callable $callback, array $options = array() ): array {
		homeboy_wordpress_bench_coverage_start( $label, $options );
		try {
			$result = $callback();
		} finally {
			$coverage = homeboy_wordpress_bench_coverage_stop();
		}

		return array(
			'result'   => $result,
			'coverage' => $coverage,
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_snapshot' ) ) {
	/**
	 * Return the current coverage snapshot with deterministic top-N truncation.
	 *
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_coverage_snapshot(): array {
		$state = $GLOBALS['homeboy_wordpress_bench_coverage_state']['active'] ?? null;
		if ( ! is_array( $state ) ) {
			return array(
				'label'  => '',
				'active' => false,
			);
		}

		$options       = is_array( $state['options'] ?? null ) ? $state['options'] : array();
		$top           = isset( $options['top'] ) && is_numeric( $options['top'] ) ? max( 1, (int) $options['top'] ) : 30;
		$actions_after = homeboy_wordpress_bench_coverage_wp_actions_snapshot();
		$actions       = homeboy_wordpress_bench_coverage_count_deltas( $state['actions_before'], $actions_after );
		$all_hooks     = $state['all_hooks'];
		$filters       = homeboy_wordpress_bench_coverage_infer_filter_counts( $all_hooks, $actions );
		$query_profile = homeboy_wordpress_bench_query_profiler_snapshot();
		$table_after   = homeboy_wordpress_bench_coverage_table_counts( $options );

		return array(
			'schema'             => 'homeboy/wordpress-fuzz-coverage/v1',
			'label'              => (string) $state['label'],
			'active'             => true,
			'started_at_unix_ms' => (int) $state['started_at_unix_ms'],
			'elapsed_ms'         => max( 0, ( microtime( true ) - (float) $state['started_at'] ) * 1000 ),
			'hooks'              => array(
				'all'     => homeboy_wordpress_bench_query_profiler_top_counts( $all_hooks, $top ),
				'actions' => homeboy_wordpress_bench_query_profiler_top_counts( $actions, $top ),
				'filters' => homeboy_wordpress_bench_query_profiler_top_counts( $filters, $top ),
			),
			'db'                 => array(
				'query_count'      => (int) ( $query_profile['query_count'] ?? 0 ),
				'operations'       => $query_profile['operations'] ?? array(),
				'tables'           => $query_profile['tables'] ?? array(),
				'categories'       => $query_profile['categories'] ?? array(),
				'top_query_shapes' => $query_profile['signatures'] ?? array(),
			),
			'mutations'          => homeboy_wordpress_bench_coverage_mutation_summary( $state['table_counts_before'], $table_after, $query_profile ),
			'php_errors'         => homeboy_wordpress_bench_coverage_php_error_summary( $state['php_errors'], $top ),
			'coverage_gaps'      => homeboy_wordpress_bench_coverage_gaps(),
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_record_hook' ) ) {
	/** Record the currently executing hook from WordPress' synthetic all hook. */
	function homeboy_wordpress_bench_coverage_record_hook(): void {
		if ( empty( $GLOBALS['homeboy_wordpress_bench_coverage_state']['active'] ) ) {
			return;
		}

		$hook = function_exists( 'current_filter' ) ? (string) current_filter() : '';
		if ( '' === $hook || 'all' === $hook ) {
			return;
		}

		$state =& $GLOBALS['homeboy_wordpress_bench_coverage_state']['active'];
		homeboy_wordpress_bench_query_profiler_increment( $state['all_hooks'], $hook );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_record_php_error' ) ) {
	/**
	 * Temporary error handler that summarizes non-fatal PHP issues.
	 *
	 * @param int    $severity PHP error severity.
	 * @param string $message Error message.
	 * @param string $file Error file.
	 * @param int    $line Error line.
	 * @return bool
	 */
	function homeboy_wordpress_bench_coverage_record_php_error( int $severity, string $message, string $file = '', int $line = 0 ): bool {
		if ( 0 === ( error_reporting() & $severity ) ) {
			return false;
		}

		if ( ! empty( $GLOBALS['homeboy_wordpress_bench_coverage_state']['active'] ) ) {
			$state =& $GLOBALS['homeboy_wordpress_bench_coverage_state']['active'];
			$kind  = homeboy_wordpress_bench_coverage_error_kind( $severity );
			$key   = $kind . ':' . $message;
			if ( ! isset( $state['php_errors'][ $key ] ) ) {
				$state['php_errors'][ $key ] = array(
					'kind'     => $kind,
					'severity' => $severity,
					'message'  => $message,
					'file'     => $file,
					'line'     => $line,
					'count'    => 0,
				);
			}
			++$state['php_errors'][ $key ]['count'];
		}

		$previous = $GLOBALS['homeboy_wordpress_bench_coverage_state']['active']['previous_error_handler'] ?? null;
		if ( is_callable( $previous ) ) {
			return (bool) $previous( $severity, $message, $file, $line );
		}

		return false;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_wp_actions_snapshot' ) ) {
	/** @return array<string,int> */
	function homeboy_wordpress_bench_coverage_wp_actions_snapshot(): array {
		$actions = $GLOBALS['wp_actions'] ?? array();
		return is_array( $actions ) ? array_map( 'intval', $actions ) : array();
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_count_deltas' ) ) {
	/**
	 * @param array<string,int> $before Earlier counts.
	 * @param array<string,int> $after Later counts.
	 * @return array<string,int>
	 */
	function homeboy_wordpress_bench_coverage_count_deltas( array $before, array $after ): array {
		$deltas = array();
		foreach ( array_unique( array_merge( array_keys( $before ), array_keys( $after ) ) ) as $key ) {
			$delta = (int) ( $after[ $key ] ?? 0 ) - (int) ( $before[ $key ] ?? 0 );
			if ( 0 !== $delta ) {
				$deltas[ $key ] = $delta;
			}
		}

		return $deltas;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_infer_filter_counts' ) ) {
	/**
	 * @param array<string,int> $all_hooks Hooks observed through the all hook.
	 * @param array<string,int> $actions Action deltas from wp_actions.
	 * @return array<string,int>
	 */
	function homeboy_wordpress_bench_coverage_infer_filter_counts( array $all_hooks, array $actions ): array {
		$filters = array();
		foreach ( $all_hooks as $hook => $count ) {
			$filter_count = (int) $count - (int) ( $actions[ $hook ] ?? 0 );
			if ( $filter_count > 0 ) {
				$filters[ $hook ] = $filter_count;
			}
		}

		return $filters;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_table_counts' ) ) {
	/**
	 * @param array<string,mixed> $options Coverage options.
	 * @return array<string,int>
	 */
	function homeboy_wordpress_bench_coverage_table_counts( array $options ): array {
		global $wpdb;

		$tables = $options['mutation_tables'] ?? null;
		if ( null === $tables ) {
			$tables = homeboy_wordpress_bench_coverage_default_tables();
		}

		if ( ! is_array( $tables ) || ! isset( $wpdb ) ) {
			return array();
		}

		$resolved = array();
		foreach ( $tables as $label => $table ) {
			$table_name = is_string( $table ) ? $table : '';
			if ( '' === $table_name ) {
				continue;
			}
			$key              = is_string( $label ) ? $label : homeboy_wordpress_bench_query_profiler_table_key( $table_name );
			$resolved[ $key ] = $table_name;
		}

		return homeboy_wordpress_bench_query_profiler_table_counts( $resolved );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_default_tables' ) ) {
	/** @return array<string,string> */
	function homeboy_wordpress_bench_coverage_default_tables(): array {
		global $wpdb;

		if ( ! isset( $wpdb ) ) {
			return array();
		}

		$properties = array( 'posts', 'postmeta', 'options', 'terms', 'term_taxonomy', 'term_relationships', 'comments', 'commentmeta', 'users', 'usermeta' );
		$tables     = array();
		foreach ( $properties as $property ) {
			if ( isset( $wpdb->{$property} ) && is_string( $wpdb->{$property} ) && '' !== $wpdb->{$property} ) {
				$tables[ $property ] = $wpdb->{$property};
			}
		}

		return $tables;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_mutation_summary' ) ) {
	/**
	 * @param array<string,int>   $before Table counts before workload.
	 * @param array<string,int>   $after Table counts after workload.
	 * @param array<string,mixed> $query_profile Query profile snapshot.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_coverage_mutation_summary( array $before, array $after, array $query_profile ): array {
		$write_operations = array();
		foreach ( array( 'insert', 'update', 'delete', 'replace', 'alter', 'create', 'drop' ) as $operation ) {
			$count = (int) ( $query_profile['operations'][ $operation ] ?? 0 );
			if ( $count > 0 ) {
				$write_operations[ $operation ] = $count;
			}
		}

		return array(
			'table_row_deltas' => homeboy_wordpress_bench_coverage_count_deltas( $before, $after ),
			'write_operations' => $write_operations,
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_php_error_summary' ) ) {
	/**
	 * @param array<string,array<string,mixed>> $errors Recorded errors.
	 * @param int                              $top Max entries.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_coverage_php_error_summary( array $errors, int $top ): array {
		$by_kind = array();
		foreach ( $errors as $error ) {
			$kind = (string) ( $error['kind'] ?? 'unknown' );
			homeboy_wordpress_bench_query_profiler_increment( $by_kind, $kind );
		}

		usort(
			$errors,
			static function ( array $left, array $right ): int {
				$count_delta = (int) ( $right['count'] ?? 0 ) <=> (int) ( $left['count'] ?? 0 );
				return 0 !== $count_delta ? $count_delta : strcmp( (string) ( $left['message'] ?? '' ), (string) ( $right['message'] ?? '' ) );
			}
		);

		return array(
			'total'   => array_sum( array_map( static fn( $error ): int => (int) ( $error['count'] ?? 0 ), $errors ) ),
			'by_kind' => homeboy_wordpress_bench_query_profiler_top_counts( $by_kind, $top ),
			'top'     => array_slice( array_values( $errors ), 0, $top ),
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_error_kind' ) ) {
	function homeboy_wordpress_bench_coverage_error_kind( int $severity ): string {
		$map = array(
			E_ERROR             => 'error',
			E_WARNING           => 'warning',
			E_PARSE             => 'parse',
			E_NOTICE            => 'notice',
			E_CORE_ERROR        => 'core_error',
			E_CORE_WARNING      => 'core_warning',
			E_COMPILE_ERROR     => 'compile_error',
			E_COMPILE_WARNING   => 'compile_warning',
			E_USER_ERROR        => 'user_error',
			E_USER_WARNING      => 'user_warning',
			E_USER_NOTICE       => 'user_notice',
			E_RECOVERABLE_ERROR => 'recoverable_error',
			E_DEPRECATED        => 'deprecated',
			E_USER_DEPRECATED   => 'user_deprecated',
		);

		return $map[ $severity ] ?? 'unknown';
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_coverage_gaps' ) ) {
	/** @return array<int,string> */
	function homeboy_wordpress_bench_coverage_gaps(): array {
		return array(
			'Filter counts are inferred as all-hook observations minus wp_actions deltas; WordPress does not expose a wp_filters counter equivalent to wp_actions.',
			'DB query shapes are collected through the WordPress query filter and normalized SQL signatures; they do not include external service calls or lower-level database access that bypasses wpdb.',
			'Mutation summary combines SQL write-operation counts with configured/discovered table row-count deltas; it does not prove semantic object-level changes.',
			'PHP fatal errors that stop execution before the wrapper can unwind cannot be summarized in the returned coverage payload.',
		);
	}
}
