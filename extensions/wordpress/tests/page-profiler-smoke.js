'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	classifyResourceUrl,
	classifyWordPressRestPreloadOpportunities,
	compareWordPressRestWaterfalls,
	diagnoseWordPressPageProfile,
	formatWordPressRestWaterfallMarkdownReport,
	normalizePageManifest,
	profileWordPressPage,
	profileWordPressPages,
	resourceFamily,
	resolveWordPressUrl,
	summarizeWordPressRestWaterfall,
	summarizeResourceTimings,
} = require('../lib/page-profiler');

class FakeFrame {
	constructor(calls) {
		this.calls = calls;
	}

	async waitForLoadState(state) {
		this.calls.push(['frame.waitForLoadState', state]);
	}

	async waitForSelector(selector) {
		this.calls.push(['frame.waitForSelector', selector]);
	}
}

class FakePage {
	constructor(resources) {
		this.resources = resources;
		this.calls = [];
		this.fakeFrame = new FakeFrame(this.calls);
	}

	async goto(url, options) {
		this.calls.push(['goto', url, options.waitUntil]);
		return { status: () => 200 };
	}

	async waitForLoadState(state) {
		this.calls.push(['waitForLoadState', state]);
	}

	async waitForSelector(selector) {
		this.calls.push(['waitForSelector', selector]);
	}

	frame(query) {
		this.calls.push(['frame', query?.name || query]);
		return this.fakeFrame;
	}

	async evaluate() {
		return this.resources;
	}
}

assert.equal(resolveWordPressUrl('https://example.test/site', '/wp-admin/index.php'), 'https://example.test/wp-admin/index.php');
assert.equal(classifyResourceUrl('https://example.test/wp-json/wp/v2/posts?context=edit'), 'rest');
assert.equal(classifyResourceUrl('https://example.test/wp-admin/load-styles.php'), 'admin');
assert.equal(classifyResourceUrl('https://example.test/wp-content/themes/theme/style.css'), 'content-asset');
assert.equal(resourceFamily('https://example.test/wp-includes/js/dist/block-editor.min.js?ver=1'), '/wp-includes/js/dist/block-editor.js');
assert.equal(resourceFamily('https://example.test/wp-content/plugins/data-machine/assets/admin.js?ver=1'), '/wp-content/plugins/data-machine');

const manifest = normalizePageManifest({
	pages: [
		{ id: 'dashboard', path: '/wp-admin/index.php', ready: '#dashboard-widgets' },
		{
			id: 'site-editor',
			path: '/wp-admin/site-editor.php',
			ready: { selector: 'iframe[name="editor-canvas"]', frameName: 'editor-canvas', frameSelector: '[data-block]' },
		},
	],
});
assert.equal(manifest.length, 2);
assert.equal(manifest[0].ready.selector, '#dashboard-widgets');
assert.equal(manifest[1].ready.frameSelector, '[data-block]');

const resources = [
	{
		name: 'https://example.test/wp-json/wp/v2/posts?context=edit',
		initiatorType: 'fetch',
		startTime: 20,
		duration: 180,
		requestStart: 30,
		responseStart: 140,
		responseEnd: 200,
		transferSize: 600000,
		encodedBodySize: 580000,
		decodedBodySize: 580000,
	},
	{
		name: 'https://example.test/wp-content/themes/twentytwentyfive/style.css',
		initiatorType: 'link',
		startTime: 5,
		duration: 20,
		requestStart: 6,
		responseStart: 10,
		responseEnd: 25,
		transferSize: 2000,
	},
	{
		name: 'https://example.test/favicon.ico',
		initiatorType: 'img',
		startTime: 8,
		duration: 5,
	},
	{
		name: 'https://example.test/wp-json/wp/v2/settings',
		initiatorType: 'fetch',
		startTime: 1300,
		duration: 90,
		requestStart: 1310,
		responseStart: 1360,
		responseEnd: 1390,
		transferSize: 3000,
	},
];

