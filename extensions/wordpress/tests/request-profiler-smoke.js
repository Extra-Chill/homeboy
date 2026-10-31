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
	normalizeWordPressRequestDbQueryPhases,
	parseWordPressRequestProfileJsonl,
	resolveProfilerPaths,
	summarizeWordPressRequestProfilerRows,
	uninstallWordPressRequestProfiler,
	wordPressRequestDbQueryPhasesToFuzzHotspotSet,
	wordPressRequestDbQueryPhasesToFuzzObservationSet,
} = require('../lib/request-profiler');

const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-profiler-'));

try {
	fs.mkdirSync(path.join(fixture, 'wp-content'), { recursive: true });

	const plugin = generateProfilerPlugin();
	assert.match(plugin, /Plugin Name: Homeboy Request Profiler/);
	assert.match(plugin, /admin_init/);
	assert.match(plugin, /pre_http_request/);
	assert.match(plugin, /homeboy-profile\.jsonl/);
	assert.match(plugin, /db_query\.phase\.stop/);
	assert.match(plugin, /rest_pre_dispatch/);
	assert.match(plugin, /template_redirect/);
	assert.match(plugin, /ajax\.request/);
	assert.match(plugin, /admin\.page/);
	assert.match(plugin, /block\.render/);
	assert.match(plugin, /cron\.request/);
	assert.match(plugin, /homeboy_request_profiler_top_query_shapes/);

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

	const queryPhaseRows = [
		{
			event: 'db_query.phase.stop',
			request_id: 'rest-1',
			method: 'GET',
			uri: '/wp-json/wp/v2/posts',
			data: {
				phase: 'rest.request',
				surface_type: 'rest',
				route: '/wp/v2/posts',
				status: 200,
				duration_ms: 31,
				query_count: 7,
				query_time_ms: 5.25,
				total_queries: 18,
				top_query_shapes: [{ sql: 'SELECT * FROM wp_posts WHERE ID = ?', count: 2, time_ms: 3.1 }],
			},
		},
		{
			event: 'db_query.phase.stop',
			request_id: 'page-1',
			method: 'GET',
			uri: '/about/?token=secret',
			data: {
				phase: 'frontend.page',
				surface_type: 'frontend',
				path: '/about/',
				duration_ms: 44,
				query_count: 11,
				query_time_ms: 9,
			},
		},
		{
			event: 'db_query.phase.stop',
			request_id: 'admin-1',
			method: 'GET',
			uri: '/wp-admin/edit.php',
			data: {
				phase: 'admin.page',
				surface_type: 'admin',
				path: '/wp-admin/edit.php',
				duration_ms: 71,
				query_count: 21,
				query_time_ms: 13,
			},
		},
		{
			event: 'db_query.phase.stop',
			request_id: 'ajax-1',
			method: 'POST',
			uri: '/wp-admin/admin-ajax.php',
			data: {
				phase: 'ajax.request',
				surface_type: 'ajax',
				action: 'heartbeat',
				status: 200,
				duration_ms: 19,
				query_count: 3,
				query_time_ms: 2,
			},
		},
		{ event: 'hook', request_id: 'ignored', data: { hook: 'init' } },
	];
	const phases = normalizeWordPressRequestDbQueryPhases(queryPhaseRows, {
		formatUrl: (value) => value.replace(/token=[^&]+/, 'token=redacted'),
	});
	assert.deepEqual(phases.map((phase) => phase.phase), ['admin.page', 'frontend.page', 'rest.request', 'ajax.request']);
	assert.equal(phases[0].surface_type, 'admin');
	assert.equal(phases[1].subject, '/about/');
	assert.equal(phases[2].operation_id, 'rest:rest.request:/wp/v2/posts');
	assert.equal(phases[3].metadata.action, 'heartbeat');

	const observationSet = wordPressRequestDbQueryPhasesToFuzzObservationSet(queryPhaseRows, { idPrefix: 'contract' });
	assert.equal(observationSet.schema, 'homeboy/fuzz-observation-set/v1');
	assert.equal(observationSet.observations.length, 12);
	assert.deepEqual(
		observationSet.observations
			.filter((observation) => observation.operation_id === 'ajax:ajax.request:heartbeat')
			.map((observation) => observation.metric),
		['query_count', 'query_time_ms', 'duration_ms']
	);

	const hotspotSet = wordPressRequestDbQueryPhasesToFuzzHotspotSet(queryPhaseRows, { limit: 2 });
	assert.equal(hotspotSet.schema, 'homeboy/fuzz-hotspot-set/v1');
	assert.equal(hotspotSet.items.length, 2);
	assert.equal(hotspotSet.items[0].operation_key, 'admin:admin.page:/wp-admin/edit.php');
	assert.equal(hotspotSet.items[0].metadata.query_count, 21);
	assert.equal(hotspotSet.items[0].metadata.top_query_shapes.length, 0);

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
