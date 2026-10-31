'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const MARKER = 'HOMEBOY_BOOTSTRAP_TIMELINE';
const DEFAULT_ARTIFACT_RELATIVE_PATH = 'wp-content/homeboy-bootstrap-timeline.jsonl';
const DEFAULT_BACKUP_DIR_RELATIVE_PATH = 'wp-content/homeboy-bootstrap-timeline-backups';
const DEFAULT_BOOTSTRAP_MARKS = Object.freeze([
	{ search: 'require_wp_db();', event: 'wp-settings.after_require_wp_db' },
	{ search: 'wp_start_object_cache();', event: 'wp-settings.after_object_cache' },
	{ search: "require ABSPATH . WPINC . '/default-filters.php';", event: 'wp-settings.after_default_filters' },
	{ search: "register_shutdown_function( 'shutdown_action_hook' );", event: 'wp-settings.after_shutdown_hook' },
	{ search: "require_once ABSPATH . WPINC . '/class-wp-locale-switcher.php';", event: 'wp-settings.after_l10n_library' },
	{ search: 'wp_not_installed();', event: 'wp-settings.after_not_installed_check' },
	{ search: '// Load most of WordPress.', event: 'wp-settings.before_load_most', before: true },
	{ search: "require ABSPATH . WPINC . '/post.php';", event: 'wp-settings.after_post_core' },
	{ search: "require ABSPATH . WPINC . '/rest-api.php';", event: 'wp-settings.after_rest_api_base' },
	{
		search: "require ABSPATH . WPINC . '/rest-api/endpoints/class-wp-rest-navigation-fallback-controller.php';",
		event: 'wp-settings.after_rest_controllers',
	},
	{ search: "require ABSPATH . WPINC . '/blocks/index.php';", event: 'wp-settings.after_blocks_index' },
	{ search: "require ABSPATH . WPINC . '/speculative-loading.php';", event: 'wp-settings.after_load_most' },
	{ search: 'wp_plugin_directory_constants();', event: 'wp-settings.after_plugin_directory_constants' },
	{ search: 'unset( $mu_plugin, $_wp_plugin_file );', event: 'wp-settings.after_mu_plugins_included' },
	{ search: "do_action( 'muplugins_loaded' );", event: 'wp-settings.after_muplugins_loaded' },
]);

function normalizeSitePath(sitePath) {
	if (typeof sitePath !== 'string' || sitePath.trim() === '') {
		throw new TypeError('sitePath must be a non-empty string');
	}

	return path.resolve(sitePath);
}

function normalizeRelativePath(value, fallback, label) {
	const relativePath = value || fallback;
	if (typeof relativePath !== 'string' || relativePath.trim() === '') {
		throw new TypeError(`${label} must be a non-empty string`);
	}
	if (path.isAbsolute(relativePath)) {
		throw new Error(`${label} must be relative to the WordPress site path`);
	}

	const normalized = path.normalize(relativePath);
	if (normalized === '..' || normalized.startsWith(`..${path.sep}`)) {
		throw new Error(`${label} must stay inside the WordPress site path`);
	}

	return normalized;
}

function resolveWordPressBootstrapTimelinePaths(sitePath, options = {}) {
	const root = normalizeSitePath(sitePath);
	const artifactRelativePath = normalizeRelativePath(
		options.artifactRelativePath,
		DEFAULT_ARTIFACT_RELATIVE_PATH,
		'artifactRelativePath'
	);
	const backupDirRelativePath = normalizeRelativePath(
		options.backupDirRelativePath,
		DEFAULT_BACKUP_DIR_RELATIVE_PATH,
		'backupDirRelativePath'
	);

	return {
		sitePath: root,
		artifactRelativePath,
		artifactPath: path.join(root, artifactRelativePath),
		backupDirRelativePath,
		backupDir: path.join(root, backupDirRelativePath),
		indexPath: path.join(root, 'index.php'),
		wpSettingsPath: path.join(root, 'wp-settings.php'),
	};
}

function phpString(value) {
	return `'${String(value).replace(/\\/g, '\\\\').replace(/'/g, "\\'")}'`;
}

