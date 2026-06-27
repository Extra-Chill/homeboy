'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const DEFAULT_ARTIFACT_RELATIVE_PATH = 'wp-content/homeboy-profile.jsonl';
const DEFAULT_PLUGIN_FILE_NAME = 'homeboy-request-profiler.php';
const DEFAULT_HOOKS = [
	'muplugins_loaded',
	'plugins_loaded',
	'setup_theme',
	'after_setup_theme',
	'init',
	'wp_loaded',
	'admin_menu',
	'admin_init',
	'current_screen',
	'admin_enqueue_scripts',
	'admin_head',
	'admin_footer',
	'shutdown',
];
const DEFAULT_PRIORITY_BAND_HOOKS = [
	'admin_init',
	'current_screen',
	'admin_enqueue_scripts',
];
const FUZZ_OBSERVATION_SET_SCHEMA = 'homeboy/fuzz-observation-set/v1';
const FUZZ_HOTSPOT_SET_SCHEMA = 'homeboy/fuzz-hotspot-set/v1';

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

function normalizeList(value, fallback, label) {
	const list = value === undefined ? fallback : value;
	if (!Array.isArray(list)) {
		throw new TypeError(`${label} must be an array`);
	}

	return list.map((item) => {
		if (typeof item !== 'string' || item.trim() === '') {
			throw new TypeError(`${label} entries must be non-empty strings`);
		}
		return item.trim();
	});
}

