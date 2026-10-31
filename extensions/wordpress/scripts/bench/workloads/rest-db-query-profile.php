<?php
/**
 * Generic WordPress REST DB query profile workload.
 *
 * The embedding preset defines $wp_codebox_rest_db_query_profile_config before
 * this source is evaluated by the WordPress workload runner.
 */

return static function (): array {
	$started = microtime( true );
	$config  = isset( $wp_codebox_rest_db_query_profile_config ) && is_array( $wp_codebox_rest_db_query_profile_config )
		? $wp_codebox_rest_db_query_profile_config
		: array();

	$route_scopes       = isset( $config['route_scopes'] ) && is_array( $config['route_scopes'] ) ? $config['route_scopes'] : array();
	$explicit_cases     = isset( $config['rest_request_cases'] ) && is_array( $config['rest_request_cases'] ) ? $config['rest_request_cases'] : array();
	$case_limit         = isset( $config['case_limit'] ) ? max( 1, (int) $config['case_limit'] ) : 80;
	$query_length_limit = isset( $config['query_length_limit'] ) ? max( 1, (int) $config['query_length_limit'] ) : 500;

	$match_scope = static function ( string $route ) use ( $route_scopes ): ?array {
		foreach ( $route_scopes as $scope ) {
			if ( ! is_array( $scope ) ) {
				continue;
			}

			$prefixes = isset( $scope['prefixes'] ) && is_array( $scope['prefixes'] ) ? $scope['prefixes'] : array();
			foreach ( $prefixes as $prefix ) {
				if ( 0 === strpos( $route, (string) $prefix ) ) {
					return $scope;
				}
			}

			$patterns = isset( $scope['patterns'] ) && is_array( $scope['patterns'] ) ? $scope['patterns'] : array();
			foreach ( $patterns as $pattern ) {
				if ( @preg_match( (string) $pattern, $route ) ) {
					return $scope;
				}
			}
		}

		return null;
	};

	$default_params = static function ( string $route, array $scope ): array {
		$params = isset( $scope['default_params'] ) && is_array( $scope['default_params'] ) ? $scope['default_params'] : array();
		$rules  = isset( $scope['param_rules'] ) && is_array( $scope['param_rules'] ) ? $scope['param_rules'] : array();
		foreach ( $rules as $rule ) {
			if ( ! is_array( $rule ) || empty( $rule['params'] ) || ! is_array( $rule['params'] ) ) {
				continue;
			}
			$pattern = isset( $rule['pattern'] ) ? (string) $rule['pattern'] : '';
			if ( '' === $pattern || @preg_match( $pattern, $route ) ) {
				$params = array_merge( $params, $rule['params'] );
			}
		}

		return $params;
	};

	$expected_statuses = static function ( string $route, array $scope ): array {
		$rules = isset( $scope['expected_status_rules'] ) && is_array( $scope['expected_status_rules'] ) ? $scope['expected_status_rules'] : array();
		foreach ( $rules as $rule ) {
			if ( ! is_array( $rule ) || empty( $rule['statuses'] ) || ! is_array( $rule['statuses'] ) ) {
				continue;
			}
			$pattern = isset( $rule['pattern'] ) ? (string) $rule['pattern'] : '';
			if ( '' === $pattern || @preg_match( $pattern, $route ) ) {
				return array_values( array_map( 'intval', $rule['statuses'] ) );
			}
		}

		$statuses = isset( $scope['expected_statuses'] ) && is_array( $scope['expected_statuses'] ) ? $scope['expected_statuses'] : array( 200, 401, 403 );
		return array_values( array_map( 'intval', $statuses ) );
	};

	$cases   = array();
	$skipped = array();
	$counts  = array();

	foreach ( $explicit_cases as $case ) {
		if ( ! is_array( $case ) || empty( $case['path'] ) ) {
			continue;
		}
		$cases[] = array_merge(
			array(
				'id'               => sanitize_key( trim( str_replace( '/', '-', (string) $case['path'] ), '-' ) ),
				'method'           => 'GET',
				'params'           => array(),
				'capture_response' => true,
			),
			$case
		);
	}

	if ( count( $cases ) < $case_limit ) {
		$server = rest_get_server();
		$routes = $server->get_routes();
		ksort( $routes );

		foreach ( $routes as $route => $handlers ) {
			if ( count( $cases ) >= $case_limit ) {
				break;
			}

			$scope = $match_scope( $route );
			if ( null === $scope ) {
				continue;
			}

			$surface = isset( $scope['id'] ) ? (string) $scope['id'] : 'rest';
			if ( ! isset( $counts[ $surface ] ) ) {
				$counts[ $surface ] = array( 'routes' => 0, 'covered' => 0, 'skipped' => 0 );
			}
			++$counts[ $surface ]['routes'];

			if ( false !== strpos( $route, '(?P<' ) ) {
				++$counts[ $surface ]['skipped'];
				$skipped[] = array( 'path' => $route, 'surface' => $surface, 'reason_code' => 'dynamic_path_parameter' );
				continue;
			}

			$allows_get = false;
			foreach ( $handlers as $handler ) {
				foreach ( (array) ( $handler['methods'] ?? array() ) as $method => $enabled ) {
					$method_name = is_string( $method ) ? $method : (string) $enabled;
					if ( 'GET' === strtoupper( $method_name ) && $enabled ) {
						$allows_get = true;
					}
				}
			}

			if ( ! $allows_get ) {
				++$counts[ $surface ]['skipped'];
				$skipped[] = array( 'path' => $route, 'surface' => $surface, 'reason_code' => 'no_safe_read_method' );
				continue;
			}

			$statuses = $expected_statuses( $route, $scope );
			$cases[]  = array(
				'id'                => sanitize_key( trim( str_replace( '/', '-', $route ), '-' ) ),
				'method'            => 'GET',
				'path'              => $route,
				'params'            => $default_params( $route, $scope ),
				'capture_response'  => true,
				'expected_statuses' => $statuses,
				'metadata'          => array(
					'surface'          => $surface,
					'expected_outcome' => array( 200 ) === $statuses ? 'public_read_success' : 'bounded_read_or_auth_boundary',
					'source'           => 'registered-rest-route-inventory',
				),
			);
			++$counts[ $surface ]['covered'];
		}
	}

	$responses = array();
	$profiles  = array();
	foreach ( $cases as $case ) {
		$request = new WP_REST_Request( strtoupper( (string) ( $case['method'] ?? 'GET' ) ), (string) $case['path'] );
		foreach ( (array) ( $case['params'] ?? array() ) as $key => $value ) {
			$request->set_param( $key, $value );
		}

		global $wpdb;
		if ( isset( $wpdb ) ) {
			$wpdb->save_queries = true;
		}
		$start_count = isset( $wpdb->queries ) && is_array( $wpdb->queries ) ? count( $wpdb->queries ) : 0;
		$before      = microtime( true );
		$response    = rest_do_request( $request );
		$status      = (int) $response->get_status();
		$queries     = isset( $wpdb->queries ) && is_array( $wpdb->queries ) ? array_slice( $wpdb->queries, $start_count ) : array();
		$shapes      = array();
		foreach ( $queries as $query ) {
			$sql = isset( $query[0] ) ? preg_replace( '/\s+/', ' ', trim( (string) $query[0] ) ) : '';
			if ( '' === $sql ) {
				continue;
			}
			$sql = substr( $sql, 0, $query_length_limit );
			if ( ! isset( $shapes[ $sql ] ) ) {
				$shapes[ $sql ] = array( 'sql' => $sql, 'count' => 0 );
			}
			++$shapes[ $sql ]['count'];
		}

		$responses[] = array(
			'id'              => $case['id'],
			'path'            => $case['path'],
			'method'          => $case['method'] ?? 'GET',
			'surface'         => $case['metadata']['surface'] ?? '',
			'status'          => $status,
			'expected_status' => in_array( $status, (array) ( $case['expected_statuses'] ?? array( 200, 401, 403 ) ), true ),
		);
		$profiles[]  = array(
			'id'               => $case['id'],
			'path'             => $case['path'],
			'status'           => $status,
			'duration_ms'      => round( ( microtime( true ) - $before ) * 1000, 3 ),
			'query_count'      => count( $queries ),
			'top_query_shapes' => array_slice( array_values( $shapes ), 0, 10 ),
		);
	}

	$summary = array(
		'generated_case_count' => count( $cases ),
		'skipped_route_count'  => count( $skipped ),
		'response_count'       => count( $responses ),
		'route_scope_counts'   => $counts,
		'total_elapsed_ms'     => round( ( microtime( true ) - $started ) * 1000, 3 ),
	);

	$artifact_path = '';
	$shared_state  = getenv( 'WP_CODEBOX_BENCH_SHARED_STATE' );
	if ( $shared_state ) {
		$artifact_dir = rtrim( $shared_state, '/' ) . '/rest-db-query-profile';
		wp_mkdir_p( $artifact_dir );
		$artifact_path = $artifact_dir . '/rest-db-query-profile.json';
		file_put_contents(
			$artifact_path,
			wp_json_encode(
				array(
					'schema'       => 'wp-codebox/wordpress-rest-db-query-profile/v1',
					'generation'   => array(
						'source'       => 'registered-rest-route-inventory',
						'safe_methods' => array( 'GET' ),
						'route_scopes' => $route_scopes,
					),
					'cases'        => $cases,
					'responses'    => $responses,
					'profiles'     => $profiles,
					'coverage_gap' => array(
						'schema'       => 'wp-codebox/wordpress-rest-route-coverage-gap/v1',
						'surface_type' => 'rest',
						'expected'     => $counts,
						'covered'      => array_column( $cases, 'path' ),
						'gaps'         => $skipped,
						'status'       => empty( $skipped ) ? 'covered' : 'partial',
					),
					'metrics'      => $summary,
				),
				JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES
			) . "\n"
		);
	}

	return array(
		'metrics'   => $summary,
		'metadata'  => array(
			'runner'         => 'wp-codebox',
			'workload'       => 'rest-db-query-profile',
			'coverage_shape' => 'configured WordPress REST route DB query profile',
		),
		'artifacts' => $artifact_path ? array( 'rest_db_query_profile' => array( 'path' => $artifact_path, 'kind' => 'json' ) ) : array(),
	);
};
