'use strict';

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

homeboy_request_profiler_write( 'request.start', array( 'php_sapi' => PHP_SAPI ) );

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

module.exports = {
	DEFAULT_ARTIFACT_RELATIVE_PATH,
	DEFAULT_HOOKS,
	DEFAULT_PLUGIN_FILE_NAME,
	DEFAULT_PRIORITY_BAND_HOOKS,
	collectWordPressRequestProfiles,
	generateProfilerPlugin,
	installWordPressRequestProfiler,
	parseWordPressRequestProfileJsonl,
	resolveProfilerPaths,
	uninstallWordPressRequestProfiler,
};