function generateIndexInstrumentation(options = {}) {
	const artifactRelativePath = normalizeRelativePath(
		options.artifactRelativePath,
		DEFAULT_ARTIFACT_RELATIVE_PATH,
		'artifactRelativePath'
	).replace(/\\/g, '/');

	return `
/* ${MARKER}: begin */
$GLOBALS['homeboy_bootstrap_timeline_start'] = microtime( true );
$GLOBALS['homeboy_bootstrap_timeline_id'] = function_exists( 'random_bytes' ) ? bin2hex( random_bytes( 8 ) ) : uniqid( '', true );
$GLOBALS['homeboy_bootstrap_timeline_uri'] = $_SERVER['REQUEST_URI'] ?? '';
$GLOBALS['homeboy_bootstrap_timeline_method'] = $_SERVER['REQUEST_METHOD'] ?? '';
$GLOBALS['homeboy_bootstrap_timeline_file'] = __DIR__ . '/' . ${phpString(artifactRelativePath)};
if ( ! function_exists( 'homeboy_bootstrap_timeline_record' ) ) {
	function homeboy_bootstrap_timeline_record( $event ) {
		$start = $GLOBALS['homeboy_bootstrap_timeline_start'] ?? null;
		$file  = $GLOBALS['homeboy_bootstrap_timeline_file'] ?? null;
		if ( ! $start || ! $file ) {
			return;
		}

		$dir = dirname( $file );
		if ( ! is_dir( $dir ) ) {
			mkdir( $dir, 0777, true );
		}

		file_put_contents(
			$file,
			json_encode(
				array(
					'v'          => 1,
					'event'      => $event,
					'request_id' => $GLOBALS['homeboy_bootstrap_timeline_id'] ?? '',
					'uri'        => $GLOBALS['homeboy_bootstrap_timeline_uri'] ?? '',
					'method'     => $GLOBALS['homeboy_bootstrap_timeline_method'] ?? '',
					't_ms'       => round( ( microtime( true ) - $start ) * 1000, 3 ),
					'time'       => microtime( true ),
				),
				JSON_UNESCAPED_SLASHES
			) . PHP_EOL,
			FILE_APPEND | LOCK_EX
		);
	}
}
homeboy_bootstrap_timeline_record( 'entry.start' );
register_shutdown_function(
	static function () {
		homeboy_bootstrap_timeline_record( 'entry.shutdown' );
	}
);
/* ${MARKER}: end */
`;
}

function generateWpSettingsInstrumentation() {
	return `
/* ${MARKER}: begin */
if ( ! function_exists( 'homeboy_bootstrap_timeline_mark' ) ) {
	function homeboy_bootstrap_timeline_mark( $event ) {
		if ( function_exists( 'homeboy_bootstrap_timeline_record' ) ) {
			homeboy_bootstrap_timeline_record( $event );
		}
	}
}
homeboy_bootstrap_timeline_mark( 'wp-settings.start' );
/* ${MARKER}: end */
`;
}

function instrumentIndexPhp(source, options = {}) {
	if (typeof source !== 'string') {
		throw new TypeError('source must be a string');
	}
	if (source.includes(MARKER)) {
		return source;
	}
	if (!source.includes('<?php')) {
		throw new Error('index.php source must contain an opening PHP tag');
	}

	return source.replace('<?php', `<?php${generateIndexInstrumentation(options)}`);
}

function normalizeBootstrapMarks(marks) {
	const list = marks === undefined ? DEFAULT_BOOTSTRAP_MARKS : marks;
	if (!Array.isArray(list)) {
		throw new TypeError('bootstrapMarks must be an array');
	}

	return list.map((mark) => {
		if (!mark || typeof mark.search !== 'string' || mark.search === '' || typeof mark.event !== 'string' || mark.event === '') {
			throw new TypeError('bootstrapMarks entries require non-empty search and event strings');
		}
		return { search: mark.search, event: mark.event, before: mark.before === true };
	});
}

function instrumentWpSettingsPhp(source, options = {}) {
	if (typeof source !== 'string') {
		throw new TypeError('source must be a string');
	}
	if (source.includes(MARKER)) {
		return source;
	}
	if (!source.includes('<?php')) {
		throw new Error('wp-settings.php source must contain an opening PHP tag');
	}

	let instrumented = source.replace('<?php', `<?php${generateWpSettingsInstrumentation()}`);
	for (const mark of normalizeBootstrapMarks(options.bootstrapMarks)) {
		const markerCall = `homeboy_bootstrap_timeline_mark( '${mark.event}' );`;
		if (!instrumented.includes(mark.search) || instrumented.includes(markerCall)) {
			continue;
		}
		instrumented = mark.before
			? instrumented.replace(mark.search, `${markerCall}\n${mark.search}`)
			: instrumented.replace(mark.search, `${mark.search}\n${markerCall}`);
	}

	return instrumented;
}

function backupFilePath(paths, fileName) {
	return path.join(paths.backupDir, `${fileName}.bak`);
}

function backupAndWrite(paths, fileName, transform) {
	const filePath = path.join(paths.sitePath, fileName);
	const backupPath = backupFilePath(paths, fileName);
	if (!fs.existsSync(filePath)) {
		throw new Error(`Cannot install WordPress bootstrap timeline: ${fileName} does not exist`);
	}

	const source = fs.readFileSync(filePath, 'utf8');
	if (!fs.existsSync(backupPath)) {
		fs.writeFileSync(backupPath, source, 'utf8');
	}
	const next = transform(source);
	if (next !== source) {
		fs.writeFileSync(filePath, next, 'utf8');
	}

	return { filePath, backupPath, changed: next !== source };
}