function resolveProfilerPaths(sitePath, options = {}) {
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

function phpString(value) {
	return `'${String(value).replace(/\\/g, '\\\\').replace(/'/g, "\\'")}'`;
}

function phpArray(values) {
	return `array( ${values.map(phpString).join(', ')} )`;
}

function generateProfilerPlugin(options = {}) {
	const artifactRelativePath = normalizeRelativePath(
		options.artifactRelativePath,
		DEFAULT_ARTIFACT_RELATIVE_PATH
	).replace(/\\/g, '/');
	const hooks = normalizeList(options.hooks, DEFAULT_HOOKS, 'hooks');
	const priorityBandHooks = normalizeList(
		options.priorityBandHooks,
		DEFAULT_PRIORITY_BAND_HOOKS,
		'priorityBandHooks'
	);

	return `<?php
/**
 * Plugin Name: Homeboy Request Profiler
 * Description: Temporary Homeboy MU-plugin for WordPress request profiling.
 */

if ( ! defined( 'ABSPATH' ) ) {
	return;
}

if ( defined( 'HOMEBOY_REQUEST_PROFILER_LOADED' ) ) {
	return;
}

define( 'HOMEBOY_REQUEST_PROFILER_LOADED', true );

$homeboy_request_profiler_start = microtime( true );
$homeboy_request_profiler_id    = substr( hash( 'sha256', ( $_SERVER['REQUEST_METHOD'] ?? 'CLI' ) . '|' . ( $_SERVER['REQUEST_URI'] ?? '' ) . '|' . $homeboy_request_profiler_start ), 0, 16 );
$homeboy_request_profiler_file  = ABSPATH . ${phpString(artifactRelativePath)};
$homeboy_request_profiler_query_phases = array();

if ( ! function_exists( 'homeboy_request_profiler_write' ) ) {
	function homeboy_request_profiler_write( $event, $data = array() ) {
		global $homeboy_request_profiler_start, $homeboy_request_profiler_id, $homeboy_request_profiler_file;

		$entry = array(
			'v'          => 1,
			'event'      => $event,
			'timestamp'  => gmdate( 'c' ),
			't_ms'       => round( ( microtime( true ) - $homeboy_request_profiler_start ) * 1000, 3 ),
			'request_id' => $homeboy_request_profiler_id,
			'method'     => $_SERVER['REQUEST_METHOD'] ?? 'CLI',
			'uri'        => $_SERVER['REQUEST_URI'] ?? '',
			'data'       => $data,
		);

		$dir = dirname( $homeboy_request_profiler_file );
		if ( ! is_dir( $dir ) ) {
			wp_mkdir_p( $dir );
		}

		file_put_contents(
			$homeboy_request_profiler_file,
			wp_json_encode( $entry, JSON_UNESCAPED_SLASHES ) . PHP_EOL,
			FILE_APPEND | LOCK_EX
		);
	}
}

if ( ! function_exists( 'homeboy_request_profiler_query_count' ) ) {
	function homeboy_request_profiler_query_count() {
		global $wpdb;

		return isset( $wpdb->queries ) && is_array( $wpdb->queries ) ? count( $wpdb->queries ) : 0;
	}
}

if ( ! function_exists( 'homeboy_request_profiler_query_time' ) ) {
	function homeboy_request_profiler_query_time( $start = 0 ) {
		global $wpdb;

		$total   = 0.0;
		$queries = isset( $wpdb->queries ) && is_array( $wpdb->queries ) ? array_slice( $wpdb->queries, max( 0, (int) $start ) ) : array();
		foreach ( $queries as $query ) {
			if ( isset( $query[1] ) && is_numeric( $query[1] ) ) {
				$total += (float) $query[1];
			}
		}

		return $total;
	}
}

if ( ! function_exists( 'homeboy_request_profiler_normalize_sql' ) ) {
	function homeboy_request_profiler_normalize_sql( $sql ) {
		$shape = preg_replace( '/\s+/', ' ', trim( (string) $sql ) );
		$shape = preg_replace( '/\b0x[0-9a-f]+\b/i', '?', $shape );
		$shape = preg_replace( "/'(?:''|[^'])*'/", '?', $shape );
		$shape = preg_replace( '/"(?:\\\\"|[^"])*"/', '?', $shape );
		$shape = preg_replace( '/\b\d+(?:\.\d+)?\b/', '?', $shape );
		$shape = preg_replace( '/\(\s*\?(?:\s*,\s*\?)+\s*\)/', '(?)', $shape );

		return $shape;
	}
}

if ( ! function_exists( 'homeboy_request_profiler_top_query_shapes' ) ) {
	function homeboy_request_profiler_top_query_shapes( $start = 0, $limit = 5 ) {
		global $wpdb;

		$queries = isset( $wpdb->queries ) && is_array( $wpdb->queries ) ? array_slice( $wpdb->queries, max( 0, (int) $start ) ) : array();
		$shapes  = array();
		foreach ( $queries as $query ) {
			$sql = isset( $query[0] ) ? (string) $query[0] : '';
			if ( '' === trim( $sql ) ) {
				continue;
			}

			$shape = homeboy_request_profiler_normalize_sql( $sql );
			if ( ! isset( $shapes[ $shape ] ) ) {
				$shapes[ $shape ] = array(
					'sql'     => $shape,
					'count'   => 0,
					'time_ms' => 0.0,
				);
			}

			$shapes[ $shape ]['count'] += 1;
			if ( isset( $query[1] ) && is_numeric( $query[1] ) ) {
				$shapes[ $shape ]['time_ms'] += (float) $query[1] * 1000;
			}
		}

		usort(
			$shapes,
			static function ( $a, $b ) {
				if ( $a['time_ms'] === $b['time_ms'] ) {
					return $b['count'] <=> $a['count'];
				}

				return $b['time_ms'] <=> $a['time_ms'];
			}
		);

		return array_map(
			static function ( $shape ) {
				$shape['time_ms'] = round( $shape['time_ms'], 3 );

				return $shape;
			},
			array_slice( $shapes, 0, max( 0, (int) $limit ) )
		);
	}
}

if ( ! function_exists( 'homeboy_request_profiler_context' ) ) {
	function homeboy_request_profiler_context() {
		if ( defined( 'REST_REQUEST' ) && REST_REQUEST ) {
			return 'rest';
		}
		if ( wp_doing_ajax() ) {
			return 'ajax';
		}
		if ( wp_doing_cron() ) {
			return 'cron';
		}
		if ( is_admin() ) {
			return 'admin';
		}

		return 'frontend';
	}
}

if ( ! function_exists( 'homeboy_request_profiler_phase_key' ) ) {
	function homeboy_request_profiler_phase_key( $phase, $data = array() ) {
		return $phase . ':' . substr( hash( 'sha256', wp_json_encode( $data, JSON_UNESCAPED_SLASHES ) ), 0, 12 );
	}
}

if ( ! function_exists( 'homeboy_request_profiler_query_phase_start' ) ) {
	function homeboy_request_profiler_query_phase_start( $phase, $data = array() ) {
		global $wpdb, $homeboy_request_profiler_query_phases;

		if ( is_object( $wpdb ) ) {
			$wpdb->save_queries = true;
		}

		$key = homeboy_request_profiler_phase_key( $phase, $data );
		$homeboy_request_profiler_query_phases[ $key ] = array(
			'phase'       => $phase,
			'data'        => $data,
			'start_time'  => microtime( true ),
			'start_count' => homeboy_request_profiler_query_count(),
		);

		homeboy_request_profiler_write( 'db_query.phase.start', array_merge( array( 'phase' => $phase ), $data ) );

		return $key;
	}
}

if ( ! function_exists( 'homeboy_request_profiler_query_phase_stop' ) ) {
	function homeboy_request_profiler_query_phase_stop( $phase, $data = array(), $key = null ) {
		global $homeboy_request_profiler_query_phases;

		$key = $key ? $key : homeboy_request_profiler_phase_key( $phase, $data );
		if ( ! isset( $homeboy_request_profiler_query_phases[ $key ] ) ) {
			foreach ( $homeboy_request_profiler_query_phases as $candidate_key => $candidate ) {
				if ( isset( $candidate['phase'] ) && $phase === $candidate['phase'] ) {
					$key = $candidate_key;
					break;
				}
			}
		}

		$start = isset( $homeboy_request_profiler_query_phases[ $key ] ) ? $homeboy_request_profiler_query_phases[ $key ] : array(
			'phase'       => $phase,
			'data'        => $data,
			'start_time'  => microtime( true ),
			'start_count' => homeboy_request_profiler_query_count(),
		);
		unset( $homeboy_request_profiler_query_phases[ $key ] );

		$start_count = isset( $start['start_count'] ) ? (int) $start['start_count'] : 0;
		$end_count   = homeboy_request_profiler_query_count();
		$phase_data  = array_merge(
			array(
				'phase'            => $phase,
				'context'          => homeboy_request_profiler_context(),
				'duration_ms'      => round( ( microtime( true ) - (float) $start['start_time'] ) * 1000, 3 ),
				'query_count'      => max( 0, $end_count - $start_count ),
				'query_time_ms'    => round( homeboy_request_profiler_query_time( $start_count ) * 1000, 3 ),
				'top_query_shapes' => homeboy_request_profiler_top_query_shapes( $start_count, 5 ),
				'total_queries'    => $end_count,
			),
			isset( $start['data'] ) && is_array( $start['data'] ) ? $start['data'] : array(),
			$data
		);

		homeboy_request_profiler_write( 'db_query.phase.stop', $phase_data );
	}
}

homeboy_request_profiler_write( 'request.start', array( 'php_sapi' => PHP_SAPI ) );
homeboy_request_profiler_query_phase_start( 'request', array( 'surface_type' => homeboy_request_profiler_context() ) );

foreach ( ${phpArray(hooks)} as $homeboy_request_profiler_hook ) {
	add_action(
		$homeboy_request_profiler_hook,
		static function () use ( $homeboy_request_profiler_hook ) {
			homeboy_request_profiler_write( 'hook', array( 'hook' => $homeboy_request_profiler_hook ) );
		},
		10,
		0
	);
}

foreach ( ${phpArray(priorityBandHooks)} as $homeboy_request_profiler_hook ) {
	add_action(
		$homeboy_request_profiler_hook,
		static function () use ( $homeboy_request_profiler_hook ) {
			homeboy_request_profiler_write( 'hook.priority_band.start', array( 'hook' => $homeboy_request_profiler_hook, 'priority' => -1000000 ) );
		},
		-1000000,
		0
	);
	add_action(
		$homeboy_request_profiler_hook,
		static function () use ( $homeboy_request_profiler_hook ) {
			homeboy_request_profiler_write( 'hook.priority_band.end', array( 'hook' => $homeboy_request_profiler_hook, 'priority' => 1000000 ) );
		},
		1000000,
		0
	);
}

add_filter(
	'pre_http_request',
	static function ( $preempt, $parsed_args, $url ) {
		homeboy_request_profiler_write(
			'http.request.start',
			array(
				'id'     => substr( hash( 'sha256', $url ), 0, 16 ),
				'url'    => $url,
				'method' => $parsed_args['method'] ?? 'GET',
			)
		);

		return $preempt;
	},
	10,
	3
);

add_filter(
	'rest_pre_dispatch',
	static function ( $result, $server, $request ) {
		$route = is_object( $request ) && method_exists( $request, 'get_route' ) ? $request->get_route() : '';
		$method = is_object( $request ) && method_exists( $request, 'get_method' ) ? $request->get_method() : '';
		homeboy_request_profiler_query_phase_start( 'rest.request', array( 'surface_type' => 'rest', 'route' => $route, 'method' => $method ) );

		return $result;
	},
	0,
	3
);

add_filter(
	'rest_post_dispatch',
	static function ( $response, $server, $request ) {
		$route  = is_object( $request ) && method_exists( $request, 'get_route' ) ? $request->get_route() : '';
		$method = is_object( $request ) && method_exists( $request, 'get_method' ) ? $request->get_method() : '';
		$status = is_object( $response ) && method_exists( $response, 'get_status' ) ? (int) $response->get_status() : null;
		homeboy_request_profiler_query_phase_stop( 'rest.request', array( 'surface_type' => 'rest', 'route' => $route, 'method' => $method, 'status' => $status ) );

		return $response;
	},
	1000000,
	3
);

add_action(
	'template_redirect',
	static function () {
		if ( 'frontend' === homeboy_request_profiler_context() ) {
			homeboy_request_profiler_query_phase_start( 'frontend.page', array( 'surface_type' => 'frontend', 'path' => wp_parse_url( $_SERVER['REQUEST_URI'] ?? '/', PHP_URL_PATH ) ) );
		}
	},
	0,
	0
);

add_action(
	'admin_init',
	static function () {
		if ( wp_doing_ajax() ) {
			homeboy_request_profiler_query_phase_start( 'ajax.request', array( 'surface_type' => 'ajax', 'action' => $_REQUEST['action'] ?? '' ) );
			return;
		}

		if ( is_admin() ) {
			homeboy_request_profiler_query_phase_start( 'admin.page', array( 'surface_type' => 'admin', 'path' => wp_parse_url( $_SERVER['REQUEST_URI'] ?? '/', PHP_URL_PATH ) ) );
		}
	},
	0,
	0
);

add_action(
	'init',
	static function () {
		if ( wp_doing_cron() ) {
			homeboy_request_profiler_query_phase_start( 'cron.request', array( 'surface_type' => 'cron' ) );
		}
	},
	0,
	0
);

add_filter(
	'pre_render_block',
	static function ( $pre_render, $parsed_block ) {
		$block_name = isset( $parsed_block['blockName'] ) ? (string) $parsed_block['blockName'] : '';
		homeboy_request_profiler_query_phase_start( 'block.render', array( 'surface_type' => 'block', 'block_name' => $block_name ) );

		return $pre_render;
	},
	0,
	2
);

add_filter(
	'render_block',
	static function ( $block_content, $parsed_block ) {
		$block_name = isset( $parsed_block['blockName'] ) ? (string) $parsed_block['blockName'] : '';
		homeboy_request_profiler_query_phase_stop( 'block.render', array( 'surface_type' => 'block', 'block_name' => $block_name ) );

		return $block_content;
	},
	1000000,
	2
);

add_action(
	'shutdown',
	static function () {
		$context = homeboy_request_profiler_context();
		if ( 'frontend' === $context ) {
			homeboy_request_profiler_query_phase_stop( 'frontend.page', array( 'surface_type' => 'frontend', 'path' => wp_parse_url( $_SERVER['REQUEST_URI'] ?? '/', PHP_URL_PATH ) ) );
		} elseif ( 'ajax' === $context ) {
			homeboy_request_profiler_query_phase_stop( 'ajax.request', array( 'surface_type' => 'ajax', 'action' => $_REQUEST['action'] ?? '' ) );
		} elseif ( 'admin' === $context ) {
			homeboy_request_profiler_query_phase_stop( 'admin.page', array( 'surface_type' => 'admin', 'path' => wp_parse_url( $_SERVER['REQUEST_URI'] ?? '/', PHP_URL_PATH ) ) );
		} elseif ( 'cron' === $context ) {
			homeboy_request_profiler_query_phase_stop( 'cron.request', array( 'surface_type' => 'cron' ) );
		}

		homeboy_request_profiler_query_phase_stop( 'request', array( 'surface_type' => $context, 'status' => http_response_code() ) );
	},
	1000000,
	0
);
`;
}

function installWordPressRequestProfiler(sitePath, options = {}) {
	const paths = resolveProfilerPaths(sitePath, options);
	fs.mkdirSync(paths.muPluginsDir, { recursive: true });
	fs.mkdirSync(path.dirname(paths.artifactPath), { recursive: true });

	if (options.clearArtifact !== false && fs.existsSync(paths.artifactPath)) {
		fs.unlinkSync(paths.artifactPath);
	}

	fs.writeFileSync(paths.pluginPath, generateProfilerPlugin(options), 'utf8');
	return paths;
}

function uninstallWordPressRequestProfiler(sitePath, options = {}) {
	const paths = resolveProfilerPaths(sitePath, options);
	if (fs.existsSync(paths.pluginPath)) {
		fs.unlinkSync(paths.pluginPath);
	}
	if (options.removeArtifact === true && fs.existsSync(paths.artifactPath)) {
		fs.unlinkSync(paths.artifactPath);
	}
	return paths;
}

function parseWordPressRequestProfileJsonl(contents) {
	if (typeof contents !== 'string') {
		throw new TypeError('contents must be a string');
	}

	return contents
		.split(/\r?\n/)
		.map((line, index) => ({ line: line.trim(), lineNumber: index + 1 }))
		.filter(({ line }) => line !== '')
		.map(({ line, lineNumber }) => {
			try {
				return JSON.parse(line);
			} catch (error) {
				throw new Error(`Invalid WordPress request profile JSONL at line ${lineNumber}: ${error.message}`);
			}
		});
}

function collectWordPressRequestProfiles(sitePath, options = {}) {
	const paths = resolveProfilerPaths(sitePath, options);
	if (!fs.existsSync(paths.artifactPath)) {
		return [];
	}

	return parseWordPressRequestProfileJsonl(fs.readFileSync(paths.artifactPath, 'utf8'));
}

function numericValue(value, fallback = 0) {
	return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

function requestRowTime(row) {
	return numericValue(row?.t_ms, 0);
}

function requestRowStatus(row) {
	return row?.status ?? row?.data?.status ?? row?.data?.status_code ?? row?.data?.http_status;
}

function formatSummaryUrl(value, options = {}) {
	const formatter = options.formatUrl || options.redactUrl;
	return typeof formatter === 'function' ? formatter(value || '') : (value || '');
}

function normalizeLimit(value, fallback) {
	const limit = Number(value ?? fallback);
	return Number.isFinite(limit) && limit >= 0 ? Math.floor(limit) : fallback;
}

function groupWordPressRequestProfilerRows(rows = []) {
	if (!Array.isArray(rows)) {
		throw new TypeError('rows must be an array');
	}

	const byRequest = new Map();
	for (const row of rows) {
		const requestId = row?.request_id || 'unknown';
		if (!byRequest.has(requestId)) {
			byRequest.set(requestId, []);
		}
		byRequest.get(requestId).push(row);
	}

	return [...byRequest.entries()].map(([requestId, requestRows]) => ({
		request_id: requestId,
		rows: requestRows.sort((a, b) => requestRowTime(a) - requestRowTime(b)),
	}));
}

function summarizeHookRows(events, options = {}) {
	const hookLimit = normalizeLimit(options.hookLimit, 8);
	return events
		.filter((event) => event?.data?.hook)
		.map((event) => ({
			event: event.event || '',
			hook: event.data.hook,
			duration_ms: numericValue(event.data.duration_ms, 0),
			t_ms: requestRowTime(event),
			priority: event.data.priority,
		}))
		.sort((a, b) => (b.duration_ms - a.duration_ms) || (b.t_ms - a.t_ms))
		.slice(0, hookLimit);
}

function summarizeRequestEvents(events, options = {}) {
	const last = events[events.length - 1] || {};
	const first = events[0] || {};
	const status = events.map(requestRowStatus).find((value) => value !== undefined && value !== null);

	return {
		request_id: last.request_id || first.request_id || 'unknown',
		uri: formatSummaryUrl(last.uri || first.uri || '', options),
		method: last.method || first.method || '',
		duration_ms: requestRowTime(last),
		status: status ?? null,
		event_count: events.length,
		http_urls: events
			.filter((event) => event.event === 'http.request.start')
			.map((event) => formatSummaryUrl(event.data?.url || '', options))
			.filter(Boolean),
		hooks: summarizeHookRows(events, options),
	};
}

function summarizeWordPressRequestProfilerRows(rows = [], options = {}) {
	const requestLimit = normalizeLimit(options.limit, 80);
	const slowLimit = normalizeLimit(options.slowLimit, requestLimit);
	const timingRowLimit = normalizeLimit(options.timingRowLimit, 80);
	const slowThresholdMs = numericValue(options.slowThresholdMs, 0);
	const grouped = groupWordPressRequestProfilerRows(rows);
	const requests = grouped
		.map(({ rows: requestRows }) => summarizeRequestEvents(requestRows, options))
		.sort((a, b) => b.duration_ms - a.duration_ms);
	const hookRows = requests
		.flatMap((request) => request.hooks.map((hook) => ({
			...hook,
			request_id: request.request_id,
			uri: request.uri,
			method: request.method,
		})))
		.sort((a, b) => (b.duration_ms - a.duration_ms) || (b.t_ms - a.t_ms));
	const timingRows = rows
		.filter((row) => typeof row?.t_ms === 'number' && Number.isFinite(row.t_ms))
		.map((row) => ({
			request_id: row.request_id || 'unknown',
			event: row.event || '',
			uri: formatSummaryUrl(row.uri || '', options),
			method: row.method || '',
			t_ms: row.t_ms,
		}))
		.sort((a, b) => b.t_ms - a.t_ms)
		.slice(0, timingRowLimit);

	return {
		row_count: Array.isArray(rows) ? rows.length : 0,
		request_count: grouped.length,
		requests: requests.slice(0, requestLimit),
		slow_requests: requests.filter((request) => request.duration_ms >= slowThresholdMs).slice(0, slowLimit),
		hooks: hookRows.slice(0, normalizeLimit(options.hooksLimit, 80)),
		timing_rows: timingRows,
	};
}

function phaseSubject(data = {}) {
	return data.route || data.action || data.path || data.block_name || data.hook || data.phase || 'request';
}

function normalizeDbQueryPhaseRow(row = {}, options = {}) {
	if (row?.event !== 'db_query.phase.stop') {
		return undefined;
	}
	const data = row.data || {};
	const phase = data.phase || row.phase || 'request';
	const surfaceType = data.surface_type || data.context || 'wordpress';
	const subject = phaseSubject(data);
	const queryCount = numericValue(data.query_count, 0);
	const queryTimeMs = numericValue(data.query_time_ms, 0);
	const durationMs = numericValue(data.duration_ms, requestRowTime(row));
	const requestId = row.request_id || 'unknown';
	const operationId = [surfaceType, phase, subject].filter(Boolean).join(':');

	return {
		id: [requestId, phase, subject].filter(Boolean).join(':').replace(/\s+/g, '-'),
		request_id: requestId,
		surface_type: surfaceType,
		phase,
		subject: formatSummaryUrl(subject, options),
		operation_id: operationId,
		method: data.method || row.method || '',
		uri: formatSummaryUrl(row.uri || data.path || data.route || '', options),
		status: data.status ?? requestRowStatus(row) ?? null,
		duration_ms: durationMs,
		query_count: queryCount,
		query_time_ms: queryTimeMs,
		total_queries: numericValue(data.total_queries, 0),
		top_query_shapes: Array.isArray(data.top_query_shapes) ? data.top_query_shapes : [],
		metadata: {
			context: data.context,
			route: data.route,
			action: data.action,
			path: data.path,
			block_name: data.block_name,
		},
	};
}

function normalizeWordPressRequestDbQueryPhases(rows = [], options = {}) {
	if (!Array.isArray(rows)) {
		throw new TypeError('rows must be an array');
	}

	return rows
		.map((row) => normalizeDbQueryPhaseRow(row, options))
		.filter(Boolean)
		.sort((a, b) => (b.query_time_ms - a.query_time_ms) || (b.query_count - a.query_count) || (b.duration_ms - a.duration_ms));
}

function dbQueryPhaseObservation(phase, metric, value, unit, defaults = {}) {
	return {
		id: [defaults.idPrefix || 'wordpress-request-profiler', phase.id, metric].filter(Boolean).join(':'),
		family: metric === 'duration_ms' ? 'timing' : 'query',
		target_id: phase.surface_type,
		operation_id: phase.operation_id,
		phase: phase.phase,
		subject: phase.subject,
		metric,
		value,
		unit,
		sample_count: 1,
		metadata: {
			request_id: phase.request_id,
			uri: phase.uri,
			method: phase.method,
			status: phase.status,
			...phase.metadata,
		},
	};
}

function wordPressRequestDbQueryPhasesToFuzzObservationSet(rows = [], options = {}) {
	const phases = normalizeWordPressRequestDbQueryPhases(rows, options);
	const observations = phases.flatMap((phase) => [
		dbQueryPhaseObservation(phase, 'query_count', phase.query_count, 'count', options),
		dbQueryPhaseObservation(phase, 'query_time_ms', phase.query_time_ms, 'ms', options),
		dbQueryPhaseObservation(phase, 'duration_ms', phase.duration_ms, 'ms', options),
	]);

	return {
		schema: FUZZ_OBSERVATION_SET_SCHEMA,
		version: 1,
		id: options.id || 'wordpress-request-db-query-observations',
		label: options.label || 'WordPress request DB query observations',
		observations,
		metadata: {
			profiler: 'wordpress-request-profiler',
			phase_count: phases.length,
			...(options.metadata || {}),
		},
	};
}

function wordPressRequestDbQueryPhasesToFuzzHotspotSet(rows = [], options = {}) {
	const phases = normalizeWordPressRequestDbQueryPhases(rows, options).slice(0, normalizeLimit(options.limit, 20));
	const maxScore = phases.reduce((max, phase) => Math.max(max, phase.query_time_ms || phase.duration_ms || phase.query_count), 0) || 1;
	const items = phases.map((phase, index) => ({
		rank: index + 1,
		surface_key: phase.surface_type,
		operation_key: phase.operation_id,
		dimension: 'query',
		metric: options.metric || 'query_time_ms',
		value: phase.query_time_ms,
		unit: 'ms',
		sample_count: 1,
		relative_score: Number(((phase.query_time_ms || phase.duration_ms || phase.query_count) / maxScore).toFixed(6)),
		metadata: {
			request_id: phase.request_id,
			phase: phase.phase,
			subject: phase.subject,
			query_count: phase.query_count,
			duration_ms: phase.duration_ms,
			top_query_shapes: phase.top_query_shapes,
		},
	}));

	return {
		schema: FUZZ_HOTSPOT_SET_SCHEMA,
		version: 1,
		id: options.id || 'wordpress-request-db-query-hotspots',
		label: options.label || 'WordPress request DB query hotspots',
		dimension: 'query',
		metric: options.metric || 'query_time_ms',
		unit: 'ms',
		items,
		metadata: {
			profiler: 'wordpress-request-profiler',
			phase_count: phases.length,
			...(options.metadata || {}),
		},
	};
}

module.exports = {
	DEFAULT_ARTIFACT_RELATIVE_PATH,
	DEFAULT_HOOKS,
	DEFAULT_PLUGIN_FILE_NAME,
	DEFAULT_PRIORITY_BAND_HOOKS,
	FUZZ_HOTSPOT_SET_SCHEMA,
	FUZZ_OBSERVATION_SET_SCHEMA,
	collectWordPressRequestProfiles,
	generateProfilerPlugin,
	groupWordPressRequestProfilerRows,
	installWordPressRequestProfiler,
	normalizeWordPressRequestDbQueryPhases,
	parseWordPressRequestProfileJsonl,
	resolveProfilerPaths,
	summarizeWordPressRequestProfilerRows,
	uninstallWordPressRequestProfiler,
	wordPressRequestDbQueryPhasesToFuzzHotspotSet,
	wordPressRequestDbQueryPhasesToFuzzObservationSet,
};
