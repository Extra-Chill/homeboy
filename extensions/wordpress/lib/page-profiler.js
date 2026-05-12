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

const DEFAULT_REST_OBSERVATION_MS = 1000;
const DEFAULT_GATE_THRESHOLDS = {
	readyMsRegression: 250,
	networkIdleMsRegression: 500,
	unusedPreloadCount: 0,
	preloadPayloadBytes: 250000,
	serverRequestDurationMsRegression: 100,
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

function sleep(ms) {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

function normalizeRestUrl(url) {
	const normalized = normalizeUrl(url, { lowercasePath: false });
	if (!normalized) {
		return '';
	}
	const restIndex = normalized.indexOf('/wp-json/');
	if (restIndex >= 0) {
		return normalized.slice(restIndex + '/wp-json'.length);
	}
	const restRouteIndex = normalized.indexOf('rest_route=');
	if (restRouteIndex >= 0) {
		return normalized;
	}
	return normalized;
}

function normalizeRestMethod(method) {
	return String(method || 'GET').trim().toUpperCase() || 'GET';
}

function restKey(row) {
	return `${normalizeRestMethod(row?.method)} ${normalizeRestUrl(row?.url || row?.path || row?.normalizedUrl)}`;
}

function normalizeRestWaterfallUrl(url) {
	const normalized = normalizeUrl(url, {
		lowercasePath: false,
		dropQueryParams: ['_', 'v', '_wpnonce', 'nocache', 'cb', 't', '_locale'],
	});
	if (!normalized) {
		return '';
	}
	const restIndex = normalized.indexOf('/wp-json/');
	if (restIndex >= 0) {
		return normalized.slice(restIndex + '/wp-json'.length);
	}
	return normalized;
}

function restWaterfallKey(row) {
	return `${normalizeRestMethod(row?.method)} ${normalizeRestWaterfallUrl(row?.url || row?.path || row?.normalizedUrl)}`;
}

function restRoutePath(url) {
	return normalizeRestWaterfallUrl(url).split('?', 1)[0];
}

function restRouteQuery(url) {
	const normalized = normalizeRestWaterfallUrl(url);
	const queryIndex = normalized.indexOf('?');
	return queryIndex >= 0 ? normalized.slice(queryIndex + 1) : '';
}

function joinRestPathAndParams(path, params) {
	const normalizedPath = normalizeRestUrl(path || '');
	if (!isPlainObject(params) || Object.keys(params).length === 0) {
		return normalizedPath;
	}
	const searchParams = new URLSearchParams();
	for (const [key, value] of Object.entries(params)) {
		if (value === undefined || value === null) {
			continue;
		}
		if (Array.isArray(value)) {
			for (const item of value) {
				searchParams.append(key, String(item));
			}
			continue;
		}
		searchParams.set(key, String(value));
	}
	const query = searchParams.toString();
	return query ? `${normalizedPath}${normalizedPath.includes('?') ? '&' : '?'}${query}` : normalizedPath;
}

function urlMatchesPattern(url, pattern) {
	if (typeof pattern === 'function') {
		return Boolean(pattern(url));
	}
	if (pattern instanceof RegExp) {
		return pattern.test(url);
	}
	if (typeof pattern !== 'string' || pattern.trim() === '') {
		return false;
	}
	const normalized = normalizeRestWaterfallUrl(url);
	const value = pattern.trim();
	if (value.includes('*')) {
		const escaped = value.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*');
		return new RegExp(`^${escaped}$`).test(normalized) || new RegExp(`^${escaped}$`).test(restRoutePath(normalized));
	}
	return normalized === value || restRoutePath(normalized) === value || normalized.startsWith(`${value}?`);
}

function estimatePayloadBytes(value) {
	if (typeof value === 'number' && Number.isFinite(value)) {
		return Math.max(0, Math.round(value));
	}
	if (typeof value === 'string') {
		return typeof Buffer !== 'undefined' ? Buffer.byteLength(value) : value.length;
	}
	if (value !== undefined && value !== null) {
		try {
			const serialized = JSON.stringify(value);
			return typeof Buffer !== 'undefined' ? Buffer.byteLength(serialized) : serialized.length;
		} catch {
			return 0;
		}
	}
	return 0;
}

function normalizeRestPreload(row) {
	const value = typeof row === 'string' ? { path: row } : row || {};
	const url = normalizeRestUrl(value.url || value.path || value.route || value.normalizedUrl || '');
	const payload = value.body ?? value.data ?? value.response ?? value.result;
	const payloadBytes = estimatePayloadBytes(
		value.payloadBytes ?? value.bytes ?? value.size ?? value.contentLength ?? value.encodedBodySize ?? value.decodedBodySize ?? payload
	);

	return {
		url,
		method: normalizeRestMethod(value.method),
		status: value.status || 0,
		payloadBytes,
		payloadAvailable: payloadBytes > 0,
	};
}

function normalizeRestPreloadList(value) {
	if (Array.isArray(value)) {
		return value.map(normalizeRestPreload).filter((preload) => preload.url);
	}
	if (!isPlainObject(value)) {
		return [];
	}

	return Object.entries(value)
		.map(([path, metadata]) => normalizeRestPreload({ path, ...(isPlainObject(metadata) ? metadata : { body: metadata }) }))
		.filter((preload) => preload.url);
}

function normalizeApiFetchAttempt(row) {
	const url = normalizeRestUrl(row?.url || row?.path || row?.route || '');
	return {
		source: row?.source || 'apiFetch',
		url,
		method: normalizeRestMethod(row?.method),
		startedAtMs: round(row?.startedAtMs ?? row?.startMs),
		resolvedAtMs: round(row?.resolvedAtMs ?? row?.endMs),
		durationMs: round(row?.durationMs),
		status: row?.status || 0,
		failed: Boolean(row?.failed),
		error: row?.error,
		errorCode: row?.errorCode,
		responseContentType: row?.responseContentType,
		responseBodyBytes: row?.responseBodyBytes ?? row?.decodedBodySize ?? row?.encodedBodySize ?? row?.transferSize,
		transferSize: row?.transferSize,
		encodedBodySize: row?.encodedBodySize,
		decodedBodySize: row?.decodedBodySize,
		responseBodySample: row?.responseBodySample,
		responseBodySampleTruncated: Boolean(row?.responseBodySampleTruncated),
		responseBodySampleError: row?.responseBodySampleError,
	};
}

function normalizeRestNetworkRequest(row) {
	const url = normalizeRestUrl(row?.url || row?.name || row?.normalizedUrl || '');
	return {
		source: 'network',
		url,
		method: normalizeRestMethod(row?.method),
		status: row?.status || 0,
		failed: Boolean(row?.failed) || (typeof row?.status === 'number' && row.status >= 400),
		startMs: round(row?.start_ms ?? row?.startMs ?? row?.startTime),
		responseEndMs: round(row?.end_ms ?? row?.endMs ?? row?.responseEnd),
		durationMs: round(row?.duration_ms ?? row?.durationMs ?? row?.duration),
		ttfbMs: round(row?.ttfb_ms ?? row?.ttfbMs),
		resourceType: row?.resource_type || row?.resourceType || row?.initiatorType,
		responseContentType: row?.responseContentType,
		responseBodyBytes: row?.responseBodyBytes ?? row?.decodedBodySize ?? row?.encodedBodySize ?? row?.transferSize,
		transferSize: row?.transferSize,
		encodedBodySize: row?.encodedBodySize,
		decodedBodySize: row?.decodedBodySize,
		responseBodySample: row?.responseBodySample,
		responseBodySampleTruncated: Boolean(row?.responseBodySampleTruncated),
		responseBodySampleError: row?.responseBodySampleError,
	};
}

function copyResponseSampleFields(target, source) {
	if (!target || !source) {
		return target;
	}
	for (const key of ['responseContentType', 'responseBodyBytes', 'transferSize', 'encodedBodySize', 'decodedBodySize', 'responseBodySample', 'responseBodySampleTruncated', 'responseBodySampleError']) {
		if (target[key] === undefined && source[key] !== undefined) {
			target[key] = source[key];
		}
	}
	return target;
}

function isRestUrl(url) {
	const value = String(url || '');
	return value.includes('/wp-json/') || value.includes('rest_route=');
}

async function installWordPressRestInstrumentation(page) {
	if (!page || typeof page.addInitScript !== 'function') {
		throw new TypeError('page must provide addInitScript()');
	}

	await page.addInitScript(() => {
		if (window.__homeboyWordPressRestProbe?.installed) {
			return;
		}

		const probe = window.__homeboyWordPressRestProbe = {
			installed: true,
			attempts: [],
			samplePromises: [],
			startedAt: performance.now(),
		};

		const normalizePath = (input) => {
			if (!input) {
				return '';
			}
			if (typeof input === 'string') {
				return input;
			}
			if (input instanceof Request) {
				return input.url;
			}
			if (typeof input === 'object') {
				if (typeof input.url === 'string') {
					return input.url;
				}
				if (typeof input.path === 'string') {
					return input.path;
				}
				if (typeof input.endpoint === 'string') {
					return input.namespace ? `/${input.namespace.replace(/^\/+|\/+$/g, '')}/${input.endpoint.replace(/^\/+/, '')}` : input.endpoint;
				}
			}
			return '';
		};

		const isRest = (url) => {
			const value = String(url || '');
			return value.includes('/wp-json/') || /^\/[A-Za-z0-9_-]+\/v\d+(?:\/|$)/.test(value) || value.includes('rest_route=');
		};
		const responseSampleBytes = 4096;
		const attachResponseSample = async (entry, response) => {
			if (!entry || !response || typeof response.clone !== 'function') {
				return;
			}

			const contentType = response.headers?.get?.('content-type') || '';
			entry.responseContentType = contentType;
			if (!/json|text|html|xml|javascript/i.test(contentType)) {
				return;
			}

			const maxBytes = responseSampleBytes;
			try {
				const clone = response.clone();
				if (clone.body?.getReader && typeof TextDecoder !== 'undefined') {
					const reader = clone.body.getReader();
					const decoder = new TextDecoder();
					let sample = '';
					let bytes = 0;
					let truncated = false;

					while (bytes <= maxBytes) {
						const { done, value } = await reader.read();
						if (done) {
							break;
						}
						bytes += value.byteLength || value.length || 0;
						sample += decoder.decode(value, { stream: true });
						if (bytes > maxBytes) {
							truncated = true;
							await reader.cancel().catch(() => {});
							break;
						}
					}
					sample += decoder.decode();
					entry.responseBodyBytes = bytes;
					entry.responseBodySample = sample.slice(0, maxBytes);
					entry.responseBodySampleTruncated = truncated;
					return;
				}

				const text = await clone.text();
				entry.responseBodyBytes = text.length;
				entry.responseBodySample = text.slice(0, maxBytes);
				entry.responseBodySampleTruncated = text.length > maxBytes;
			} catch (error) {
				entry.responseBodySampleError = error?.message || String(error);
			}
		};
		const record = (source, input, init) => {
			const url = normalizePath(input);
			if (!isRest(url)) {
				return null;
			}
			const entry = {
				source,
				url,
				method: (input?.method || init?.method || 'GET').toUpperCase(),
				startedAtMs: performance.now() - probe.startedAt,
			};
			probe.attempts.push(entry);
			return entry;
		};

		if (typeof window.fetch === 'function' && !window.fetch.__homeboyRestProbeWrapped) {
			const originalFetch = window.fetch.bind(window);
			const wrappedFetch = async (...args) => {
				const entry = record('fetch', args[0], args[1]);
				try {
					const response = await originalFetch(...args);
					if (entry) {
						entry.status = response.status;
						entry.resolvedAtMs = performance.now() - probe.startedAt;
						entry.durationMs = entry.resolvedAtMs - entry.startedAtMs;
						probe.samplePromises.push(attachResponseSample(entry, response));
					}
					return response;
				} catch (error) {
					if (entry) {
						entry.failed = true;
						entry.error = error?.message || String(error);
						entry.resolvedAtMs = performance.now() - probe.startedAt;
						entry.durationMs = entry.resolvedAtMs - entry.startedAtMs;
					}
					throw error;
				}
			};
			wrappedFetch.__homeboyRestProbeWrapped = true;
			window.fetch = wrappedFetch;
		}

		const patchApiFetch = () => {
			const apiFetch = window.wp?.apiFetch;
			if (typeof apiFetch !== 'function' || apiFetch.__homeboyRestProbeWrapped) {
				return Boolean(apiFetch?.__homeboyRestProbeWrapped);
			}
			const wrappedApiFetch = async (options = {}) => {
				const entry = record('apiFetch', options, options);
				try {
					const result = await apiFetch(options);
					if (entry) {
						entry.resolvedAtMs = performance.now() - probe.startedAt;
						entry.durationMs = entry.resolvedAtMs - entry.startedAtMs;
					}
					return result;
				} catch (error) {
					if (entry) {
						entry.failed = true;
						entry.error = error?.message || String(error);
						entry.errorCode = error?.code;
						entry.status = error?.data?.status || error?.status || 0;
						entry.resolvedAtMs = performance.now() - probe.startedAt;
						entry.durationMs = entry.resolvedAtMs - entry.startedAtMs;
					}
					throw error;
				}
			};
			for (const key of Object.keys(apiFetch)) {
				wrappedApiFetch[key] = apiFetch[key];
			}
			wrappedApiFetch.__homeboyRestProbeWrapped = true;
			window.wp.apiFetch = wrappedApiFetch;
			return true;
		};

		if (!patchApiFetch()) {
			const interval = setInterval(() => {
				if (patchApiFetch()) {
					clearInterval(interval);
				}
			}, 20);
			setTimeout(() => clearInterval(interval), 30000);
		}
	});
}

async function collectWordPressRestAttempts(page) {
	if (!page || typeof page.evaluate !== 'function') {
		throw new TypeError('page must provide evaluate()');
	}
	const attempts = await page.evaluate(async () => {
		const probe = window.__homeboyWordPressRestProbe;
		if (!probe) {
			return [];
		}
		await Promise.allSettled(probe.samplePromises || []);
		return probe.attempts || [];
	});
	return attempts.map(normalizeApiFetchAttempt).filter((attempt) => attempt.url);
}

async function collectWordPressRestPreloads(page) {
	if (!page || typeof page.evaluate !== 'function') {
		throw new TypeError('page must provide evaluate()');
	}
	const preloads = await page.evaluate(() => {
		const candidates = [
			window.__homeboyWordPressRestProbe?.preloads,
			window.__homeboyWordPressRestPreloads,
			window.__wpRestPreloads,
			window.wp?.apiFetch?.preloadData,
		];
		return candidates.find((candidate) => Array.isArray(candidate) || (candidate && typeof candidate === 'object')) || [];
	});
	return normalizeRestPreloadList(preloads);
}

function summarizeWordPressRestWaterfall(input = {}) {
	const readyMs = round(input.readyMs);
	const rawApiFetchAttempts = Array.isArray(input.apiFetchAttempts) ? input.apiFetchAttempts.map(normalizeApiFetchAttempt).filter((row) => row.url) : [];
	const fetchSamplesByKey = new Map();
	for (const attempt of rawApiFetchAttempts) {
		if (attempt.source === 'fetch' && attempt.responseBodySample !== undefined) {
			fetchSamplesByKey.set(restKey(attempt), attempt);
		}
	}
	for (const attempt of rawApiFetchAttempts) {
		if (attempt.source === 'apiFetch') {
			copyResponseSampleFields(attempt, fetchSamplesByKey.get(restKey(attempt)));
		}
	}
	const apiFetchKeys = new Set(rawApiFetchAttempts.filter((row) => row.source === 'apiFetch').map(restKey));
	const apiFetchAttempts = rawApiFetchAttempts.filter((row) => row.source !== 'fetch' || !apiFetchKeys.has(restKey(row)));
	// Callers may pass an array of paths/metadata rows or a WordPress-style object keyed by REST path.
	const preloads = [
		...normalizeRestPreloadList(input.preloadedRestPaths),
		...normalizeRestPreloadList(input.restPreloads),
		...normalizeRestPreloadList(input.preloadMetadata),
	];
	const resourceTimings = Array.isArray(input.resourceTimings) ? input.resourceTimings.filter((row) => isRestUrl(row?.url || row?.name || row?.normalizedUrl)).map(normalizeRestNetworkRequest).filter((row) => row.url) : [];
	const networkRequests = Array.isArray(input.networkRequests) ? input.networkRequests.filter((row) => isRestUrl(row?.url || row?.name || row?.normalizedUrl)).map(normalizeRestNetworkRequest).filter((row) => row.url) : [];
	const networkByKey = new Map();
	const clientKeys = new Set(apiFetchAttempts.map(restKey));
	const preloadByKey = new Map();

	for (const row of [...resourceTimings, ...networkRequests]) {
		const key = restKey(row);
		if (!networkByKey.has(key)) {
			networkByKey.set(key, []);
		}
		networkByKey.get(key).push(row);
	}
	for (const preload of preloads) {
		preloadByKey.set(restKey(preload), preload);
	}

	const rows = [];
	const seen = new Set();
	for (const attempt of apiFetchAttempts) {
		const key = restKey(attempt);
		seen.add(key);
		const matches = networkByKey.get(key) || [];
		const network = matches.shift();
		const preload = preloadByKey.get(key);
		rows.push({
			url: attempt.url,
			method: attempt.method,
			source: network ? 'network' : 'preloaded-or-cache',
			clientSource: attempt.source,
			preloadMatched: Boolean(preload),
			preloadPayloadBytes: preload?.payloadBytes || 0,
			status: network?.status || attempt.status || 0,
			failed: Boolean(network?.failed || attempt.failed),
			startMs: network?.startMs || attempt.startedAtMs,
			responseEndMs: network?.responseEndMs || attempt.resolvedAtMs,
			durationMs: network?.durationMs || attempt.durationMs,
			ttfbMs: network?.ttfbMs,
			afterReady: readyMs > 0 && (network?.startMs || attempt.startedAtMs) > readyMs,
			networkMatched: Boolean(network),
			error: attempt.error,
			errorCode: attempt.errorCode,
			responseContentType: attempt.responseContentType || network?.responseContentType,
			responseBodyBytes: attempt.responseBodyBytes || network?.responseBodyBytes,
			transferSize: network?.transferSize || attempt.transferSize,
			encodedBodySize: network?.encodedBodySize || attempt.encodedBodySize,
			decodedBodySize: network?.decodedBodySize || attempt.decodedBodySize,
			responseBodySample: attempt.responseBodySample || network?.responseBodySample,
			responseBodySampleTruncated: Boolean(attempt.responseBodySampleTruncated || network?.responseBodySampleTruncated),
			responseBodySampleError: attempt.responseBodySampleError || network?.responseBodySampleError,
		});
	}

	for (const [key, matches] of networkByKey.entries()) {
		for (const network of matches) {
			rows.push({
				url: network.url,
				method: network.method,
				source: seen.has(key) ? 'network-duplicate' : 'raw-network',
				clientSource: undefined,
				status: network.status,
				failed: network.failed,
				startMs: network.startMs,
				responseEndMs: network.responseEndMs,
				durationMs: network.durationMs,
				ttfbMs: network.ttfbMs,
				afterReady: readyMs > 0 && network.startMs > readyMs,
				networkMatched: true,
				responseContentType: network.responseContentType,
				responseBodyBytes: network.responseBodyBytes,
				transferSize: network.transferSize,
				encodedBodySize: network.encodedBodySize,
				decodedBodySize: network.decodedBodySize,
				responseBodySample: network.responseBodySample,
				responseBodySampleTruncated: Boolean(network.responseBodySampleTruncated),
				responseBodySampleError: network.responseBodySampleError,
			});
		}
	}

	rows.sort((a, b) => (a.startMs || 0) - (b.startMs || 0));
	const usedPreloadRows = preloads.filter((preload) => clientKeys.has(restKey(preload)));
	const unusedPreloadRows = preloads.filter((preload) => !clientKeys.has(restKey(preload)));
	const networkRows = rows.filter((row) => row.networkMatched);
	const preloadPayloadBytes = preloads.reduce((total, preload) => total + (preload.payloadBytes || 0), 0);
	const counts = {
		total: rows.length,
		network: networkRows.length,
		preloadedOrCache: rows.filter((row) => row.source === 'preloaded-or-cache').length,
		afterReady: rows.filter((row) => row.afterReady).length,
		failed: rows.filter((row) => row.failed || row.status >= 400).length,
		preload: preloads.length,
		usedPreload: usedPreloadRows.length,
		unusedPreload: unusedPreloadRows.length,
		unusedPreloadCount: unusedPreloadRows.length,
		preloadPayloadBytes,
		remainingRestNetworkCount: networkRows.length,
	};
	const metrics = {
		unused_preload_count: unusedPreloadRows.length,
		preload_payload_bytes: preloadPayloadBytes,
		remaining_rest_network_count: networkRows.length,
	};

	return {
		readyMs,
		counts,
		metrics,
		apiFetchAttempts,
		preloads,
		resourceTimings,
		networkRequests,
		rows,
		networkRows,
		usedPreloadRows,
		unusedPreloadRows,
		remainingRestNetworkRows: networkRows,
		preloadedOrCacheRows: rows.filter((row) => row.source === 'preloaded-or-cache'),
		afterReadyRows: rows.filter((row) => row.afterReady),
	};
}

function restResponseBytes(row) {
	return pickNumber(row, ['responseBodyBytes', 'decodedBodySize', 'encodedBodySize', 'transferSize', 'preloadPayloadBytes']) || 0;
}

function normalizeRestRouteBudgets(restBudget = {}) {
	const routes = [];
	const addRouteBudget = (pattern, budget) => {
		if (!pattern || !isPlainObject(budget)) {
			return;
		}
		routes.push({ pattern, ...budget });
	};

	if (Array.isArray(restBudget.routes)) {
		for (const route of restBudget.routes) {
			if (isPlainObject(route)) {
				routes.push(route);
			}
		}
	}
	if (isPlainObject(restBudget.routes)) {
		for (const [pattern, budget] of Object.entries(restBudget.routes)) {
			addRouteBudget(pattern, budget);
		}
	}
	if (isPlainObject(restBudget.perRoute)) {
		for (const [pattern, budget] of Object.entries(restBudget.perRoute)) {
			addRouteBudget(pattern, budget);
		}
	}
	return routes;
}

function routeBudgetForRow(row, restBudget = {}) {
	const routeBudgets = normalizeRestRouteBudgets(restBudget);
	return routeBudgets.find((budget) => urlMatchesPattern(row.url, budget.pattern || budget.path || budget.route));
}

function createWordPressBudgetFinding({ severity = 'error', code, message, actual, expected, unit, subject, data = {} }) {
	return {
		category: 'budget',
		severity,
		code,
		message,
		actual,
		expected,
		unit,
		subject,
		...data,
	};
}

function restBudgetRowData(row, budgetThreshold, phase = 'page-profile') {
	return {
		method: normalizeRestMethod(row.method),
		normalizedPath: restRoutePath(row.url),
		queryString: restRouteQuery(row.url),
		status: row.status || 0,
		durationMs: round(row.durationMs),
		phase,
		threshold: budgetThreshold,
		responseBodyBytes: restResponseBytes(row),
		decodedBodySize: row.decodedBodySize,
		encodedBodySize: row.encodedBodySize,
		transferSize: row.transferSize,
	};
}

function evaluateWordPressRestPayloadBudgets(waterfall, budgets = {}, options = {}) {
	const restBudget = budgets.rest || budgets;
	if (!isPlainObject(restBudget)) {
		return { findings: [], topPayloadRows: [], totals: { responseBytes: 0, restRequests: 0 } };
	}
	const rows = Array.isArray(waterfall?.rows) ? waterfall.rows : [];
	const allow = Array.isArray(restBudget.allow) ? restBudget.allow : [];
	const evaluatedRows = rows.filter((row) => !allow.some((pattern) => urlMatchesPattern(row.url, pattern)));
	const topPayloadRows = [...evaluatedRows]
		.map((row) => ({ ...row, responseBytes: restResponseBytes(row) }))
		.sort((a, b) => b.responseBytes - a.responseBytes)
		.slice(0, options.limit || 10);
	const totalResponseBytes = evaluatedRows.reduce((total, row) => total + restResponseBytes(row), 0);
	const findings = [];

	for (const row of evaluatedRows) {
		const routeBudget = routeBudgetForRow(row, restBudget);
		const maxResponseBytes = routeBudget?.maxResponseBytes ?? routeBudget?.maxBytes ?? restBudget.maxResponseBytes ?? restBudget.maxBytes;
		const bytes = restResponseBytes(row);
		if (typeof maxResponseBytes === 'number' && bytes > maxResponseBytes) {
			const subject = `${normalizeRestMethod(row.method)} ${normalizeRestWaterfallUrl(row.url)}`;
			findings.push(createWordPressBudgetFinding({
				code: 'wordpress.rest.max_response_bytes',
				message: `REST response exceeded ${maxResponseBytes} byte budget`,
				actual: bytes,
				expected: maxResponseBytes,
				unit: 'bytes',
				subject,
				data: restBudgetRowData(row, maxResponseBytes, options.phase),
			}));
		}
	}

	if (typeof restBudget.maxTotalResponseBytes === 'number' && totalResponseBytes > restBudget.maxTotalResponseBytes) {
		findings.push(createWordPressBudgetFinding({
			code: 'wordpress.rest.max_total_response_bytes',
			message: `Total REST responses exceeded ${restBudget.maxTotalResponseBytes} byte budget`,
			actual: totalResponseBytes,
			expected: restBudget.maxTotalResponseBytes,
			unit: 'bytes',
			subject: options.subject || 'WordPress REST page profile',
			data: {
				phase: options.phase || 'page-profile',
				threshold: restBudget.maxTotalResponseBytes,
				topPayloadRows,
			},
		}));
	}
	if (typeof restBudget.maxRestRequests === 'number' && evaluatedRows.length > restBudget.maxRestRequests) {
		findings.push(createWordPressBudgetFinding({
			code: 'wordpress.rest.max_requests',
			message: `REST request count exceeded ${restBudget.maxRestRequests} request budget`,
			actual: evaluatedRows.length,
			expected: restBudget.maxRestRequests,
			unit: 'requests',
			subject: options.subject || 'WordPress REST page profile',
			data: {
				phase: options.phase || 'page-profile',
				threshold: restBudget.maxRestRequests,
			},
		}));
	}

	return {
		findings,
		topPayloadRows,
		totals: {
			responseBytes: totalResponseBytes,
			restRequests: evaluatedRows.length,
		},
	};
}

function formatWordPressRestPayloadBudgetMarkdownReport(result, options = {}) {
	const rows = Array.isArray(result?.topPayloadRows) ? result.topPayloadRows : [];
	const findings = Array.isArray(result?.findings) ? result.findings : [];
	const limit = options.limit || 10;
	const lines = [
		'## WordPress REST payload budgets',
		'',
		`- REST response bytes: ${result?.totals?.responseBytes || 0}`,
		`- REST requests evaluated: ${result?.totals?.restRequests || 0}`,
		`- Budget findings: ${findings.length}`,
		'',
		'| Endpoint | Status | Duration | Bytes |',
		'|---|---:|---:|---:|',
	];
	for (const row of rows.slice(0, limit)) {
		lines.push(`| \`${normalizeRestMethod(row.method)} ${normalizeRestWaterfallUrl(row.url)}\` | ${row.status || 0} | ${round(row.durationMs)}ms | ${row.responseBytes || restResponseBytes(row)} |`);
	}
	if (findings.length > 0) {
		lines.push('', '## Budget findings', '', '| Code | Subject | Actual | Expected |', '|---|---|---:|---:|');
		for (const finding of findings.slice(0, limit)) {
			lines.push(`| \`${finding.code}\` | \`${finding.subject}\` | ${finding.actual} ${finding.unit || ''} | ${finding.expected} ${finding.unit || ''} |`);
		}
	}
	return lines.join('\n');
}

function normalizeRestMatrixEndpoint(endpoint, index = 0) {
	if (typeof endpoint === 'string') {
		return { id: `rest-${index + 1}`, method: 'GET', path: endpoint, params: {}, headers: {}, body: undefined, budget: {} };
	}
	if (!isPlainObject(endpoint)) {
		throw new TypeError('REST matrix endpoint must be a string or object');
	}
	const path = endpoint.path || endpoint.url || endpoint.route;
	if (typeof path !== 'string' || path.trim() === '') {
		throw new TypeError('REST matrix endpoint requires path, url, or route');
	}
	return {
		id: endpoint.id || endpoint.label || `${normalizeRestMethod(endpoint.method)} ${joinRestPathAndParams(path, endpoint.params)}`,
		label: endpoint.label,
		method: normalizeRestMethod(endpoint.method),
		path,
		params: isPlainObject(endpoint.params) ? endpoint.params : {},
		headers: isPlainObject(endpoint.headers) ? endpoint.headers : {},
		body: endpoint.body,
		budget: isPlainObject(endpoint.budget) ? endpoint.budget : {},
		user: endpoint.user,
	};
}

function summarizeJsonShape(value) {
	if (Array.isArray(value)) {
		return {
			type: 'array',
			itemCount: value.length,
			itemType: value.length > 0 ? summarizeJsonShape(value[0]).type : undefined,
		};
	}
	if (isPlainObject(value)) {
		const keys = Object.keys(value);
		return {
			type: 'object',
			keys,
			topLevelKeys: keys,
			itemCount: Array.isArray(value.items) ? value.items.length : undefined,
		};
	}
	return { type: value === null ? 'null' : typeof value };
}

function restMatrixResultFindings(result) {
	const budget = result.budget || {};
	const findings = [];
	const subject = `${result.method} ${result.normalizedUrl}`;
	const expectedStatus = budget.expectedStatus ?? budget.status;
	if (expectedStatus !== undefined) {
		const expectedStatuses = Array.isArray(expectedStatus) ? expectedStatus : [expectedStatus];
		if (!expectedStatuses.includes(result.status)) {
			findings.push(createWordPressBudgetFinding({
				code: 'wordpress.rest_matrix.expected_status',
				message: `REST matrix endpoint returned status ${result.status}`,
				actual: result.status,
				expected: expectedStatuses.join(','),
				unit: 'status',
				subject,
				data: { phase: 'rest-matrix', method: result.method, normalizedPath: result.normalizedPath, queryString: result.queryString, threshold: expectedStatuses },
			}));
		}
	}
	if (typeof budget.maxBytes === 'number' && result.responseBytes > budget.maxBytes) {
		findings.push(createWordPressBudgetFinding({
			code: 'wordpress.rest_matrix.max_bytes',
			message: `REST matrix endpoint exceeded ${budget.maxBytes} byte budget`,
			actual: result.responseBytes,
			expected: budget.maxBytes,
			unit: 'bytes',
			subject,
			data: { phase: 'rest-matrix', method: result.method, normalizedPath: result.normalizedPath, queryString: result.queryString, status: result.status, durationMs: result.durationMs, threshold: budget.maxBytes },
		}));
	}
	if (typeof budget.maxMs === 'number' && result.durationMs > budget.maxMs) {
		findings.push(createWordPressBudgetFinding({
			code: 'wordpress.rest_matrix.max_ms',
			message: `REST matrix endpoint exceeded ${budget.maxMs}ms duration budget`,
			actual: result.durationMs,
			expected: budget.maxMs,
			unit: 'ms',
			subject,
			data: { phase: 'rest-matrix', method: result.method, normalizedPath: result.normalizedPath, queryString: result.queryString, status: result.status, threshold: budget.maxMs },
		}));
	}
	if (typeof budget.maxItemCount === 'number' && typeof result.itemCount === 'number' && result.itemCount > budget.maxItemCount) {
		findings.push(createWordPressBudgetFinding({
			code: 'wordpress.rest_matrix.max_item_count',
			message: `REST matrix endpoint exceeded ${budget.maxItemCount} item budget`,
			actual: result.itemCount,
			expected: budget.maxItemCount,
			unit: 'items',
			subject,
			data: { phase: 'rest-matrix', method: result.method, normalizedPath: result.normalizedPath, queryString: result.queryString, status: result.status, threshold: budget.maxItemCount },
		}));
	}
	return findings;
}

async function executeWordPressRestMatrixRequest({ page, baseUrl, endpoint, headers = {} }) {
	const normalizedUrl = joinRestPathAndParams(endpoint.path, endpoint.params);
	const url = resolveWordPressUrl(baseUrl, normalizedUrl.startsWith('/wp-json/') ? normalizedUrl : `/wp-json${normalizedUrl.startsWith('/') ? '' : '/'}${normalizedUrl}`);
	const requestHeaders = { accept: 'application/json', ...headers, ...endpoint.headers };
	const body = endpoint.body === undefined || endpoint.body === null ? undefined : JSON.stringify(endpoint.body);
	if (body !== undefined && !requestHeaders['content-type'] && !requestHeaders['Content-Type']) {
		requestHeaders['content-type'] = 'application/json';
	}

	if (page && typeof page.evaluate === 'function') {
		return page.evaluate(async ({ url: requestUrl, method, requestHeaders: browserHeaders, body: requestBody }) => {
			const started = performance.now();
			const response = await fetch(requestUrl, {
				method,
				headers: browserHeaders,
				body: requestBody,
				credentials: 'same-origin',
			});
			const text = await response.text();
			return {
				status: response.status,
				durationMs: Math.round(performance.now() - started),
				contentType: response.headers.get('content-type') || '',
				bodyText: text,
			};
		}, { url, method: endpoint.method, requestHeaders, body });
	}

	if (typeof fetch !== 'function') {
		throw new TypeError('profileWordPressRestMatrix requires page.evaluate() or global fetch()');
	}
	const started = Date.now();
	const response = await fetch(url, { method: endpoint.method, headers: requestHeaders, body });
	return {
		status: response.status,
		durationMs: Date.now() - started,
		contentType: response.headers.get('content-type') || '',
		bodyText: await response.text(),
	};
}

async function profileWordPressRestMatrix(input = {}) {
	if (!input || typeof input !== 'object') {
		throw new TypeError('profileWordPressRestMatrix requires an input object');
	}
	const endpoints = Array.isArray(input.restMatrix) ? input.restMatrix : input.endpoints;
	if (!Array.isArray(endpoints)) {
		throw new TypeError('profileWordPressRestMatrix requires restMatrix or endpoints array');
	}
	const normalizedEndpoints = endpoints.map(normalizeRestMatrixEndpoint);
	const results = [];
	for (const endpoint of normalizedEndpoints) {
		const user = endpoint.user || input.user || 'default-admin';
		if (typeof input.authenticateAs === 'function') {
			await input.authenticateAs(user, { page: input.page, endpoint });
		}
		const response = await executeWordPressRestMatrixRequest({
			page: input.page,
			baseUrl: input.baseUrl,
			endpoint,
			headers: input.headers || {},
		});
		let parsed;
		try {
			parsed = response.bodyText ? JSON.parse(response.bodyText) : undefined;
		} catch {
			parsed = undefined;
		}
		const shape = summarizeJsonShape(parsed);
		const responseBytes = estimatePayloadBytes(response.bodyText || '');
		const normalizedUrl = joinRestPathAndParams(endpoint.path, endpoint.params);
		const result = {
			id: endpoint.id,
			label: endpoint.label,
			user,
			method: endpoint.method,
			path: endpoint.path,
			normalizedUrl,
			normalizedPath: restRoutePath(normalizedUrl),
			queryString: restRouteQuery(normalizedUrl),
			status: response.status,
			durationMs: round(response.durationMs),
			responseBytes,
			contentType: response.contentType,
			jsonShape: shape,
			topLevelKeys: shape.topLevelKeys || [],
			itemCount: shape.itemCount,
			budget: endpoint.budget,
		};
		result.findings = restMatrixResultFindings(result);
		results.push(result);
	}
	const findings = results.flatMap((result) => result.findings);
	return {
		user: input.user || 'default-admin',
		count: results.length,
		findings,
		results,
	};
}

function formatWordPressRestMatrixMarkdownReport(matrix, options = {}) {
	const results = Array.isArray(matrix?.results) ? matrix.results : [];
	const limit = options.limit || 50;
	const lines = [
		'## WordPress REST endpoint matrix',
		'',
		`- Endpoints checked: ${results.length}`,
		`- Budget findings: ${(matrix?.findings || []).length}`,
		'',
		'| Endpoint | Status | Duration | Bytes | Shape | Findings |',
		'|---|---:|---:|---:|---|---:|',
	];
	for (const result of results.slice(0, limit)) {
		const shape = result.jsonShape?.type === 'object'
			? `object(${(result.topLevelKeys || []).slice(0, 6).join(', ')})`
			: result.jsonShape?.type === 'array'
				? `array(${result.itemCount || 0})`
				: result.jsonShape?.type || 'unknown';
		lines.push(`| \`${result.method} ${result.normalizedUrl}\` | ${result.status} | ${result.durationMs}ms | ${result.responseBytes} | ${shape} | ${(result.findings || []).length} |`);
	}
	return lines.join('\n');
}

function classifyWordPressRestPreloadOpportunity(row) {
	const url = normalizeRestUrl(row?.url || '');
	const pathOnly = url.split('?', 1)[0];
	const reason = [];
	let classification = 'investigate';

	if (/\/wp\/v2\/(?:users\/me|autosaves|revisions|preferences)/.test(pathOnly)) {
		classification = 'client-state';
		reason.push('user/session or editor state request');
	} else if (/\/wp\/v2\/template-parts\//.test(pathOnly) || /\/wp\/v2\/templates\/lookup/.test(pathOnly) || /\/wp\/v2\/types\//.test(pathOnly) || /\/wp\/v2\/taxonomies/.test(pathOnly) || /\/wp\/v2\/pages\/\d+/.test(pathOnly)) {
		classification = 'safe-deterministic';
		reason.push('route/template/page data can usually be derived server-side');
	} else if (/\/wp\/v2\/(?:posts|pages)(?:\?|$)/.test(pathOnly) || /\/wp\/v2\/search/.test(pathOnly) || /\/wp\/v2\/navigation/.test(pathOnly)) {
		classification = 'conditional';
		reason.push('depends on resolved template blocks or initial editor route');
	} else if (row?.source === 'preloaded-or-cache') {
		classification = 'already-preloaded-or-cache';
		reason.push('client requested it without an observed network request');
	}

	return {
		...row,
		classification,
		reason: reason.join('; ') || 'no built-in rule matched',
	};
}

function classifyWordPressRestPreloadOpportunities(waterfall, options = {}) {
	const rows = Array.isArray(waterfall?.rows) ? waterfall.rows : [];
	const candidates = rows
		.filter((row) => options.includePreloaded || row.networkMatched)
		.map(classifyWordPressRestPreloadOpportunity);
	const byClassification = {};
	for (const row of candidates) {
		byClassification[row.classification] = (byClassification[row.classification] || 0) + 1;
	}
	return {
		byClassification,
		safeDeterministic: candidates.filter((row) => row.classification === 'safe-deterministic'),
		conditional: candidates.filter((row) => row.classification === 'conditional'),
		clientState: candidates.filter((row) => row.classification === 'client-state'),
		alreadyPreloadedOrCache: candidates.filter((row) => row.classification === 'already-preloaded-or-cache'),
		investigate: candidates.filter((row) => row.classification === 'investigate'),
		rows: candidates,
	};
}

function classifyWordPressRestNetworkRoute(row) {
	const method = normalizeRestMethod(row?.method);
	const url = normalizeRestWaterfallUrl(row?.url || '');
	const pathOnly = restRoutePath(url);

	if (method === 'OPTIONS') {
		return { classification: 'options-schema', reason: 'REST schema/options request' };
	}
	if (/\/wp\/v2\/users\/me(?:$|\?)/.test(pathOnly)) {
		return { classification: 'current-user', reason: 'current user/session state' };
	}
	if (/\/wp\/v2\/template-parts\//.test(pathOnly) || /\/wp\/v2\/templates\//.test(pathOnly)) {
		return { classification: 'template-data', reason: 'template or template-part data can often be derived from the initial route' };
	}
	if (/\/wp\/v2\/posts(?:$|\?)/.test(pathOnly)) {
		return { classification: 'query-loop-posts', reason: 'post collection is preloadable when the resolved template contains a Query Loop' };
	}
	if (/\/wp\/v2\/taxonomies(?:$|\?)/.test(pathOnly)) {
		return { classification: 'taxonomy-data', reason: 'taxonomy data is deterministic for the initial editor boot' };
	}
	if (/\/wp\/v2\/(?:navigation|menus)(?:\/|$|\?)/.test(pathOnly) || /\/wp-block-editor\/v1\/navigation-fallback/.test(pathOnly)) {
		return { classification: 'navigation-state', reason: 'navigation data depends on current site menu/navigation state' };
	}
	if (/\/wp\/v2\/pages(?:\/|$|\?)/.test(pathOnly)) {
		return { classification: 'page-tree', reason: 'page tree or page record data depends on the initial editor route/site content' };
	}
	if (/\/wp\/v2\/(?:block-patterns|wp_pattern_category)(?:\/|$|\?)/.test(pathOnly)) {
		return { classification: 'pattern-data', reason: 'pattern browser data is global editor background data' };
	}
	if (/\/wp\/v2\/types(?:\/|$|\?)/.test(pathOnly)) {
		return { classification: 'type-metadata', reason: 'post type metadata' };
	}
	if (/\/wp\/v2\/settings(?:$|\?)/.test(pathOnly)) {
		return { classification: 'settings', reason: 'site settings data' };
	}

	return { classification: 'investigate', reason: 'no built-in WordPress REST route rule matched' };
}

function summarizeWordPressRestNetworkRows(input = []) {
	const sourceRows = Array.isArray(input)
		? input
		: Array.isArray(input?.networkRequests)
			? input.networkRequests
			: Array.isArray(input?.rows)
				? input.rows
				: [];
	const groups = new Map();

	for (const rawRow of sourceRows) {
		if (!isRestUrl(rawRow?.url || rawRow?.name || rawRow?.normalizedUrl || rawRow?.path)) {
			continue;
		}
		const row = normalizeRestNetworkRequest(rawRow);
		const key = restWaterfallKey(row);
		if (!row.url || !key.trim()) {
			continue;
		}
		const existing = groups.get(key) || {
			key,
			url: normalizeRestWaterfallUrl(row.url),
			method: row.method,
			count: 0,
			totalDurationMs: 0,
			maxDurationMs: 0,
			statuses: [],
			rows: [],
			...classifyWordPressRestNetworkRoute(row),
		};
		existing.count += 1;
		existing.totalDurationMs += row.durationMs || 0;
		existing.maxDurationMs = Math.max(existing.maxDurationMs, row.durationMs || 0);
		if (row.status && !existing.statuses.includes(row.status)) {
			existing.statuses.push(row.status);
		}
		existing.rows.push(row);
		groups.set(key, existing);
	}

	const routes = [...groups.values()]
		.map((group) => ({
			...group,
			avgDurationMs: group.count > 0 ? group.totalDurationMs / group.count : 0,
			maxDurationMs: round(group.maxDurationMs),
			totalDurationMs: round(group.totalDurationMs),
		}))
		.sort((a, b) => (b.count - a.count) || (b.maxDurationMs - a.maxDurationMs) || a.key.localeCompare(b.key));
	const byClassification = {};
	for (const route of routes) {
		byClassification[route.classification] = (byClassification[route.classification] || 0) + route.count;
	}

	return {
		count: routes.reduce((total, route) => total + route.count, 0),
		uniqueCount: routes.length,
		byClassification,
		routes,
	};
}

function compareWordPressRestNetworkWaterfalls({ baseline, candidate }) {
	const baselineSummary = summarizeWordPressRestNetworkRows(baseline);
	const candidateSummary = summarizeWordPressRestNetworkRows(candidate);
	const candidateByKey = new Map(candidateSummary.routes.map((route) => [route.key, route]));
	const baselineByKey = new Map(baselineSummary.routes.map((route) => [route.key, route]));
	const rows = [];

	for (const baselineRoute of baselineSummary.routes) {
		const candidateRoute = candidateByKey.get(baselineRoute.key);
		rows.push({
			key: baselineRoute.key,
			url: baselineRoute.url,
			method: baselineRoute.method,
			classification: baselineRoute.classification,
			reason: baselineRoute.reason,
			before: baselineRoute.count,
			after: candidateRoute?.count || 0,
			beforeMaxDurationMs: baselineRoute.maxDurationMs,
			afterMaxDurationMs: candidateRoute?.maxDurationMs || 0,
			result: candidateRoute ? 'remaining' : 'removed',
		});
	}
	for (const candidateRoute of candidateSummary.routes) {
		if (baselineByKey.has(candidateRoute.key)) {
			continue;
		}
		rows.push({
			key: candidateRoute.key,
			url: candidateRoute.url,
			method: candidateRoute.method,
			classification: candidateRoute.classification,
			reason: candidateRoute.reason,
			before: 0,
			after: candidateRoute.count,
			beforeMaxDurationMs: 0,
			afterMaxDurationMs: candidateRoute.maxDurationMs,
			result: 'added',
		});
	}

	rows.sort((a, b) => {
		const order = { removed: 0, remaining: 1, added: 2 };
		return (order[a.result] - order[b.result]) || (b.before - a.before) || (b.beforeMaxDurationMs - a.beforeMaxDurationMs) || a.key.localeCompare(b.key);
	});

	return {
		counts: {
			baseline: baselineSummary.count,
			candidate: candidateSummary.count,
			baselineUnique: baselineSummary.uniqueCount,
			candidateUnique: candidateSummary.uniqueCount,
			removed: rows.filter((row) => row.result === 'removed').reduce((total, row) => total + row.before, 0),
			removedUnique: rows.filter((row) => row.result === 'removed').length,
			remainingUnique: rows.filter((row) => row.result === 'remaining').length,
			added: rows.filter((row) => row.result === 'added').reduce((total, row) => total + row.after, 0),
			addedUnique: rows.filter((row) => row.result === 'added').length,
		},
		baseline: baselineSummary,
		candidate: candidateSummary,
		rows,
		removedRoutes: rows.filter((row) => row.result === 'removed'),
		remainingRoutes: rows.filter((row) => row.result === 'remaining'),
		addedRoutes: rows.filter((row) => row.result === 'added'),
	};
}

function formatWordPressRestNetworkDiffMarkdownReport(diff, options = {}) {
	const limit = options.limit || 40;
	const lines = [
		'## WordPress REST network diff',
		'',
		`- Baseline REST requests: ${diff?.counts?.baseline || 0} (${diff?.counts?.baselineUnique || 0} unique)`,
		`- Candidate REST requests: ${diff?.counts?.candidate || 0} (${diff?.counts?.candidateUnique || 0} unique)`,
		`- Removed REST requests: ${diff?.counts?.removed || 0} (${diff?.counts?.removedUnique || 0} unique)`,
		'',
		'| Result | Endpoint | Before | After | Max before | Max after | Class |',
		'|---|---|---:|---:|---:|---:|---|',
	];

	for (const row of (diff?.rows || []).slice(0, limit)) {
		lines.push(`| ${row.result} | \`${row.method} ${row.url}\` | ${row.before} | ${row.after} | ${round(row.beforeMaxDurationMs)}ms | ${round(row.afterMaxDurationMs)}ms | ${row.classification} |`);
	}

	return lines.join('\n');
}

function compareWordPressRestWaterfalls({ baseline, candidate }) {
	const baselineRows = Array.isArray(baseline?.rows) ? baseline.rows : [];
	const candidateRows = Array.isArray(candidate?.rows) ? candidate.rows : [];
	const candidateByKey = new Map();
	for (const row of candidateRows) {
		const key = restKey(row);
		if (!candidateByKey.has(key)) {
			candidateByKey.set(key, []);
		}
		candidateByKey.get(key).push(row);
	}
	const rows = [];
	const seen = new Set();
	for (const row of baselineRows) {
		const key = restKey(row);
		seen.add(key);
		const candidateMatch = (candidateByKey.get(key) || []).shift();
		rows.push({
			url: row.url,
			method: row.method,
			baselineSource: row.source,
			candidateSource: candidateMatch?.source || 'missing',
			baselineNetwork: Boolean(row.networkMatched),
			candidateNetwork: Boolean(candidateMatch?.networkMatched),
			baselineDurationMs: row.durationMs,
			candidateDurationMs: candidateMatch?.durationMs,
			result: row.networkMatched && !candidateMatch?.networkMatched ? 'removed-network' : candidateMatch ? 'unchanged' : 'missing',
		});
	}
	for (const [key, candidateMatches] of candidateByKey.entries()) {
		if (seen.has(key)) {
			continue;
		}
		for (const row of candidateMatches) {
			rows.push({
				url: row.url,
				method: row.method,
				baselineSource: 'missing',
				candidateSource: row.source,
				baselineNetwork: false,
				candidateNetwork: Boolean(row.networkMatched),
				candidateDurationMs: row.durationMs,
				result: row.networkMatched ? 'new-network' : 'new-preloaded-or-cache',
			});
		}
	}
	const counts = {
		baselineNetwork: baselineRows.filter((row) => row.networkMatched).length,
		candidateNetwork: candidateRows.filter((row) => row.networkMatched).length,
		removedNetwork: rows.filter((row) => row.result === 'removed-network').length,
		newNetwork: rows.filter((row) => row.result === 'new-network').length,
	};
	return {
		counts,
		baseline,
		candidate,
		rows,
		unusedPreloadRows: candidate?.unusedPreloadRows || [],
		remainingNetworkOpportunities: classifyWordPressRestPreloadOpportunities(candidate).rows,
	};
}

function formatWordPressRestWaterfallMarkdownReport(comparison, options = {}) {
	const rows = Array.isArray(comparison?.rows) ? comparison.rows : [];
	const limit = options.limit || 30;
	const lines = [
		'## REST waterfall comparison',
		'',
		'| Endpoint | Baseline | Candidate | Result |',
		'|---|---:|---:|---|',
	];
	for (const row of rows.slice(0, limit)) {
		lines.push(`| \`${row.method} ${row.url}\` | ${row.baselineSource} | ${row.candidateSource} | ${row.result} |`);
	}
	const opportunities = comparison?.remainingNetworkOpportunities || [];
	if (opportunities.length > 0) {
		lines.push('', '## Remaining preload opportunities', '', '| Endpoint | Classification | Reason |', '|---|---|---|');
		for (const row of opportunities.slice(0, limit)) {
			lines.push(`| \`${row.method} ${row.url}\` | ${row.classification} | ${row.reason} |`);
		}
	}
	const unusedPreloads = comparison?.candidate?.unusedPreloadRows || comparison?.unusedPreloadRows || [];
	if (unusedPreloads.length > 0) {
		lines.push('', '## Unused REST preloads', '', '| Endpoint | Payload bytes |', '|---|---:|');
		for (const row of unusedPreloads.slice(0, limit)) {
			lines.push(`| \`${row.method} ${row.url}\` | ${row.payloadBytes || 0} |`);
		}
	}
	return lines.join('\n');
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

function addGateRecommendation(recommendations, gate) {
	recommendations.push({
		status: gate.available === false ? 'skipped' : 'recommended',
		...gate,
	});
}

function pickNumber(source, keys) {
	if (!source || typeof source !== 'object') {
		return undefined;
	}
	for (const key of keys) {
		const value = source[key];
		if (typeof value === 'number' && Number.isFinite(value)) {
			return value;
		}
	}
	return undefined;
}

function maxNumber(values) {
	const numbers = values.filter((value) => typeof value === 'number' && Number.isFinite(value));
	return numbers.length > 0 ? Math.max(...numbers) : undefined;
}

function sumNumbers(rows, keys) {
	if (!Array.isArray(rows)) {
		return undefined;
	}
	let total = 0;
	let found = false;
	for (const row of rows) {
		const value = pickNumber(row, keys);
		if (value !== undefined) {
			total += value;
			found = true;
		}
	}
	return found ? total : undefined;
}

function normalizeGateInput(input = {}) {
	const comparison = input.comparison || (input.counts && Array.isArray(input.rows) ? input : undefined);
	const baseline = input.baselineProfile || input.baseline;
	const candidate = input.candidateProfile || input.candidate || (comparison ? undefined : input);
	return { comparison, baseline, candidate };
}

function metricFromProfile(profile, keys) {
	return pickNumber(profile?.metrics, keys)
		?? pickNumber(profile?.browserMetrics, keys)
		?? pickNumber(profile?.diagnosis?.summary, keys)
		?? pickNumber(profile, keys);
}

function restNetworkCount(source) {
	return pickNumber(source?.counts, ['candidateNetwork'])
		?? pickNumber(source?.restWaterfall?.counts, ['network'])
		?? pickNumber(source?.resources, ['restCount']);
}

function lateRestCount(source) {
	return pickNumber(source?.restWaterfall?.counts, ['afterReady'])
		?? pickNumber(source?.diagnosis?.summary, ['restAfterReadyCount']);
}

function serverRequestDurationMs(source) {
	return maxNumber([
		metricFromProfile(source, ['serverRequestDurationMs', 'wordpressRequestDurationMs']),
		maxNumber((source?.correlation?.correlated || []).map((row) => row.wordpressDurationMs)),
	]);
}

function gateThreshold(baselineValue, regressionThreshold) {
	return typeof baselineValue === 'number' ? round(baselineValue + regressionThreshold) : undefined;
}

function recommendWordPressPerformanceGates(profileOrComparison, options = {}) {
	if (!profileOrComparison || typeof profileOrComparison !== 'object') {
		throw new TypeError('recommendWordPressPerformanceGates requires a profile or comparison object');
	}
	const thresholds = { ...DEFAULT_GATE_THRESHOLDS, ...(options.thresholds || {}) };
	const { comparison, baseline, candidate } = normalizeGateInput(profileOrComparison);
	const recommendations = [];
	const baselineFailed = metricFromProfile(baseline, ['failedRequestCount', 'browser_failed_request_count']);
	const candidateFailed = metricFromProfile(candidate, ['failedRequestCount', 'browser_failed_request_count']);
	const baselineConsoleErrors = metricFromProfile(baseline, ['consoleErrorCount', 'pageErrorCount', 'browser_console_error_count', 'browser_page_error_count']);
	const candidateConsoleErrors = metricFromProfile(candidate, ['consoleErrorCount', 'pageErrorCount', 'browser_console_error_count', 'browser_page_error_count']);
	const baselineRestNetwork = pickNumber(comparison?.counts, ['baselineNetwork']) ?? restNetworkCount(baseline);
	const candidateRestNetwork = pickNumber(comparison?.counts, ['candidateNetwork']) ?? restNetworkCount(candidate);
	const baselineLateRest = lateRestCount(baseline);
	const candidateLateRest = lateRestCount(candidate);
	const baselineReadyMs = metricFromProfile(baseline, ['readyMs']);
	const candidateReadyMs = metricFromProfile(candidate, ['readyMs']);
	const baselineNetworkIdleMs = metricFromProfile(baseline, ['networkIdleAfterReadyMs', 'browser_network_idle_ms']);
	const candidateNetworkIdleMs = metricFromProfile(candidate, ['networkIdleAfterReadyMs', 'browser_network_idle_ms']);
	const unusedPreloadCount = metricFromProfile(candidate, ['unusedPreloadCount', 'wordpressUnusedPreloadCount', 'wordpress_unused_preload_count']);
	const preloadPayloadBytes = metricFromProfile(candidate, ['preloadPayloadBytes', 'wordpressPreloadPayloadBytes', 'wordpress_preload_payload_bytes'])
		?? sumNumbers(candidate?.preloads || candidate?.preloadRequests, ['bytes', 'payloadBytes', 'transferSize']);
	const baselineServerMs = serverRequestDurationMs(baseline);
	const candidateServerMs = serverRequestDurationMs(candidate);
	const restNetworkThreshold = baselineRestNetwork ?? candidateRestNetwork;
	const lateRestThreshold = baselineLateRest ?? candidateLateRest;
	const readyMsThreshold = baselineReadyMs !== undefined
		? gateThreshold(baselineReadyMs, thresholds.readyMsRegression)
		: gateThreshold(candidateReadyMs, thresholds.readyMsRegression);
	const networkIdleMsThreshold = baselineNetworkIdleMs !== undefined
		? gateThreshold(baselineNetworkIdleMs, thresholds.networkIdleMsRegression)
		: gateThreshold(candidateNetworkIdleMs, thresholds.networkIdleMsRegression);
	const serverMsThreshold = baselineServerMs !== undefined
		? gateThreshold(baselineServerMs, thresholds.serverRequestDurationMsRegression)
		: gateThreshold(candidateServerMs, thresholds.serverRequestDurationMsRegression);

	addGateRecommendation(recommendations, {
		id: 'wordpress.no_new_failed_requests',
		metric: 'wordpress_failed_request_count',
		description: 'No new failed REST/browser requests.',
		operator: '<=',
		threshold: baselineFailed ?? 0,
		baselineValue: baselineFailed,
		candidateValue: candidateFailed,
		available: candidateFailed !== undefined || baselineFailed !== undefined,
		reason: candidateFailed === undefined && baselineFailed === undefined ? 'failed request counts were not captured' : undefined,
	});
	addGateRecommendation(recommendations, {
		id: 'wordpress.rest_network_count',
		metric: 'wordpress_rest_network_count',
		description: 'Candidate REST network count does not increase.',
		operator: '<=',
		threshold: restNetworkThreshold,
		baselineValue: baselineRestNetwork,
		candidateValue: candidateRestNetwork,
		available: restNetworkThreshold !== undefined,
		reason: restNetworkThreshold === undefined ? 'REST network counts were not captured' : undefined,
	});
	addGateRecommendation(recommendations, {
		id: 'wordpress.late_rest_count',
		metric: 'wordpress_late_rest_count',
		description: 'Candidate late REST count does not increase.',
		operator: '<=',
		threshold: lateRestThreshold,
		baselineValue: baselineLateRest,
		candidateValue: candidateLateRest,
		available: lateRestThreshold !== undefined,
		reason: lateRestThreshold === undefined ? 'late REST counts were not captured' : undefined,
	});
	addGateRecommendation(recommendations, {
		id: 'wordpress.ready_ms',
		metric: 'wordpress_ready_ms',
		description: 'Ready time does not regress beyond threshold.',
		operator: '<=',
		threshold: readyMsThreshold,
		baselineValue: baselineReadyMs,
		candidateValue: candidateReadyMs,
		available: readyMsThreshold !== undefined,
		reason: readyMsThreshold === undefined ? 'ready timings were not captured' : undefined,
	});
	addGateRecommendation(recommendations, {
		id: 'wordpress.network_idle_ms',
		metric: 'wordpress_network_idle_after_ready_ms',
		description: 'Network-idle time does not regress beyond threshold.',
		operator: '<=',
		threshold: networkIdleMsThreshold,
		baselineValue: baselineNetworkIdleMs,
		candidateValue: candidateNetworkIdleMs,
		available: networkIdleMsThreshold !== undefined,
		reason: networkIdleMsThreshold === undefined ? 'network-idle timings were not captured' : undefined,
	});
	addGateRecommendation(recommendations, {
		id: 'wordpress.unused_preload_count',
		metric: 'wordpress_unused_preload_count',
		description: 'Unused preload count is zero or under threshold when available.',
		operator: '<=',
		threshold: thresholds.unusedPreloadCount,
		candidateValue: unusedPreloadCount,
		available: unusedPreloadCount !== undefined,
		reason: unusedPreloadCount === undefined ? 'unused preload counts were not captured' : undefined,
	});
	addGateRecommendation(recommendations, {
		id: 'wordpress.preload_payload_bytes',
		metric: 'wordpress_preload_payload_bytes',
		description: 'Preload payload bytes stay under threshold when available.',
		operator: '<=',
		threshold: thresholds.preloadPayloadBytes,
		candidateValue: preloadPayloadBytes,
		available: preloadPayloadBytes !== undefined,
		reason: preloadPayloadBytes === undefined ? 'preload payload bytes were not captured' : undefined,
	});
	addGateRecommendation(recommendations, {
		id: 'wordpress.server_request_duration_ms',
		metric: 'wordpress_server_request_duration_ms',
		description: 'Server request duration does not regress beyond threshold when available.',
		operator: '<=',
		threshold: serverMsThreshold,
		baselineValue: baselineServerMs,
		candidateValue: candidateServerMs,
		available: serverMsThreshold !== undefined,
		reason: serverMsThreshold === undefined ? 'server request durations were not captured' : undefined,
	});
	addGateRecommendation(recommendations, {
		id: 'wordpress.no_new_console_errors',
		metric: 'wordpress_console_error_count',
		description: 'No new console/page errors when available.',
		operator: '<=',
		threshold: baselineConsoleErrors ?? 0,
		baselineValue: baselineConsoleErrors,
		candidateValue: candidateConsoleErrors,
		available: candidateConsoleErrors !== undefined || baselineConsoleErrors !== undefined,
		reason: candidateConsoleErrors === undefined && baselineConsoleErrors === undefined ? 'console/page error counts were not captured' : undefined,
	});

	return {
		thresholds,
		recommendations,
		recommended: recommendations.filter((gate) => gate.status === 'recommended'),
		skipped: recommendations.filter((gate) => gate.status === 'skipped'),
	};
}

function formatWordPressPerformanceGateReport(recommendations, options = {}) {
	const gates = Array.isArray(recommendations)
		? recommendations
		: Array.isArray(recommendations?.recommendations) ? recommendations.recommendations : [];
	const title = options.title || 'WordPress performance gate recommendations';
	const lines = [
		`## ${title}`,
		'',
		'| Gate | Metric | Operator | Threshold | Candidate | Status |',
		'|---|---|---:|---:|---:|---|',
	];
	for (const gate of gates) {
		const threshold = gate.threshold === undefined ? '-' : gate.threshold;
		const candidate = gate.candidateValue === undefined ? '-' : gate.candidateValue;
		lines.push(`| ${gate.description || gate.id} | \`${gate.metric}\` | ${gate.operator || '<='} | ${threshold} | ${candidate} | ${gate.status} |`);
	}
	const skipped = gates.filter((gate) => gate.status === 'skipped' && gate.reason);
	if (skipped.length > 0) {
		lines.push('', '## Skipped gates', '');
		for (const gate of skipped) {
			lines.push(`- \`${gate.id}\`: ${gate.reason}`);
		}
	}
	return lines.join('\n');
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
	const budgets = options.budgets || profile.budgets || {};
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
	const restPayloadBudgets = evaluateWordPressRestPayloadBudgets(profile.restWaterfall, budgets, {
		phase: 'page-profile',
		subject: profile.id || profile.path || 'WordPress page profile',
	});

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
	findings.push(...restPayloadBudgets.findings);

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
		restPayloadBudgets,
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
	if (input.restInstrumentation !== false && typeof page.addInitScript === 'function') {
		await installWordPressRestInstrumentation(page);
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
	const restObservationMs = Number(spec.restObservationMs ?? input.restObservationMs ?? DEFAULT_REST_OBSERVATION_MS);
	if (restObservationMs > 0) {
		await sleep(restObservationMs);
	}
	const resources = await collectBrowserResourceTimings(page, spec.resources || {});
	const resourceSummary = summarizeResourceTimings(resources);
	const apiFetchAttempts = typeof page.evaluate === 'function'
		? await collectWordPressRestAttempts(page).catch(() => [])
		: [];
	const restPreloads = [
		...normalizeRestPreloadList(input.preloadedRestPaths),
		...normalizeRestPreloadList(input.restPreloads),
		...normalizeRestPreloadList(input.preloadMetadata),
		...normalizeRestPreloadList(spec.preloadedRestPaths),
		...normalizeRestPreloadList(spec.restPreloads),
		...normalizeRestPreloadList(spec.preloadMetadata),
	];
	if (restPreloads.length === 0 && typeof page.evaluate === 'function') {
		restPreloads.push(...await collectWordPressRestPreloads(page).catch(() => []));
	}
	const restWaterfall = summarizeWordPressRestWaterfall({
		readyMs,
		apiFetchAttempts,
		restPreloads,
		resourceTimings: resources,
		networkRequests: input.networkRequests || [],
	});
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
		restWaterfall,
		budgets: {
			...(input.budgets || {}),
			...(spec.budgets || {}),
		},
		correlation,
	};

	return { ...profile, diagnosis: diagnoseWordPressPageProfile(profile, { budgets: profile.budgets }) };
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
	DEFAULT_REST_OBSERVATION_MS,
	classifyResourceUrl,
	classifyWordPressRestPreloadOpportunities,
	compareWordPressRestNetworkWaterfalls,
	collectBrowserResourceTimings,
	collectWordPressRestAttempts,
	collectWordPressRestPreloads,
	compareWordPressRestWaterfalls,
	diagnoseWordPressPageProfile,
	evaluateWordPressRestPayloadBudgets,
	formatWordPressPerformanceGateReport,
	formatWordPressRestMatrixMarkdownReport,
	formatWordPressRestNetworkDiffMarkdownReport,
	formatWordPressRestPayloadBudgetMarkdownReport,
	formatWordPressRestWaterfallMarkdownReport,
	installWordPressRestInstrumentation,
	normalizePageManifest,
	normalizePageSpec,
	profileWordPressRestMatrix,
	resourceFamily,
	profileWordPressPage,
	profileWordPressPages,
	recommendWordPressPerformanceGates,
	resolveWordPressUrl,
	summarizeWordPressRestNetworkRows,
	summarizeWordPressRestWaterfall,
	summarizeResourceTimings,
	waitForPageReady,
};
