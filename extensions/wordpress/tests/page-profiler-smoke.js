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
	compareWordPressRestNetworkWaterfalls,
	compareWordPressRestWaterfalls,
	diagnoseWordPressPageProfile,
	formatWordPressPerformanceGateReport,
	formatWordPressRestNetworkDiffMarkdownReport,
	formatWordPressRestWaterfallMarkdownReport,
	normalizeBrowserAction,
	normalizePageManifest,
	profileWordPressPage,
	profileWordPressPages,
	recommendWordPressPerformanceGates,
	resourceFamily,
	resolveWordPressUrl,
	runBrowserActions,
	summarizeWordPressRestNetworkRows,
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

	async click(selector) {
		this.calls.push(['click', selector]);
	}

	locator(selector) {
		this.calls.push(['locator', selector]);
		return new FakeLocator(this.calls, selector);
	}

	getByRole(role, options) {
		this.calls.push(['getByRole', role, options?.name]);
		return new FakeLocator(this.calls, `role:${role}`);
	}

	getByText(text, options) {
		this.calls.push(['getByText', text, options?.exact]);
		return new FakeLocator(this.calls, `text:${text}`);
	}

	async waitForResponse(predicate) {
		this.calls.push(['waitForResponse']);
		const response = {
			url: () => 'https://example.test/wp-json/datamachine/v1/jobs',
			status: () => 200,
			request: () => ({ method: () => 'GET' }),
		};
		if (!predicate(response)) {
			throw new Error('response predicate did not match');
		}
		return response;
	}

	frame(query) {
		this.calls.push(['frame', query?.name || query]);
		return this.fakeFrame;
	}

	async evaluate() {
		return this.resources;
	}
}

class FakeLocator {
	constructor(calls, selector) {
		this.calls = calls;
		this.selector = selector;
	}

	nth(index) {
		this.calls.push(['locator.nth', this.selector, index]);
		return this;
	}

	async click() {
		this.calls.push(['locator.click', this.selector]);
	}

	async fill(value) {
		this.calls.push(['locator.fill', this.selector, value]);
	}

	async selectOption(value) {
		this.calls.push(['locator.selectOption', this.selector, value]);
	}

