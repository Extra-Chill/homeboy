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

const DEFAULT_DIAGNOSIS_THRESHOLDS = {
	readyMs: 1000,
	networkIdleGapMs: 2000,
	requestCount: 100,
	lateRequestCount: 5,
	restAfterReadyCount: 1,
	assetBytes: 500000,
	failedRequestCount: 1,
};

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

function resourceFamily(url) {
	const normalized = normalizeUrl(url, { lowercasePath: false }).split('?', 1)[0];
	const parts = normalized.split('/').filter(Boolean);
	const wpIncludes = parts.indexOf('wp-includes');
	if (wpIncludes >= 0 && parts[wpIncludes + 1] === 'js' && parts[wpIncludes + 2] === 'dist' && parts[wpIncludes + 3]) {
		return `/wp-includes/js/dist/${parts[wpIncludes + 3].replace(/\.min\.js$/, '.js')}`;
	}
	const wpContent = parts.indexOf('wp-content');
	if (wpContent >= 0 && parts[wpContent + 1] && parts[wpContent + 2]) {
		return `/wp-content/${parts[wpContent + 1]}/${parts[wpContent + 2]}`;
	}
	const wpAdmin = parts.indexOf('wp-admin');
	if (wpAdmin >= 0 && parts[wpAdmin + 1]) {
		return `/wp-admin/${parts[wpAdmin + 1]}`;
	}
	return normalized || 'unknown';
}

function browserMetricName(name) {
	return String(name).trim().toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '') || 'mark';
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
		responseEndMs: round(entry.responseEnd),
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

function addFinding(findings, severity, code, message, data = {}) {
	findings.push({ severity, code, message, ...data });
}

function aggregateResourcesByKind(resources) {
	const groups = {};
	for (const resource of resources) {
		const kind = resource.kind || 'other';
		groups[kind] ||= { kind, count: 0, transferSize: 0, encodedBodySize: 0, decodedBodySize: 0 };
		groups[kind].count += 1;
		groups[kind].transferSize += resource.transferSize || 0;
		groups[kind].encodedBodySize += resource.encodedBodySize || 0;
		groups[kind].decodedBodySize += resource.decodedBodySize || 0;
	}
	return Object.values(groups).sort((a, b) => b.transferSize - a.transferSize);
}

function aggregateResourcesByFamily(resources) {
	const groups = new Map();
	for (const resource of resources) {
		const family = resourceFamily(resource.url);
		if (!groups.has(family)) {
			groups.set(family, { family, kind: resource.kind || 'other', count: 0, transferSize: 0, encodedBodySize: 0, decodedBodySize: 0 });
		}
		const group = groups.get(family);
		group.count += 1;
		group.transferSize += resource.transferSize || 0;
		group.encodedBodySize += resource.encodedBodySize || 0;
		group.decodedBodySize += resource.decodedBodySize || 0;
	}
	return [...groups.values()].sort((a, b) => b.transferSize - a.transferSize);
}

function normalizeNetworkRequest(request) {
	return {
		url: normalizeUrl(request?.url || ''),
		method: request?.method,
		resourceType: request?.resource_type || request?.resourceType,
		status: request?.status || 0,
		failed: Boolean(request?.failed),
		failure: request?.failure,
		durationMs: round(request?.duration_ms ?? request?.durationMs),
	};
}

