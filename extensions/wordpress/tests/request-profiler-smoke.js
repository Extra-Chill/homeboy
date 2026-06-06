'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	DEFAULT_ARTIFACT_RELATIVE_PATH,
	collectWordPressRequestProfiles,
	generateProfilerPlugin,
	groupWordPressRequestProfilerRows,
	installWordPressRequestProfiler,
	parseWordPressRequestProfileJsonl,
	resolveProfilerPaths,
	summarizeWordPressRequestProfilerRows,
	uninstallWordPressRequestProfiler,
} = require('../lib/request-profiler');

const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-profiler-'));

try {
	fs.mkdirSync(path.join(fixture, 'wp-content'), { recursive: true });

	const plugin = generateProfilerPlugin();
	assert.match(plugin, /Plugin Name: Homeboy Request Profiler/);
	assert.match(plugin, /admin_init/);
	assert.match(plugin, /pre_http_request/);
	assert.match(plugin, /homeboy-profile\.jsonl/);

	const paths = installWordPressRequestProfiler(fixture);
	assert.equal(paths.artifactRelativePath, DEFAULT_ARTIFACT_RELATIVE_PATH);
	assert.equal(paths.pluginPath, path.join(fixture, 'wp-content', 'mu-plugins', 'homeboy-request-profiler.php'));
	assert.equal(fs.existsSync(paths.pluginPath), true);
	assert.equal(fs.existsSync(path.dirname(paths.artifactPath)), true);

	fs.writeFileSync(
		paths.artifactPath,
		[
			JSON.stringify({ event: 'request.start', request_id: 'abc', t_ms: 0 }),
			JSON.stringify({ event: 'hook', request_id: 'abc', t_ms: 12.3, data: { hook: 'init' } }),
			'',
		].join('\n'),
		'utf8'
	);

	const entries = collectWordPressRequestProfiles(fixture);
	assert.equal(entries.length, 2);
	assert.equal(entries[1].data.hook, 'init');

	const parsed = parseWordPressRequestProfileJsonl('{"event":"one"}\n\n{"event":"two"}\n');
	assert.deepEqual(parsed.map((entry) => entry.event), ['one', 'two']);

	const profileRows = [
		{ event: 'request.start', request_id: 'fast', method: 'GET', uri: '/wp-json/fast', t_ms: 0 },
		{ event: 'shutdown', request_id: 'fast', method: 'GET', uri: '/wp-json/fast', t_ms: 12, data: { status: 200 } },
		{ event: 'request.start', request_id: 'slow', method: 'POST', uri: '/wp-admin/admin-ajax.php', t_ms: 0 },
		{ event: 'http.request.start', request_id: 'slow', method: 'POST', uri: '/wp-admin/admin-ajax.php', t_ms: 20, data: { url: 'https://api.example.test/private?token=secret' } },
		{ event: 'hook.stop', request_id: 'slow', method: 'POST', uri: '/wp-admin/admin-ajax.php', t_ms: 55, data: { hook: 'init', duration_ms: 25 } },
		{ event: 'shutdown', request_id: 'slow', method: 'POST', uri: '/wp-admin/admin-ajax.php', t_ms: 80, status: 500 },
	];
	const grouped = groupWordPressRequestProfilerRows(profileRows);
	assert.equal(grouped.length, 2);
	assert.deepEqual(grouped.map((group) => group.request_id), ['fast', 'slow']);

	const summary = summarizeWordPressRequestProfilerRows(profileRows, {
		slowThresholdMs: 50,
		formatUrl: (value) => value.replace(/token=[^&]+/, 'token=redacted'),
	});
	assert.equal(summary.row_count, 6);
	assert.equal(summary.request_count, 2);
	assert.equal(summary.requests[0].request_id, 'slow');
	assert.equal(summary.requests[0].duration_ms, 80);
	assert.equal(summary.requests[0].status, 500);
	assert.deepEqual(summary.requests[0].http_urls, ['https://api.example.test/private?token=redacted']);
	assert.deepEqual(summary.requests[0].hooks[0], {
		event: 'hook.stop',
		hook: 'init',
		duration_ms: 25,
		t_ms: 55,
		priority: undefined,
	});
	assert.equal(summary.slow_requests.length, 1);
	assert.equal(summary.slow_requests[0].request_id, 'slow');
	assert.equal(summary.hooks[0].request_id, 'slow');
	assert.equal(summary.timing_rows[0].t_ms, 80);

	assert.throws(
		() => parseWordPressRequestProfileJsonl('{"event":"ok"}\nnot-json'),
		/Invalid WordPress request profile JSONL at line 2/
	);

	assert.throws(
		() => resolveProfilerPaths(fixture, { artifactRelativePath: '../outside.jsonl' }),
		/must stay inside/
	);

	uninstallWordPressRequestProfiler(fixture);
	assert.equal(fs.existsSync(paths.pluginPath), false);
	assert.equal(fs.existsSync(paths.artifactPath), true);

	uninstallWordPressRequestProfiler(fixture, { removeArtifact: true });
	assert.equal(fs.existsSync(paths.artifactPath), false);

	console.log('WordPress request profiler smoke passed.');
} finally {
	fs.rmSync(fixture, { recursive: true, force: true });
}
