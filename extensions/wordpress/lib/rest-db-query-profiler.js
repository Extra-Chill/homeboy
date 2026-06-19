'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const DEFAULT_ARTIFACT_RELATIVE_PATH = 'wp-content/homeboy-rest-db-queries.jsonl';
const DEFAULT_PLUGIN_FILE_NAME = 'homeboy-rest-db-query-profiler.php';

function normalizeSitePath(sitePath) {
	if (!sitePath || typeof sitePath !== 'string') {
		throw new TypeError('sitePath must be a non-empty string');
	}

	return path.resolve(sitePath);
}

function normalizeRelativePath(value, fallback) {
	const relativePath = value || fallback;
	if (typeof relativePath !== 'string' || relativePath.trim() === '') {
		throw new TypeError('artifactRelativePath must be a non-empty string');
	}
	if (path.isAbsolute(relativePath)) {
		throw new Error('artifactRelativePath must be relative to the WordPress site path');
	}

	const normalized = path.normalize(relativePath);
	if (normalized === '..' || normalized.startsWith(`..${path.sep}`)) {
		throw new Error('artifactRelativePath must stay inside the WordPress site path');
	}

	return normalized;
}

function phpString(value) {
	return `'${String(value).replace(/\\/g, '\\\\').replace(/'/g, "\\'")}'`;
}

function resolveRestDbQueryProfilerPaths(sitePath, options = {}) {
	const root = normalizeSitePath(sitePath);
	const artifactRelativePath = normalizeRelativePath(
		options.artifactRelativePath,
		DEFAULT_ARTIFACT_RELATIVE_PATH
	);
	const pluginFileName = options.pluginFileName || DEFAULT_PLUGIN_FILE_NAME;
	if (typeof pluginFileName !== 'string' || pluginFileName.trim() === '' || pluginFileName.includes('/') || pluginFileName.includes('\\')) {
		throw new TypeError('pluginFileName must be a file name, not a path');
	}

	return {
		sitePath: root,
		muPluginsDir: path.join(root, 'wp-content', 'mu-plugins'),
		pluginPath: path.join(root, 'wp-content', 'mu-plugins', pluginFileName),
		artifactPath: path.join(root, artifactRelativePath),
		artifactRelativePath,
	};
}

