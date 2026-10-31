<?php
/**
 * Generic WordPress database query profiling helpers for bench workloads.
 *
 * Workloads can require this file from the mounted extension path:
 * /homeboy-extension/scripts/bench/lib/wordpress-db-query-profiler.php
 */

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_start' ) ) {
	/**
	 * Start collecting SQL query profile counters through WordPress' query filter.
	 *
	 * @param string              $label Profile label.
	 * @param array<string,mixed> $options Profiler options.
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_query_profiler_start( string $label = '', array $options = array() ): array {
		global $wpdb;

		if ( isset( $GLOBALS['homeboy_wordpress_bench_query_profiler_state']['active'] ) ) {
			homeboy_wordpress_bench_query_profiler_stop();
		}

		$state = array(
			'label'               => $label,
			'options'             => $options,
			'started_at_unix_ms'  => (int) floor( microtime( true ) * 1000 ),
			'started_at'          => microtime( true ),
			'query_before'        => isset( $wpdb ) && isset( $wpdb->num_queries ) ? (int) $wpdb->num_queries : 0,
			'operations'          => array_fill_keys( array( 'select', 'insert', 'update', 'delete', 'replace', 'alter', 'create', 'drop', 'show', 'other' ), 0 ),
			'tables'              => array(),
			'operation_tables'    => array(),
			'categories'          => array(),
			'option_names'        => array(),
			'meta_keys'           => array(),
			'meta_key_operations' => array(),
			'signatures'          => array(),
			'queries_seen'        => 0,
		);

		$GLOBALS['homeboy_wordpress_bench_query_profiler_state'] = array( 'active' => $state );
		if ( function_exists( 'add_filter' ) ) {
			add_filter( 'query', 'homeboy_wordpress_bench_query_profiler_record_query', 10, 1 );
		}

		return homeboy_wordpress_bench_query_profiler_snapshot();
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_stop' ) ) {
	/**
	 * Stop profiling and return the final snapshot.
	 *
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_query_profiler_stop(): array {
		$snapshot = homeboy_wordpress_bench_query_profiler_snapshot();
		if ( function_exists( 'remove_filter' ) ) {
			remove_filter( 'query', 'homeboy_wordpress_bench_query_profiler_record_query', 10 );
		}
		unset( $GLOBALS['homeboy_wordpress_bench_query_profiler_state']['active'] );

		return $snapshot;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_snapshot' ) ) {
	/**
	 * Return the current profile snapshot with deterministic top-N truncation.
	 *
	 * @return array<string,mixed>
	 */
	function homeboy_wordpress_bench_query_profiler_snapshot(): array {
		global $wpdb;

		$state = $GLOBALS['homeboy_wordpress_bench_query_profiler_state']['active'] ?? null;
		if ( ! is_array( $state ) ) {
			return array(
				'label'        => '',
				'active'       => false,
				'query_count'  => 0,
				'queries_seen' => 0,
			);
		}

		$elapsed_ms  = max( 0, ( microtime( true ) - (float) $state['started_at'] ) * 1000 );
		$query_count = isset( $wpdb ) && isset( $wpdb->num_queries )
			? max( 0, (int) $wpdb->num_queries - (int) $state['query_before'] )
			: (int) $state['queries_seen'];
		$top         = isset( $state['options']['top'] ) && is_numeric( $state['options']['top'] )
			? max( 1, (int) $state['options']['top'] )
			: 30;

		return array(
			'label'               => (string) $state['label'],
			'active'              => true,
			'started_at_unix_ms'  => (int) $state['started_at_unix_ms'],
			'elapsed_ms'          => $elapsed_ms,
			'query_count'         => $query_count,
			'queries_seen'        => (int) $state['queries_seen'],
			'operations'          => homeboy_wordpress_bench_query_profiler_top_counts( $state['operations'], $top ),
			'tables'              => homeboy_wordpress_bench_query_profiler_top_counts( $state['tables'], $top ),
			'operation_tables'    => homeboy_wordpress_bench_query_profiler_top_counts( $state['operation_tables'], $top ),
			'categories'          => homeboy_wordpress_bench_query_profiler_top_counts( $state['categories'], $top ),
			'option_names'        => homeboy_wordpress_bench_query_profiler_top_counts( $state['option_names'], $top ),
			'meta_keys'           => homeboy_wordpress_bench_query_profiler_top_counts( $state['meta_keys'], $top ),
			'meta_key_operations' => homeboy_wordpress_bench_query_profiler_top_counts( $state['meta_key_operations'], $top ),
			'signatures'          => homeboy_wordpress_bench_query_profiler_top_counts( $state['signatures'], $top ),
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_profile_call' ) ) {
	/**
	 * Profile a callable and return its result with the query profile.
	 *
	 * @param string              $label Profile label.
	 * @param callable            $callback Workload callback.
	 * @param array<string,mixed> $options Profiler options.
	 * @return array{result:mixed,profile:array<string,mixed>}
	 */
	function homeboy_wordpress_bench_query_profiler_profile_call( string $label, callable $callback, array $options = array() ): array {
		homeboy_wordpress_bench_query_profiler_start( $label, $options );
		try {
			$result = $callback();
		} finally {
			$profile = homeboy_wordpress_bench_query_profiler_stop();
		}

		return array(
			'result'  => $result,
			'profile' => $profile,
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_record_query' ) ) {
	/**
	 * WordPress query filter callback.
	 *
	 * @param string $query SQL query.
	 * @return string
	 */
	function homeboy_wordpress_bench_query_profiler_record_query( string $query ): string {
		if ( empty( $GLOBALS['homeboy_wordpress_bench_query_profiler_state']['active'] ) ) {
			return $query;
		}

		$profile =& $GLOBALS['homeboy_wordpress_bench_query_profiler_state']['active'];
		++$profile['queries_seen'];

		$operation = homeboy_wordpress_bench_query_profiler_operation( $query );
		$tables    = homeboy_wordpress_bench_query_profiler_tables( $query );
		$category  = homeboy_wordpress_bench_query_profiler_category( $query, $operation, $tables, $profile['options'] ?? array() );

		homeboy_wordpress_bench_query_profiler_increment( $profile['operations'], $operation );
		foreach ( $tables as $table ) {
			homeboy_wordpress_bench_query_profiler_increment( $profile['tables'], $table );
			homeboy_wordpress_bench_query_profiler_increment( $profile['operation_tables'], $operation . ':' . $table );
		}
		homeboy_wordpress_bench_query_profiler_increment( $profile['categories'], $category );
		homeboy_wordpress_bench_query_profiler_collect_named_values( $profile['option_names'], $query, '/option_name\s*=\s*[\'\"]([^\'\"]+)[\'\"]/i' );
		homeboy_wordpress_bench_query_profiler_collect_named_values( $profile['meta_keys'], $query, '/meta_key\s*=\s*[\'\"]([^\'\"]+)[\'\"]/i' );

		$meta_operation = homeboy_wordpress_bench_query_profiler_meta_operation( $query, $operation );
		if ( null !== $meta_operation ) {
			homeboy_wordpress_bench_query_profiler_increment( $profile['meta_key_operations'], $meta_operation );
		}

		homeboy_wordpress_bench_query_profiler_increment(
			$profile['signatures'],
			homeboy_wordpress_bench_query_profiler_signature( $query )
		);

		return $query;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_metric' ) ) {
	/**
	 * Read a count from a profile section.
	 *
	 * @param array<string,mixed> $profile Query profile.
	 * @param string              $section Section name.
	 * @param string              $key Counter key.
	 * @return int
	 */
	function homeboy_wordpress_bench_query_profiler_metric( array $profile, string $section, string $key ): int {
		return (int) ( $profile[ $section ][ $key ] ?? 0 );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_metric_per_item' ) ) {
	/**
	 * Read a count from a profile section divided by item count.
	 *
	 * @param array<string,mixed> $profile Query profile.
	 * @param string              $section Section name.
	 * @param string              $key Counter key.
	 * @param int|float           $items Item count.
	 * @return float
	 */
	function homeboy_wordpress_bench_query_profiler_metric_per_item( array $profile, string $section, string $key, $items ): float {
		$items = max( 1, (float) $items );
		return homeboy_wordpress_bench_query_profiler_metric( $profile, $section, $key ) / $items;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_table_counts' ) ) {
	/**
	 * Return row counts for table names or labels mapped to table names.
	 *
	 * @param array<int|string,string> $tables Tables to count.
	 * @return array<string,int>
	 */
	function homeboy_wordpress_bench_query_profiler_table_counts( array $tables ): array {
		global $wpdb;

		$counts = array();
		foreach ( $tables as $label => $table ) {
			$key = is_string( $label ) ? $label : homeboy_wordpress_bench_query_profiler_table_key( (string) $table );
			if ( ! isset( $wpdb ) || ! homeboy_wordpress_bench_query_profiler_table_exists( (string) $table ) ) {
				$counts[ $key ] = 0;
				continue;
			}
			$counts[ $key ] = (int) $wpdb->get_var( 'SELECT COUNT(*) FROM `' . esc_sql( (string) $table ) . '`' );
		}

		return $counts;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_count_deltas' ) ) {
	/**
	 * Return numeric after-before deltas for two count maps.
	 *
	 * @param array<string,int|float> $before Earlier counts.
	 * @param array<string,int|float> $after Later counts.
	 * @return array<string,int|float>
	 */
	function homeboy_wordpress_bench_query_profiler_count_deltas( array $before, array $after ): array {
		$keys   = array_unique( array_merge( array_keys( $before ), array_keys( $after ) ) );
		$deltas = array();
		foreach ( $keys as $key ) {
			$deltas[ $key ] = ( $after[ $key ] ?? 0 ) - ( $before[ $key ] ?? 0 );
		}

		return $deltas;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_invariant' ) ) {
	/**
	 * Format an invariant failure when a condition is false.
	 *
	 * @param string              $name Invariant name.
	 * @param bool                $passed Whether the invariant passed.
	 * @param array<string,mixed> $context Failure context.
	 * @return array<string,mixed>|null
	 */
	function homeboy_wordpress_bench_query_profiler_invariant( string $name, bool $passed, array $context = array() ): ?array {
		if ( $passed ) {
			return null;
		}

		return array(
			'name'    => $name,
			'context' => $context,
		);
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_increment' ) ) {
	/**
	 * Increment a counter map key.
	 *
	 * @param array<string,int> $map Counter map.
	 * @param string            $key Counter key.
	 */
	function homeboy_wordpress_bench_query_profiler_increment( array &$map, string $key ): void {
		$map[ $key ] = (int) ( $map[ $key ] ?? 0 ) + 1;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_operation' ) ) {
	function homeboy_wordpress_bench_query_profiler_operation( string $query ): string {
		if ( preg_match( '/^\s*(SELECT|INSERT|UPDATE|DELETE|REPLACE|ALTER|CREATE|DROP|SHOW)\b/i', $query, $match ) ) {
			return strtolower( $match[1] );
		}

		return 'other';
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_tables' ) ) {
	/**
	 * Extract normalized table keys from common SQL clauses.
	 *
	 * @param string $query SQL query.
	 * @return array<int,string>
	 */
	function homeboy_wordpress_bench_query_profiler_tables( string $query ): array {
		$tables = array();
		if ( preg_match_all( '/(?:FROM|JOIN|INTO|UPDATE|TABLE)\s+`?([a-zA-Z0-9_]+)`?/i', $query, $matches ) ) {
			foreach ( $matches[1] as $table ) {
				$tables[] = homeboy_wordpress_bench_query_profiler_table_key( $table );
			}
		}

		$tables = array_values( array_unique( array_filter( $tables ) ) );
		return empty( $tables ) ? array( 'unknown' ) : $tables;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_table_key' ) ) {
	function homeboy_wordpress_bench_query_profiler_table_key( string $table ): string {
		global $wpdb;

		$prefix = isset( $wpdb ) && isset( $wpdb->prefix ) ? (string) $wpdb->prefix : '';
		if ( '' !== $prefix && 0 === strpos( $table, $prefix ) ) {
			return substr( $table, strlen( $prefix ) );
		}

		return $table;
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_category' ) ) {
	/**
	 * Classify a query into generic WordPress bench evidence buckets.
	 *
	 * @param string              $query SQL query.
	 * @param string              $operation SQL operation.
	 * @param array<int,string>   $tables Normalized table keys.
	 * @param array<string,mixed> $options Profiler options.
	 * @return string
	 */
	function homeboy_wordpress_bench_query_profiler_category( string $query, string $operation, array $tables, array $options ): string {
		if ( ! empty( $options['classifiers'] ) && is_array( $options['classifiers'] ) ) {
			foreach ( $options['classifiers'] as $classifier ) {
				if ( is_callable( $classifier ) ) {
					$classified = $classifier( $query, $operation, $tables );
					if ( is_string( $classified ) && '' !== $classified ) {
						return $classified;
					}
				}
			}
		}

		$first_table = $tables[0] ?? 'unknown';
		if ( preg_match( '/_transient_[a-zA-Z0-9_\-]+/', $query ) ) {
			return 'transient_option';
		}
		if ( false !== strpos( $query, 'actionscheduler_' ) ) {
			return 'action_scheduler';
		}
		if ( 'postmeta' === $first_table && 'select' === $operation && preg_match( '/SELECT\s+meta_id\s+FROM/i', $query ) ) {
			return 'meta_exists';
		}
		if ( 'postmeta' === $first_table && 'insert' === $operation ) {
			return 'meta_insert';
		}
		if ( 'postmeta' === $first_table && 'update' === $operation ) {
			return 'meta_update';
		}
		if ( 'postmeta' === $first_table ) {
			return 'meta_read';
		}
		if ( in_array( 'terms', $tables, true ) || in_array( 'term_taxonomy', $tables, true ) || in_array( 'term_relationships', $tables, true ) ) {
			return 'term_lookup';
		}
		if ( 'options' === $first_table ) {
			return 'option';
		}
		if ( 'posts' === $first_table ) {
			return 'post';
		}

		return 'other';
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_meta_operation' ) ) {
	function homeboy_wordpress_bench_query_profiler_meta_operation( string $query, string $operation ): ?string {
		if ( ! preg_match( '/meta_key\s*=\s*[\'\"]([^\'\"]+)[\'\"]/i', $query, $match ) ) {
			if ( 'insert' === $operation && preg_match( '/INSERT\s+INTO\s+`?[a-zA-Z0-9_]*postmeta`?.*VALUES\s*\([^,]+,\s*[\'\"]([^\'\"]+)[\'\"]/i', $query, $match ) ) {
				return 'insert:' . $match[1];
			}
			return null;
		}

		$meta_operation = $operation;
		if ( 'select' === $operation && preg_match( '/SELECT\s+meta_id\s+FROM/i', $query ) ) {
			$meta_operation = 'exists';
		}

		return $meta_operation . ':' . $match[1];
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_collect_named_values' ) ) {
	function homeboy_wordpress_bench_query_profiler_collect_named_values( array &$map, string $query, string $pattern ): void {
		if ( ! preg_match_all( $pattern, $query, $matches ) ) {
			return;
		}
		foreach ( $matches[1] as $value ) {
			homeboy_wordpress_bench_query_profiler_increment( $map, (string) $value );
		}
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_signature' ) ) {
	function homeboy_wordpress_bench_query_profiler_signature( string $query ): string {
		$signature = preg_replace( '/\s+/', ' ', trim( $query ) );
		$signature = preg_replace( '/\b\d+\b/', '?', (string) $signature );
		$signature = preg_replace( "/'[^']*'/", '?', (string) $signature );
		$signature = preg_replace( '/"[^"]*"/', '?', (string) $signature );

		return substr( (string) $signature, 0, 220 );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_top_counts' ) ) {
	/**
	 * Sort counts descending, then by key for deterministic ties, and truncate.
	 *
	 * @param array<string,int> $counts Counter map.
	 * @param int               $limit Max entries.
	 * @return array<string,int>
	 */
	function homeboy_wordpress_bench_query_profiler_top_counts( array $counts, int $limit ): array {
		uksort(
			$counts,
			static function ( string $left, string $right ) use ( $counts ): int {
				$left_count  = (int) $counts[ $left ];
				$right_count = (int) $counts[ $right ];
				if ( $left_count === $right_count ) {
					return strcmp( $left, $right );
				}
				return $right_count <=> $left_count;
			}
		);

		return array_slice( $counts, 0, $limit, true );
	}
}

if ( ! function_exists( 'homeboy_wordpress_bench_query_profiler_table_exists' ) ) {
	function homeboy_wordpress_bench_query_profiler_table_exists( string $table ): bool {
		global $wpdb;

		if ( ! isset( $wpdb ) || ! method_exists( $wpdb, 'prepare' ) ) {
			return false;
		}

		return (string) $wpdb->get_var( $wpdb->prepare( 'SHOW TABLES LIKE %s', $table ) ) === $table;
	}
}
