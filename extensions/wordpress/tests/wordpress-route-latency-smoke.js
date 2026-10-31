'use strict';

const assert = require('node:assert/strict');

const {
	averageBootstrapDeltas,
	formatWordPressRouteLatencyMarkdown,
	normalizeRouteSpec,
	normalizeWordPressRouteUri,
	profileWordPressRoutes,
	routeLabel,
	routeMatches,
	summarizePriorityBands,
	summarizeProfilerRows,
	summarizeWordPressRouteLatency,
} = require('../lib/wordpress-route-latency');

assert.equal(routeLabel('/wp-json/wp/v2/types/post?context=edit'), 'wp_json_wp_v2_types_post');
assert.equal(normalizeWordPressRouteUri('https://example.test/wp-json/wp/v2/types?_locale=user&context=edit'), '/wp-json/wp/v2/types?context=edit');
assert.equal(routeMatches('/wp-json/wp/v2/types', '/wp-json/wp/v2/types/post?context=edit'), true);
assert.equal(routeMatches('/', '/wp-json/'), false, 'front page route should match only the front page');
assert.equal(routeMatches({ route: 'types', match: '/wp-json/wp/v2/types', exact: true }, '/wp-json/wp/v2/types/post'), false);
assert.deepEqual(normalizeRouteSpec('/wp-json/'), {
	route: '/wp-json/',
	label: 'wp_json',
	match: '/wp-json/',
	exact: false,
});

const profilerRows = [
	{ event: 'request.start', request_id: 'r1', uri: '/wp-json/wp/v2/types/post?context=edit&_locale=user', method: 'GET', t_ms: 0 },
	{ event: 'hook.priority_band.start', request_id: 'r1', uri: '/wp-json/wp/v2/types/post?context=edit&_locale=user', method: 'GET', t_ms: 10, data: { hook: 'admin_init' } },
	{ event: 'hook.priority_band.end', request_id: 'r1', uri: '/wp-json/wp/v2/types/post?context=edit&_locale=user', method: 'GET', t_ms: 35, data: { hook: 'admin_init' } },
	{ event: 'shutdown', request_id: 'r1', uri: '/wp-json/wp/v2/types/post?context=edit&_locale=user', method: 'GET', t_ms: 80 },
	{ event: 'request.start', request_id: 'r2', uri: '/wp-json/wp/v2/blocks?context=edit', method: 'GET', t_ms: 0 },
	{ event: 'shutdown', request_id: 'r2', uri: '/wp-json/wp/v2/blocks?context=edit', method: 'GET', t_ms: 140 },
];

const profilerSummaries = summarizeProfilerRows(profilerRows);
assert.equal(profilerSummaries.length, 2);
assert.equal(profilerSummaries[0].duration_ms, 80);
assert.deepEqual(profilerSummaries[0].priority_bands, [{ hook: 'admin_init', duration_ms: 25 }]);
assert.deepEqual(summarizePriorityBands(profilerRows)[0], { hook: 'admin_init', duration_ms: 25 });

const bootstrapSummaries = [
	{
		uri: '/wp-json/wp/v2/types/post?context=edit',
		durationMs: 70,
		events: [
			{ event: 'entry.start', deltaFromPreviousMs: 0 },
			{ event: 'wp-settings.after_rest_api_base', deltaFromPreviousMs: 18 },
			{ event: 'entry.shutdown', deltaFromPreviousMs: 52 },
		],
	},
	{
		uri: '/wp-json/wp/v2/types/post?context=edit',
		durationMs: 90,
		events: [
			{ event: 'entry.start', delta_from_previous_ms: 0 },
			{ event: 'wp-settings.after_rest_api_base', delta_from_previous_ms: 22 },
			{ event: 'entry.shutdown', delta_from_previous_ms: 68 },
		],
	},
];

assert.deepEqual(averageBootstrapDeltas(bootstrapSummaries, { limit: 1 }), [
	{ event: 'entry.shutdown', avg_delta_ms: 60 },
]);

const routeSummaries = summarizeWordPressRouteLatency({
	routes: [
		'/wp-json/wp/v2/types',
		{ route: 'blocks', match: '/wp-json/wp/v2/blocks', label: 'Blocks' },
	],
	browserResults: [
		{ route: '/wp-json/wp/v2/types', status: 200, total_ms: 120, headers_ms: 90, body_bytes: 2000 },
		{ url: 'https://example.test/wp-json/wp/v2/types/post?context=edit', status: 200, durationMs: 160, ttfbMs: 110, transferSizeBytes: 2400 },
		{ url: 'https://example.test/wp-json/wp/v2/blocks?context=edit', statusCode: 500, totalMs: 240, headersMs: 220, bodyBytes: 1200 },
	],
	wordpressProfilerRows: profilerRows,
	bootstrapSummaries,
});

assert.equal(routeSummaries.length, 2);
assert.equal(routeSummaries[0].label, 'wp_json_wp_v2_types');
assert.equal(routeSummaries[0].n, 2);
assert.deepEqual(routeSummaries[0].status_codes, [200]);
assert.equal(routeSummaries[0].avg_total_ms, 140);
assert.equal(routeSummaries[0].avg_headers_ms, 100);
assert.equal(routeSummaries[0].avg_body_bytes, 2200);
assert.equal(routeSummaries[0].avg_wordpress_muplugin_to_shutdown_ms, 80);
assert.equal(routeSummaries[0].avg_entry_to_shutdown_ms, 80);
assert.equal(routeSummaries[0].avg_outer_ms, 60);
assert.equal(routeSummaries[0].wordpress_profile_count, 1);
assert.equal(routeSummaries[0].bootstrap_profile_count, 2);
assert.deepEqual(routeSummaries[0].slowest_priority_bands, [{ hook: 'admin_init', duration_ms: 25 }]);
assert.equal(routeSummaries[1].label, 'Blocks');
assert.deepEqual(routeSummaries[1].status_codes, [500]);

const markdown = formatWordPressRouteLatencyMarkdown(routeSummaries, { title: 'REST Route Latency' });
assert.match(markdown, /## REST Route Latency/);
assert.match(markdown, /`\/wp-json\/wp\/v2\/types`/);
assert.match(markdown, /140\.0/);

(async () => {
	const profiled = await profileWordPressRoutes({
		routes: ['/wp-json/'],
		iterations: 2,
		warmupIterations: 1,
		requestRoute: async (route, context) => ({
			url: route,
			status: context.warmup ? 204 : 200,
			totalMs: context.warmup ? 1 : 10 + context.iteration,
		}),
		wordpressProfilerRows: [
			{ event: 'request.start', request_id: 'p1', uri: '/wp-json/', method: 'GET', t_ms: 0 },
			{ event: 'shutdown', request_id: 'p1', uri: '/wp-json/', method: 'GET', t_ms: 4 },
		],
	});

	assert.equal(profiled.browserResults.length, 2, 'warmups are not included in browser results');
	assert.equal(profiled.routes[0].n, 2);
	assert.equal(profiled.routes[0].avg_total_ms, 10.5);
	console.log('WordPress route latency smoke passed.');
})().catch((error) => {
	console.error(error);
	process.exitCode = 1;
});
