'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	BOOTSTRAP_TIMELINE_MARKER,
	DEFAULT_BOOTSTRAP_TIMELINE_ARTIFACT_RELATIVE_PATH,
	collectWordPressBootstrapTimeline,
	instrumentIndexPhp,
	instrumentWpSettingsPhp,
	installWordPressBootstrapTimeline,
	parseWordPressBootstrapTimelineJsonl,
	resolveWordPressBootstrapTimelinePaths,
	summarizeWordPressBootstrapTimeline,
	uninstallWordPressBootstrapTimeline,
} = require('../lib/wordpress-bootstrap-timeline');

const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-bootstrap-timeline-'));

try {
	fs.mkdirSync(path.join(fixture, 'wp-content'), { recursive: true });
	const indexSource = "<?php\ndefine( 'WP_USE_THEMES', true );\nrequire __DIR__ . '/wp-blog-header.php';\n";
	const wpSettingsSource = `<?php
require_wp_db();
wp_start_object_cache();
// Load most of WordPress.
require ABSPATH . WPINC . '/post.php';
do_action( 'muplugins_loaded' );
`;
	fs.writeFileSync(path.join(fixture, 'index.php'), indexSource, 'utf8');
	fs.writeFileSync(path.join(fixture, 'wp-settings.php'), wpSettingsSource, 'utf8');

	const instrumentedIndex = instrumentIndexPhp(indexSource);
	assert.match(instrumentedIndex, new RegExp(BOOTSTRAP_TIMELINE_MARKER));
	assert.match(instrumentedIndex, /homeboy_bootstrap_timeline_record\( 'entry.start' \)/);
	assert.equal(instrumentIndexPhp(instrumentedIndex), instrumentedIndex, 'index instrumentation is idempotent');

	const instrumentedWpSettings = instrumentWpSettingsPhp(wpSettingsSource);
	assert.match(instrumentedWpSettings, /wp-settings.start/);
	assert.match(instrumentedWpSettings, /wp-settings.after_require_wp_db/);
	assert.match(instrumentedWpSettings, /wp-settings.before_load_most/);
	assert.equal(instrumentWpSettingsPhp(instrumentedWpSettings), instrumentedWpSettings, 'wp-settings instrumentation is idempotent');

	const paths = installWordPressBootstrapTimeline(fixture);
	assert.equal(paths.artifactRelativePath, DEFAULT_BOOTSTRAP_TIMELINE_ARTIFACT_RELATIVE_PATH);
	assert.equal(fs.existsSync(paths.artifactPath), true);
	assert.equal(fs.existsSync(path.join(paths.backupDir, 'index.php.bak')), true);
	assert.match(fs.readFileSync(path.join(fixture, 'index.php'), 'utf8'), new RegExp(BOOTSTRAP_TIMELINE_MARKER));

	fs.writeFileSync(
		paths.artifactPath,
		[
			JSON.stringify({ event: 'entry.start', request_id: 'slow', uri: '/', method: 'GET', t_ms: 0 }),
			JSON.stringify({ event: 'wp-settings.start', request_id: 'slow', uri: '/', method: 'GET', t_ms: 3 }),
			JSON.stringify({ event: 'entry.shutdown', request_id: 'slow', uri: '/', method: 'GET', t_ms: 25 }),
			JSON.stringify({ event: 'entry.shutdown', request_id: 'fast', uri: '/wp-admin/', method: 'GET', t_ms: 5 }),
			'',
		].join('\n'),
		'utf8'
	);

	const rows = collectWordPressBootstrapTimeline(fixture);
	assert.equal(rows.length, 4);

	const summary = summarizeWordPressBootstrapTimeline(rows);
	assert.equal(summary[0].requestId, 'slow');
	assert.equal(summary[0].durationMs, 25);
	assert.deepEqual(summary[0].events.map((event) => event.deltaFromPreviousMs), [0, 3, 22]);

	assert.deepEqual(parseWordPressBootstrapTimelineJsonl('{"event":"one"}\n\n{"event":"two"}\n').map((row) => row.event), ['one', 'two']);
	assert.throws(
		() => parseWordPressBootstrapTimelineJsonl('{"event":"ok"}\nnot-json'),
		/Invalid WordPress bootstrap timeline JSONL at line 2/
	);
	assert.throws(
		() => resolveWordPressBootstrapTimelinePaths(fixture, { artifactRelativePath: '../outside.jsonl' }),
		/must stay inside/
	);
	assert.throws(() => summarizeWordPressBootstrapTimeline('nope'), /rows must be an array/);

	const uninstallResult = uninstallWordPressBootstrapTimeline(fixture);
	assert.equal(uninstallResult.files.every((file) => file.restored), true);
	assert.equal(fs.readFileSync(path.join(fixture, 'index.php'), 'utf8'), indexSource);
	assert.equal(fs.readFileSync(path.join(fixture, 'wp-settings.php'), 'utf8'), wpSettingsSource);
	assert.equal(fs.existsSync(paths.artifactPath), true, 'artifact is preserved by default');

	uninstallWordPressBootstrapTimeline(fixture, { removeArtifact: true });
	assert.equal(fs.existsSync(paths.artifactPath), false);

	console.log('WordPress bootstrap timeline smoke passed.');
} finally {
	fs.rmSync(fixture, { recursive: true, force: true });
}