function generateRestDbQueryProfilerPlugin(options = {}) {
	const artifactRelativePath = normalizeRelativePath(
		options.artifactRelativePath,
		DEFAULT_ARTIFACT_RELATIVE_PATH
	).replace(/\\/g, '/');

	return `<?php
/**
 * Plugin Name: Homeboy REST DB Query Profiler
 * Description: Temporary Homeboy MU-plugin for correlating REST requests with DB query counts.
 */

if ( ! defined( 'ABSPATH' ) ) {
	return;
}

if ( defined( 'HOMEBOY_REST_DB_QUERY_PROFILER_LOADED' ) ) {
	return;
}

define( 'HOMEBOY_REST_DB_QUERY_PROFILER_LOADED', true );

$homeboy_rest_db_query_profiler_file = ABSPATH . ${phpString(artifactRelativePath)};
$homeboy_rest_db_query_profiler_requests = array();

if ( ! function_exists( 'homeboy_rest_db_query_profiler_query_count' ) ) {
	function homeboy_rest_db_query_profiler_query_count() {
		global $wpdb;

		return isset( $wpdb->queries ) && is_array( $wpdb->queries ) ? count( $wpdb->queries ) : 0;
	}
}

if ( ! function_exists( 'homeboy_rest_db_query_profiler_query_time' ) ) {
	function homeboy_rest_db_query_profiler_query_time( $start = 0 ) {
		global $wpdb;

		$total = 0.0;
		$queries = isset( $wpdb->queries ) && is_array( $wpdb->queries ) ? array_slice( $wpdb->queries, max( 0, (int) $start ) ) : array();
		foreach ( $queries as $query ) {
			if ( isset( $query[1] ) && is_numeric( $query[1] ) ) {
				$total += (float) $query[1];
			}
		}

		return $total;
	}
}

if ( ! function_exists( 'homeboy_rest_db_query_profiler_write' ) ) {
	function homeboy_rest_db_query_profiler_write( $entry ) {
		global $homeboy_rest_db_query_profiler_file;

		$dir = dirname( $homeboy_rest_db_query_profiler_file );
		if ( ! is_dir( $dir ) ) {
			wp_mkdir_p( $dir );
		}

		file_put_contents(
			$homeboy_rest_db_query_profiler_file,
			wp_json_encode( $entry, JSON_UNESCAPED_SLASHES ) . PHP_EOL,
			FILE_APPEND | LOCK_EX
		);
	}
}

add_filter(
	'rest_pre_dispatch',
	static function ( $result, $server, $request ) use ( &$homeboy_rest_db_query_profiler_requests ) {
		global $wpdb;

		if ( is_object( $wpdb ) ) {
			$wpdb->save_queries = true;
		}

		$key = is_object( $request ) ? spl_object_id( $request ) : count( $homeboy_rest_db_query_profiler_requests );
		$homeboy_rest_db_query_profiler_requests[ $key ] = array(
			'start_time'  => microtime( true ),
			'start_count' => homeboy_rest_db_query_profiler_query_count(),
		);

		return $result;
	},
	0,
	3
);

add_filter(
	'rest_post_dispatch',
	static function ( $response, $server, $request ) use ( &$homeboy_rest_db_query_profiler_requests ) {
		$key = is_object( $request ) ? spl_object_id( $request ) : null;
		$start = $key !== null && isset( $homeboy_rest_db_query_profiler_requests[ $key ] ) ? $homeboy_rest_db_query_profiler_requests[ $key ] : array( 'start_time' => microtime( true ), 'start_count' => homeboy_rest_db_query_profiler_query_count() );
		unset( $homeboy_rest_db_query_profiler_requests[ $key ] );

		$start_count = isset( $start['start_count'] ) ? (int) $start['start_count'] : 0;
		$end_count   = homeboy_rest_db_query_profiler_query_count();
		$status      = is_object( $response ) && method_exists( $response, 'get_status' ) ? (int) $response->get_status() : null;

		homeboy_rest_db_query_profiler_write(
			array(
				'schema'         => 'homeboy/wordpress-rest-db-query-profile/v1',
				'timestamp'      => gmdate( 'c' ),
				'method'         => is_object( $request ) && method_exists( $request, 'get_method' ) ? $request->get_method() : '',
				'route'          => is_object( $request ) && method_exists( $request, 'get_route' ) ? $request->get_route() : '',
				'status'         => $status,
				'duration_ms'    => round( ( microtime( true ) - (float) $start['start_time'] ) * 1000, 3 ),
				'query_count'    => max( 0, $end_count - $start_count ),
				'query_time_ms'  => round( homeboy_rest_db_query_profiler_query_time( $start_count ) * 1000, 3 ),
				'total_queries'  => $end_count,
			)
		);

		return $response;
	},
	1000000,
	3
);
`;
}

function installWordPressRestDbQueryProfiler(sitePath, options = {}) {
	const paths = resolveRestDbQueryProfilerPaths(sitePath, options);
	fs.mkdirSync(paths.muPluginsDir, { recursive: true });
	fs.mkdirSync(path.dirname(paths.artifactPath), { recursive: true });

	if (options.clearArtifact !== false && fs.existsSync(paths.artifactPath)) {
		fs.unlinkSync(paths.artifactPath);
	}

	fs.writeFileSync(paths.pluginPath, generateRestDbQueryProfilerPlugin(options), 'utf8');

	return paths;
}

function uninstallWordPressRestDbQueryProfiler(sitePath, options = {}) {
	const paths = resolveRestDbQueryProfilerPaths(sitePath, options);
	if (fs.existsSync(paths.pluginPath)) {
		fs.unlinkSync(paths.pluginPath);
	}
	return paths;
}

module.exports = {
	DEFAULT_REST_DB_QUERY_PROFILER_ARTIFACT_RELATIVE_PATH: DEFAULT_ARTIFACT_RELATIVE_PATH,
	DEFAULT_REST_DB_QUERY_PROFILER_PLUGIN_FILE_NAME: DEFAULT_PLUGIN_FILE_NAME,
	generateRestDbQueryProfilerPlugin,
	installWordPressRestDbQueryProfiler,
	resolveRestDbQueryProfilerPaths,
	uninstallWordPressRestDbQueryProfiler,
};