const summary = summarizeResourceTimings(resources.map((entry) => ({ ...entry, kind: classifyResourceUrl(entry.name) })));
	assert.equal(summary.count, 4);
	assert.equal(summary.countsByKind.rest, 2);
	assert.equal(summary.resources[0].responseEndMs, 200);

	const diagnosis = diagnoseWordPressPageProfile(
		{
			id: 'admin-dashboard',
			readyMs: 1200,
			resources: summary,
		},
		{
			browserMetrics: {
				admin_dashboard_ready_ms: 1500,
				browser_network_idle_ms: 4700,
				browser_request_count: 120,
				browser_failed_request_count: 1,
			},
			networkRequests: [
				{ url: 'https://example.test/wp-json/wp/v2/fail', method: 'GET', status: 500, failed: true, duration_ms: 120 },
			],
		}
	);
	assert.equal(diagnosis.summary.networkIdleAfterReadyMs, 3200);
	assert.equal(diagnosis.summary.lateRequestCount, 1);
	assert.equal(diagnosis.summary.restAfterReadyCount, 1);
	assert.equal(diagnosis.summary.failedRequestCount, 1);
	assert.equal(diagnosis.assets.heavyFamilies[0].family, '/wp-json/wp/v2/posts');
	assert.equal(diagnosis.findings.some((finding) => finding.code === 'network-active-after-ready'), true);
	assert.equal(diagnosis.findings.some((finding) => finding.code === 'rest-after-ready'), true);

	const baselineWaterfall = summarizeWordPressRestWaterfall({
		readyMs: 1000,
		apiFetchAttempts: [
			{ source: 'apiFetch', path: '/wp/v2/template-parts/twentytwentyfive//header?context=edit', method: 'GET', startedAtMs: 1100 },
			{ source: 'apiFetch', path: '/wp/v2/users/me?context=edit', method: 'GET', startedAtMs: 1150 },
		],
		networkRequests: [
			{ url: 'https://example.test/wp-json/wp/v2/template-parts/twentytwentyfive//header?context=edit', method: 'GET', status: 200, start_ms: 1102, duration_ms: 180 },
			{ url: 'https://example.test/wp-json/wp/v2/users/me?context=edit', method: 'GET', status: 200, start_ms: 1152, duration_ms: 90 },
		],
	});
	const candidateWaterfall = summarizeWordPressRestWaterfall({
		readyMs: 900,
		restPreloads: [
			{ path: '/wp/v2/template-parts/twentytwentyfive//header?context=edit', body: { ok: true } },
			{ path: '/wp/v2/posts?context=edit&per_page=10', payloadBytes: 256 },
		],
		apiFetchAttempts: [
			{ source: 'apiFetch', path: '/wp/v2/template-parts/twentytwentyfive//header?context=edit', method: 'GET', startedAtMs: 950, durationMs: 2 },
			{ source: 'apiFetch', path: '/wp/v2/users/me?context=edit', method: 'GET', startedAtMs: 980 },
		],
		networkRequests: [
			{ url: 'https://example.test/wp-json/wp/v2/users/me?context=edit', method: 'GET', status: 200, start_ms: 982, duration_ms: 90 },
		],
	});
	assert.equal(baselineWaterfall.counts.network, 2);
	assert.equal(candidateWaterfall.counts.preloadedOrCache, 1);
	assert.equal(candidateWaterfall.counts.usedPreload, 1);
	assert.equal(candidateWaterfall.counts.unusedPreloadCount, 1);
	assert.equal(candidateWaterfall.counts.preloadPayloadBytes, 267);
	assert.equal(candidateWaterfall.counts.remainingRestNetworkCount, 1);
	assert.equal(candidateWaterfall.metrics.unused_preload_count, 1);
	assert.equal(candidateWaterfall.metrics.preload_payload_bytes, 267);
	assert.equal(candidateWaterfall.metrics.remaining_rest_network_count, 1);
	assert.equal(candidateWaterfall.usedPreloadRows[0].url, '/wp/v2/template-parts/twentytwentyfive//header?context=edit');
	assert.equal(candidateWaterfall.unusedPreloadRows[0].url, '/wp/v2/posts?context=edit&per_page=10');
	assert.equal(candidateWaterfall.preloadedOrCacheRows[0].url, '/wp/v2/template-parts/twentytwentyfive//header?context=edit');
	const objectPreloadWaterfall = summarizeWordPressRestWaterfall({
		apiFetchAttempts: [{ path: '/wp/v2/settings', method: 'GET' }],
		preloadMetadata: { '/wp/v2/settings': { body: { setting: true } } },
	});
	assert.equal(objectPreloadWaterfall.counts.usedPreload, 1);
	const comparison = compareWordPressRestWaterfalls({ baseline: baselineWaterfall, candidate: candidateWaterfall });
	assert.equal(comparison.counts.removedNetwork, 1);
	assert.equal(comparison.remainingNetworkOpportunities[0].classification, 'client-state');
	assert.equal(comparison.unusedPreloadRows.length, 1);
	const opportunities = classifyWordPressRestPreloadOpportunities(baselineWaterfall);
	assert.equal(opportunities.safeDeterministic.length, 1);
	assert.equal(opportunities.clientState.length, 1);
	assert.match(formatWordPressRestWaterfallMarkdownReport(comparison), /REST waterfall comparison/);
	assert.match(formatWordPressRestWaterfallMarkdownReport(comparison), /Unused REST preloads/);

async function main() {
	const page = new FakePage(resources);
	const result = await profileWordPressPage({
		page,
		baseUrl: 'https://example.test',
		spec: manifest[1],
		wordpressProfilerRows: [
			{ event: 'request.start', request_id: 'r1', method: 'GET', uri: '/wp-json/wp/v2/posts?context=edit', t_ms: 0 },
			{ event: 'shutdown', request_id: 'r1', method: 'GET', uri: '/wp-json/wp/v2/posts?context=edit', t_ms: 95 },
		],
	});

	assert.equal(result.id, 'site-editor');
	assert.equal(result.status, 200);
	assert.equal(result.resources.restCount, 2);
	assert.equal(result.correlation.correlated.length, 1);
	assert.equal(page.calls.some((call) => call[0] === 'frame.waitForSelector' && call[1] === '[data-block]'), true);

	const multiPage = new FakePage(resources);
	const multi = await profileWordPressPages({ page: multiPage, baseUrl: 'https://example.test', manifest });
	assert.equal(multi.pages.length, 2);
	assert.equal(multi.topRestWaterfalls[0].restCount, 2);

	assert.throws(() => normalizePageManifest({ pages: [{ id: 'bad' }] }), /requires url or path/);

	console.log('WordPress page profiler smoke passed.');
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