function installWordPressBootstrapTimeline(sitePath, options = {}) {
	const paths = resolveWordPressBootstrapTimelinePaths(sitePath, options);
	fs.mkdirSync(path.dirname(paths.artifactPath), { recursive: true });
	fs.mkdirSync(paths.backupDir, { recursive: true });
	if (options.clearArtifact !== false) {
		fs.writeFileSync(paths.artifactPath, '', 'utf8');
	}

	const files = [
		backupAndWrite(paths, 'index.php', (source) => instrumentIndexPhp(source, options)),
		backupAndWrite(paths, 'wp-settings.php', (source) => instrumentWpSettingsPhp(source, options)),
	];

	return { ...paths, files };
}

function uninstallWordPressBootstrapTimeline(sitePath, options = {}) {
	const paths = resolveWordPressBootstrapTimelinePaths(sitePath, options);
	const files = [];
	for (const fileName of ['index.php', 'wp-settings.php']) {
		const filePath = path.join(paths.sitePath, fileName);
		const backupPath = backupFilePath(paths, fileName);
		const result = { fileName, filePath, backupPath, restored: false, skipped: false };
		if (!fs.existsSync(backupPath)) {
			result.skipped = true;
			result.reason = 'backup-missing';
			files.push(result);
			continue;
		}

		const current = fs.existsSync(filePath) ? fs.readFileSync(filePath, 'utf8') : '';
		if (!options.forceRestore && current && !current.includes(MARKER)) {
			result.skipped = true;
			result.reason = 'marker-missing';
			files.push(result);
			continue;
		}

		fs.writeFileSync(filePath, fs.readFileSync(backupPath, 'utf8'), 'utf8');
		result.restored = true;
		files.push(result);
	}

	if (options.removeBackups !== false) {
		fs.rmSync(paths.backupDir, { recursive: true, force: true });
	}
	if (options.removeArtifact === true && fs.existsSync(paths.artifactPath)) {
		fs.unlinkSync(paths.artifactPath);
	}

	return { ...paths, files };
}

function parseWordPressBootstrapTimelineJsonl(contents) {
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
				throw new Error(`Invalid WordPress bootstrap timeline JSONL at line ${lineNumber}: ${error.message}`);
			}
		});
}

function collectWordPressBootstrapTimeline(sitePath, options = {}) {
	const paths = resolveWordPressBootstrapTimelinePaths(sitePath, options);
	if (!fs.existsSync(paths.artifactPath)) {
		return [];
	}

	return parseWordPressBootstrapTimelineJsonl(fs.readFileSync(paths.artifactPath, 'utf8'));
}

function summarizeWordPressBootstrapTimeline(rows, options = {}) {
	if (!Array.isArray(rows)) {
		throw new TypeError('rows must be an array');
	}
	const limit = Math.max(1, Number(options.limit || 40));
	const byRequest = new Map();
	for (const row of rows) {
		const id = row && row.request_id ? row.request_id : 'unknown';
		if (!byRequest.has(id)) {
			byRequest.set(id, []);
		}
		byRequest.get(id).push(row);
	}

	return [...byRequest.values()]
		.map((events) => {
			events.sort((a, b) => (Number(a.t_ms) || 0) - (Number(b.t_ms) || 0));
			const last = events[events.length - 1] || {};
			let previous = 0;
			return {
				requestId: last.request_id || 'unknown',
				uri: last.uri || '',
				method: last.method || '',
				durationMs: Number(last.t_ms) || 0,
				events: events.map((event) => {
					const tMs = Number(event.t_ms) || 0;
					const delta = tMs - previous;
					previous = tMs;
					return {
						event: event.event,
						tMs,
						deltaFromPreviousMs: delta,
					};
				}),
			};
		})
		.sort((a, b) => b.durationMs - a.durationMs)
		.slice(0, limit);
}

module.exports = {
	BOOTSTRAP_TIMELINE_MARKER: MARKER,
	DEFAULT_BOOTSTRAP_MARKS,
	DEFAULT_BOOTSTRAP_TIMELINE_ARTIFACT_RELATIVE_PATH: DEFAULT_ARTIFACT_RELATIVE_PATH,
	DEFAULT_BOOTSTRAP_TIMELINE_BACKUP_DIR_RELATIVE_PATH: DEFAULT_BACKUP_DIR_RELATIVE_PATH,
	collectWordPressBootstrapTimeline,
	generateIndexInstrumentation,
	generateWpSettingsInstrumentation,
	instrumentIndexPhp,
	instrumentWpSettingsPhp,
	installWordPressBootstrapTimeline,
	parseWordPressBootstrapTimelineJsonl,
	resolveWordPressBootstrapTimelinePaths,
	summarizeWordPressBootstrapTimeline,
	uninstallWordPressBootstrapTimeline,
};
