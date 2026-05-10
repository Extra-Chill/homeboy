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
	normalizePageManifest,
	profileWordPressPage,
	profileWordPressPages,
	resolveWordPressUrl,
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
		transferSize: 1000,
	},
	{
		name: 'https://example.test/wp-content/themes/twentytwentyfive/style.css',
		initiatorType: 'link',
		startTime: 5,
		duration: 20,
		requestStart: 6,
		responseStart: 10,
		responseEnd: 25,
	},
	{
		name: 'https://example.test/favicon.ico',
		initiatorType: 'img',
		startTime: 8,
		duration: 5,
	},
];

const summary = summarizeResourceTimings(resources.map((entry) => ({ ...entry, kind: classifyResourceUrl(entry.name) })));
assert.equal(summary.count, 3);
assert.equal(summary.countsByKind.rest, 1);

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
	assert.equal(result.resources.restCount, 1);
	assert.equal(result.correlation.correlated.length, 1);
	assert.equal(page.calls.some((call) => call[0] === 'frame.waitForSelector' && call[1] === '[data-block]'), true);

	const multiPage = new FakePage(resources);
	const multi = await profileWordPressPages({ page: multiPage, baseUrl: 'https://example.test', manifest });
	assert.equal(multi.pages.length, 2);
	assert.equal(multi.topRestWaterfalls[0].restCount, 1);

	assert.throws(() => normalizePageManifest({ pages: [{ id: 'bad' }] }), /requires url or path/);

	console.log('WordPress page profiler smoke passed.');
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
