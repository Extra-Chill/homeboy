'use strict';

/**
 * Internal dependencies
 */
const { correlateBrowserAndWordPressTimings, normalizeUrl } = require('./timing-correlator');

const DEFAULT_RESOURCE_INCLUDE = [
	'/wp-json/',
	'?rest_route=',
	'/wp-admin/',
	'/wp-content/',
	'/wp-includes/',
];

function round(value) {
	return typeof value === 'number' && Number.isFinite(value) ? Math.round(value) : 0;
}

function isPlainObject(value) {
	return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function normalizeBaseUrl(baseUrl) {
	if (typeof baseUrl !== 'string' || baseUrl.trim() === '') {
		throw new TypeError('baseUrl must be a non-empty string');
	}
	return baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`;
}

function resolveWordPressUrl(baseUrl, value) {
	if (typeof value !== 'string' || value.trim() === '') {
		throw new TypeError('page url/path must be a non-empty string');
	}
	return new URL(value, normalizeBaseUrl(baseUrl)).toString();
}

function normalizeReadySpec(ready) {
	if (ready === undefined || ready === null) {
		return { state: 'domcontentloaded' };
	}
	if (typeof ready === 'string') {
		return { selector: ready };
	}
	if (!isPlainObject(ready)) {
		throw new TypeError('ready must be a string selector or object');
	}

	const normalized = { ...ready };
	if (!normalized.state && !normalized.selector && !normalized.frame && !normalized.frameSelector && !normalized.function) {
		normalized.state = 'domcontentloaded';
	}
	return normalized;
}

function normalizePageSpec(spec) {
	if (!isPlainObject(spec)) {
		throw new TypeError('page spec must be an object');
	}
	const url = spec.url || spec.path;
	if (typeof url !== 'string' || url.trim() === '') {
		throw new TypeError('page spec requires url or path');
	}
	const id = spec.id || url.replace(/^\/+/, '').replace(/[^A-Za-z0-9_.:-]+/g, '-').replace(/^-|-$/g, '') || 'home';

	return {
		...spec,
		id,
		url,
		label: spec.label || id,
		ready: normalizeReadySpec(spec.ready),
	};
}

function normalizePageManifest(manifest) {
	const pages = Array.isArray(manifest) ? manifest : manifest?.pages;
	if (!Array.isArray(pages)) {
		throw new TypeError('page manifest must be an array or object with pages array');
	}
	return pages.map(normalizePageSpec);
}

function shouldIncludeResource(url, options = {}) {
	const include = Array.isArray(options.includeResourceSubstrings)
		? options.includeResourceSubstrings
		: DEFAULT_RESOURCE_INCLUDE;
	const exclude = Array.isArray(options.excludeResourceSubstrings)
		? options.excludeResourceSubstrings
		: [];
	const value = String(url || '');

	if (exclude.some((needle) => value.includes(needle))) {
		return false;
	}
	if (include.length === 0) {
		return true;
	}
	return include.some((needle) => value.includes(needle));
}

function classifyResourceUrl(url) {
	const normalized = normalizeUrl(url, { lowercasePath: false });
	if (normalized.includes('/wp-json/') || normalized.includes('rest_route=')) {
		return 'rest';
	}
	if (normalized.includes('/wp-admin/')) {
		return 'admin';
	}
	if (normalized.includes('/wp-content/')) {
		return 'content-asset';
	}
	if (normalized.includes('/wp-includes/')) {
		return 'core-asset';
	}
	return 'other';
}

async function collectBrowserResourceTimings(page, options = {}) {
	if (!page || typeof page.evaluate !== 'function') {
		throw new TypeError('page must provide evaluate()');
	}
	const raw = await page.evaluate(() =>
		performance.getEntriesByType('resource').map((entry) => ({
			name: entry.name,
			initiatorType: entry.initiatorType,
			startTime: entry.startTime,
			duration: entry.duration,
			fetchStart: entry.fetchStart,
			requestStart: entry.requestStart,
			responseStart: entry.responseStart,
			responseEnd: entry.responseEnd,
			transferSize: entry.transferSize,
			encodedBodySize: entry.encodedBodySize,
			decodedBodySize: entry.decodedBodySize,
		}))
	);

	return raw
		.filter((entry) => shouldIncludeResource(entry.name, options))
		.map((entry) => ({
			...entry,
			url: entry.name,
			normalizedUrl: normalizeUrl(entry.name),
			kind: classifyResourceUrl(entry.name),
			ttfbMs: typeof entry.responseStart === 'number' && typeof entry.requestStart === 'number'
				? entry.responseStart - entry.requestStart
				: undefined,
		}));
}

async function waitForPageReady(page, ready, options = {}) {
	const spec = normalizeReadySpec(ready);
	const timeout = spec.timeout || options.timeout || 120000;

	if (spec.state && typeof page.waitForLoadState === 'function') {
		await page.waitForLoadState(spec.state, { timeout });
	}

	if (spec.selector) {
		await page.waitForSelector(spec.selector, {
			state: spec.selectorState || 'visible',
			timeout,
		});
	}

	if (spec.frame || spec.frameSelector) {
		const frameName = spec.frame?.name || spec.frameName;
		const frame = typeof page.frame === 'function'
			? page.frame(frameName ? { name: frameName } : spec.frame)
			: null;
		if (!frame) {
			throw new Error(`Ready frame not found for ${frameName || JSON.stringify(spec.frame)}`);
		}
		if (spec.frameState && typeof frame.waitForLoadState === 'function') {
			await frame.waitForLoadState(spec.frameState, { timeout });
		}
		if (spec.frameSelector) {
			await frame.waitForSelector(spec.frameSelector, {
				state: spec.frameSelectorState || 'visible',
				timeout,
			});
		}
	}

	if (spec.function) {
		if (typeof page.waitForFunction !== 'function') {
			throw new TypeError('ready.function requires page.waitForFunction()');
		}
		await page.waitForFunction(spec.function, spec.functionArg, { timeout });
	}
}

function summarizeResourceTimings(entries) {
	const resources = entries.map((entry) => ({
		url: entry.normalizedUrl || normalizeUrl(entry.name || entry.url),
		kind: entry.kind || classifyResourceUrl(entry.name || entry.url),
		initiatorType: entry.initiatorType,
		startMs: round(entry.startTime),
		durationMs: round(entry.duration),
		ttfbMs: round(entry.ttfbMs),
		transferSize: entry.transferSize || 0,
		encodedBodySize: entry.encodedBodySize || 0,
		decodedBodySize: entry.decodedBodySize || 0,
	}));

	const countsByKind = {};
	for (const resource of resources) {
		countsByKind[resource.kind] = (countsByKind[resource.kind] || 0) + 1;
	}

	return {
		count: resources.length,
		countsByKind,
		restCount: countsByKind.rest || 0,
		slowest: [...resources].sort((a, b) => b.durationMs - a.durationMs).slice(0, 20),
		resources,
	};
}

async function profileWordPressPage(input) {
	if (!input || typeof input !== 'object') {
		throw new TypeError('profileWordPressPage requires an input object');
	}
	const { page, baseUrl, wordpressProfilerRows = [], mark } = input;
	const spec = normalizePageSpec(input.spec || input.pageSpec || input.page || {});
	const url = resolveWordPressUrl(baseUrl, spec.url);
	const started = Date.now();

	if (!page || typeof page.goto !== 'function') {
		throw new TypeError('page must provide goto()');
	}

	const response = await page.goto(url, {
		waitUntil: spec.gotoWaitUntil || 'commit',
		timeout: spec.timeout || 120000,
	});
	if (typeof mark === 'function') {
		await mark(`${spec.id}_commit`);
	}

	await waitForPageReady(page, spec.ready, { timeout: spec.timeout || 120000 });
	if (typeof mark === 'function') {
		await mark(`${spec.id}_ready`);
	}

	const readyMs = Date.now() - started;
	const resources = await collectBrowserResourceTimings(page, spec.resources || {});
	const resourceSummary = summarizeResourceTimings(resources);
	const correlation = correlateBrowserAndWordPressTimings({
		browserTimings: resources,
		wordpressProfilerRows,
	});

	return {
		id: spec.id,
		label: spec.label,
		url,
		path: new URL(url).pathname + new URL(url).search,
		status: response && typeof response.status === 'function' ? response.status() : 0,
		readyMs,
		resources: resourceSummary,
		correlation,
	};
}

async function profileWordPressPages(input) {
	if (!input || typeof input !== 'object') {
		throw new TypeError('profileWordPressPages requires an input object');
	}
	const specs = normalizePageManifest(input.manifest || input.pages || []);
	const results = [];
	for (const spec of specs) {
		results.push(await profileWordPressPage({ ...input, spec }));
	}
	return {
		pages: results,
		topRestWaterfalls: [...results]
			.sort((a, b) => (b.resources.restCount - a.resources.restCount) || (b.readyMs - a.readyMs))
			.slice(0, input.topLimit || 10)
			.map((result) => ({
				id: result.id,
				path: result.path,
				readyMs: result.readyMs,
				restCount: result.resources.restCount,
				slowest: result.resources.slowest.filter((resource) => resource.kind === 'rest').slice(0, 5),
			})),
	};
}

module.exports = {
	DEFAULT_RESOURCE_INCLUDE,
	classifyResourceUrl,
	collectBrowserResourceTimings,
	normalizePageManifest,
	normalizePageSpec,
	profileWordPressPage,
	profileWordPressPages,
	resolveWordPressUrl,
	summarizeResourceTimings,
	waitForPageReady,
};