function diagnoseWordPressPageProfile(profile, options = {}) {
	if (!profile || typeof profile !== 'object') {
		throw new TypeError('diagnoseWordPressPageProfile requires a page profile object');
	}
	const thresholds = { ...DEFAULT_DIAGNOSIS_THRESHOLDS, ...(options.thresholds || {}) };
	const resources = Array.isArray(profile.resources?.resources) ? profile.resources.resources : [];
	const browserMetrics = options.browserMetrics || {};
	const networkRequests = Array.isArray(options.networkRequests) ? options.networkRequests.map(normalizeNetworkRequest) : [];
	const readyMetric = browserMetrics[`${browserMetricName(profile.id)}_ready_ms`];
	const readyAbsoluteMs = typeof readyMetric === 'number' ? readyMetric : undefined;
	const networkIdleMs = typeof browserMetrics.browser_network_idle_ms === 'number' ? browserMetrics.browser_network_idle_ms : undefined;
	const networkIdleAfterReadyMs = readyAbsoluteMs !== undefined && networkIdleMs !== undefined
		? Math.max(0, round(networkIdleMs - readyAbsoluteMs))
		: undefined;
	const findings = [];
	const lateResources = resources
		.filter((resource) => typeof resource.startMs === 'number' && resource.startMs > profile.readyMs)
		.sort((a, b) => b.startMs - a.startMs);
	const restAfterReady = lateResources.filter((resource) => resource.kind === 'rest');
	const failedRequests = networkRequests.filter((request) => request.failed || request.status >= 400);
	const failedRequestCount = networkRequests.length > 0
		? failedRequests.length
		: round(browserMetrics.browser_failed_request_count);
	const totalRequestCount = networkRequests.length || round(browserMetrics.browser_request_count || resources.length);
	const byKind = aggregateResourcesByKind(resources);
	const byFamily = aggregateResourcesByFamily(resources);
	const heavyFamilies = byFamily.filter((group) => group.transferSize >= thresholds.assetBytes).slice(0, 10);

	if (profile.readyMs >= thresholds.readyMs) {
		addFinding(findings, 'info', 'slow-ready', `Page reached readiness in ${round(profile.readyMs)}ms`, {
			readyMs: round(profile.readyMs),
		});
	}
	if (networkIdleAfterReadyMs !== undefined && networkIdleAfterReadyMs >= thresholds.networkIdleGapMs) {
		addFinding(findings, 'warn', 'network-active-after-ready', `Network stayed active ${networkIdleAfterReadyMs}ms after page readiness`, {
			networkIdleAfterReadyMs,
			readyMetricMs: round(readyAbsoluteMs),
			networkIdleMs: round(networkIdleMs),
		});
	}
	if (totalRequestCount >= thresholds.requestCount) {
		addFinding(findings, 'info', 'high-request-count', `Page loaded ${totalRequestCount} tracked browser requests/resources`, {
			requestCount: totalRequestCount,
		});
	}
	if (failedRequestCount >= thresholds.failedRequestCount) {
		addFinding(findings, 'warn', 'failed-requests', `Page had ${failedRequestCount} failed/error browser requests`, {
			failedRequestCount,
			failedRequests: failedRequests.slice(0, 10),
		});
	}
	if (lateResources.length >= thresholds.lateRequestCount) {
		addFinding(findings, 'info', 'late-resources', `${lateResources.length} resources started after page readiness`, {
			lateRequestCount: lateResources.length,
			latestResources: lateResources.slice(0, 10),
		});
	}
	if (restAfterReady.length >= thresholds.restAfterReadyCount) {
		addFinding(findings, 'warn', 'rest-after-ready', `${restAfterReady.length} REST requests started after page readiness`, {
			restAfterReadyCount: restAfterReady.length,
			restAfterReady: restAfterReady.slice(0, 10),
		});
	}
	if (heavyFamilies.length > 0) {
		addFinding(findings, 'info', 'heavy-asset-families', `${heavyFamilies.length} asset families exceeded ${thresholds.assetBytes} transfer bytes`, {
			heavyFamilies,
		});
	}

	return {
		thresholds,
		summary: {
			readyMs: round(profile.readyMs),
			networkIdleAfterReadyMs,
			requestCount: totalRequestCount,
			failedRequestCount,
			lateRequestCount: lateResources.length,
			restAfterReadyCount: restAfterReady.length,
			resourceCount: resources.length,
			restCount: profile.resources?.restCount || 0,
		},
		assets: {
			byKind,
			byFamily: byFamily.slice(0, 20),
			heavyFamilies,
		},
		lateResources: lateResources.slice(0, 20),
		restAfterReady: restAfterReady.slice(0, 20),
		failedRequests: failedRequests.slice(0, 20),
		findings,
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
	const profile = {
		id: spec.id,
		label: spec.label,
		url,
		path: new URL(url).pathname + new URL(url).search,
		status: response && typeof response.status === 'function' ? response.status() : 0,
		readyMs,
		resources: resourceSummary,
		correlation,
	};

	return { ...profile, diagnosis: diagnoseWordPressPageProfile(profile) };
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
	diagnoseWordPressPageProfile,
	normalizePageManifest,
	normalizePageSpec,
	resourceFamily,
	profileWordPressPage,
	profileWordPressPages,
	resolveWordPressUrl,
	summarizeResourceTimings,
	waitForPageReady,
};