	async waitFor(options) {
		this.calls.push(['locator.waitFor', this.selector, options?.state]);
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
			interactions: [
				{ name: 'open_admin', clickRole: { role: 'button', name: 'Admin' } },
				{ waitForSelector: '.datamachine-jobs-admin-modal' },
				{ select: { selector: '.datamachine-jobs-admin-modal select', index: 0, value: 'flow' } },
				{ select: { selector: '.datamachine-jobs-admin-modal select', index: 1, optionIndex: 1 } },
				{ fill: { selector: '.datamachine-filter', value: 'queued' } },
				{ waitForResponse: { substring: '/wp-json/datamachine/v1/jobs', status: 200 } },
			],
		},
	],
});
assert.equal(manifest.length, 2);
assert.equal(manifest[0].ready.selector, '#dashboard-widgets');
assert.equal(manifest[1].ready.frameSelector, '[data-block]');
assert.equal(normalizeBrowserAction({ clickText: 'Save' }, 0).target, 'text:Save');

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
	const invalidJsonWaterfall = summarizeWordPressRestWaterfall({
		apiFetchAttempts: [
			{
				source: 'apiFetch',
				path: '/datamachine/v1/pipelines',
				method: 'GET',
				failed: true,
				error: 'The response is not a valid JSON response.',
				errorCode: 'invalid_json',
			},
			{
				source: 'fetch',
				url: 'https://example.test/wp-json/datamachine/v1/pipelines',
				method: 'GET',
				status: 200,
				responseContentType: 'text/html; charset=UTF-8',
				responseBodySample: '<br />Unexpected debug output{"ok":true}',
			},
		],
	});
	assert.equal(invalidJsonWaterfall.apiFetchAttempts.length, 1);
	assert.equal(invalidJsonWaterfall.apiFetchAttempts[0].errorCode, 'invalid_json');
	assert.match(invalidJsonWaterfall.rows[0].responseBodySample, /Unexpected debug output/);
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

	const baselineRestNetwork = [
		{ url: 'https://example.test/wp-json/wp/v2/template-parts/twentytwentyfive//header?context=edit&_locale=user', method: 'GET', status: 200, duration_ms: 340 },
		{ url: 'https://example.test/wp-json/wp/v2/template-parts/twentytwentyfive//footer?context=edit&_locale=user', method: 'GET', status: 200, duration_ms: 320 },
		{ url: 'https://example.test/wp-json/wp/v2/posts?context=edit&per_page=3&_locale=user', method: 'GET', status: 200, duration_ms: 360 },
		{ url: 'https://example.test/wp-json/wp/v2/settings?_locale=user', method: 'OPTIONS', status: 200, duration_ms: 120 },
		{ url: 'https://example.test/wp-json/wp/v2/pages?context=view&parent=0&per_page=100&_locale=user', method: 'GET', status: 200, duration_ms: 210 },
		{ url: 'https://example.test/wp-json/wp/v2/users/me?context=edit&_locale=user', method: 'GET', status: 200, duration_ms: 80 },
		{ url: 'https://example.test/wp-json/wp/v2/wp_pattern_category?context=view&per_page=100&_fields=id%2Cname&_locale=user', method: 'GET', status: 200, duration_ms: 90 },
	];
	const candidateRestNetwork = [
		{ url: 'https://candidate.test/wp-json/wp/v2/settings?_locale=user', method: 'OPTIONS', status: 200, duration_ms: 100 },
		{ url: 'https://candidate.test/wp-json/wp/v2/pages?context=view&parent=0&per_page=100&_locale=user', method: 'GET', status: 200, duration_ms: 190 },
		{ url: 'https://candidate.test/wp-json/wp/v2/users/me?context=edit&_locale=user', method: 'GET', status: 200, duration_ms: 70 },
		{ url: 'https://candidate.test/wp-json/wp/v2/wp_pattern_category?context=view&per_page=100&_fields=id%2Cname&_locale=user', method: 'GET', status: 200, duration_ms: 95 },
		{ url: 'https://candidate.test/wp-json/wp/v2/navigation?context=edit&_locale=user', method: 'GET', status: 200, duration_ms: 150 },
	];
	const baselineRestSummary = summarizeWordPressRestNetworkRows(baselineRestNetwork);
	assert.equal(baselineRestSummary.count, 7);
	assert.equal(baselineRestSummary.uniqueCount, 7);
	assert.equal(baselineRestSummary.routes.find((row) => row.url.includes('/template-parts/')).classification, 'template-data');
	assert.equal(baselineRestSummary.routes.find((row) => row.method === 'OPTIONS').classification, 'options-schema');
	assert.equal(baselineRestSummary.routes.find((row) => row.url.includes('/pages')).classification, 'page-tree');
	assert.equal(baselineRestSummary.routes.find((row) => row.url.includes('/users/me')).classification, 'current-user');
	assert.equal(baselineRestSummary.routes.find((row) => row.url.includes('wp_pattern_category')).classification, 'pattern-data');
	const restNetworkDiff = compareWordPressRestNetworkWaterfalls({ baseline: baselineRestNetwork, candidate: candidateRestNetwork });
	assert.equal(restNetworkDiff.counts.baseline, 7);
	assert.equal(restNetworkDiff.counts.candidate, 5);
	assert.equal(restNetworkDiff.counts.removed, 3);
	assert.equal(restNetworkDiff.counts.removedUnique, 3);
	assert.equal(restNetworkDiff.counts.added, 1);
	assert.equal(restNetworkDiff.removedRoutes.some((row) => row.url.includes('/posts')), true);
	assert.equal(restNetworkDiff.addedRoutes[0].classification, 'navigation-state');
	assert.match(formatWordPressRestNetworkDiffMarkdownReport(restNetworkDiff), /WordPress REST network diff/);
	assert.match(formatWordPressRestNetworkDiffMarkdownReport(restNetworkDiff), /template-data/);

	const baselineProfile = {
		readyMs: 1000,
		restWaterfall: baselineWaterfall,
		diagnosis: {
			summary: {
				failedRequestCount: 0,
				networkIdleAfterReadyMs: 1200,
				restAfterReadyCount: baselineWaterfall.counts.afterReady,
			},
		},
		metrics: {
			consoleErrorCount: 0,
		},
		correlation: {
			correlated: [
				{ wordpressDurationMs: 90 },
			],
		},
	};
	const candidateProfile = {
		readyMs: 1100,
		restWaterfall: candidateWaterfall,
		diagnosis: {
			summary: {
				failedRequestCount: 0,
				networkIdleAfterReadyMs: 1300,
				restAfterReadyCount: candidateWaterfall.counts.afterReady,
			},
		},
		metrics: {
			consoleErrorCount: 0,
			unusedPreloadCount: 0,
			preloadPayloadBytes: 12000,
		},
		correlation: {
			correlated: [
				{ wordpressDurationMs: 120 },
			],
		},
	};
	const gateRecommendations = recommendWordPressPerformanceGates({
		baseline: baselineProfile,
		candidate: candidateProfile,
		comparison,
	});
	assert.equal(gateRecommendations.recommendations.length, 9);
	assert.equal(gateRecommendations.recommended.length, 9);
	assert.equal(gateRecommendations.recommendations.find((gate) => gate.id === 'wordpress.rest_network_count').threshold, 2);
	assert.equal(gateRecommendations.recommendations.find((gate) => gate.id === 'wordpress.ready_ms').threshold, 1250);
	assert.equal(gateRecommendations.recommendations.find((gate) => gate.id === 'wordpress.server_request_duration_ms').threshold, 190);
	assert.match(formatWordPressPerformanceGateReport(gateRecommendations), /WordPress performance gate recommendations/);
	assert.match(formatWordPressPerformanceGateReport(gateRecommendations), /wordpress_rest_network_count/);

	const skippedGateRecommendations = recommendWordPressPerformanceGates({ comparison });
	assert.equal(skippedGateRecommendations.recommendations.find((gate) => gate.id === 'wordpress.rest_network_count').status, 'recommended');
	assert.equal(skippedGateRecommendations.recommendations.find((gate) => gate.id === 'wordpress.late_rest_count').status, 'skipped');
	assert.match(formatWordPressPerformanceGateReport(skippedGateRecommendations), /Skipped gates/);

async function main() {
	const actionPage = new FakePage(resources);
	const actionEvidence = await runBrowserActions(actionPage, [
		{ click: '.button-primary' },
		{ clickText: { text: 'Open', exact: true } },
		{ sleep: 1 },
	]);
	assert.equal(actionEvidence.actions.length, 3);
	assert.equal(actionEvidence.actions[0].status, 'passed');
	assert.equal(actionPage.calls.some((call) => call[0] === 'click' && call[1] === '.button-primary'), true);

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
	assert.equal(result.initialResources.restCount, 2);
	assert.equal(result.interactions.actions.length, 6);
	assert.equal(result.interactionResources.restCount, 2);
	assert.equal(result.interactionRestWaterfall.counts.total, 2);
	assert.equal(result.correlation.correlated.length, 1);
	assert.equal(page.calls.some((call) => call[0] === 'frame.waitForSelector' && call[1] === '[data-block]'), true);
	assert.equal(page.calls.some((call) => call[0] === 'getByRole' && call[1] === 'button' && call[2] === 'Admin'), true);
	assert.equal(page.calls.some((call) => call[0] === 'locator.selectOption' && call[2]?.index === 1), true);

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
