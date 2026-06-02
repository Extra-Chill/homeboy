'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const nodePath = require('node:path');

/**
 * Internal dependencies
 */
const { correlateBrowserAndWordPressTimings, normalizeUrl } = require('./timing-correlator');
const { runWordPressFixtureSetup } = require('./fixture-setup');
const { runWpCodeboxBrowserMetrics } = require('./wp-codebox-browser-metrics');
const {
	WORDPRESS_RESOURCE_INCLUDE,
	isPlainObject,
	sleep,
} = require('./shared');

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
	if (!normalized.state && !normalized.selector && !normalized.frame && !normalized.frameSelector && !normalized.function && !normalized.frameFunction) {
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
		: WORDPRESS_RESOURCE_INCLUDE;
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

function restWaterfallUrlWithoutQueryParam(url, param) {
	const normalized = normalizeRestUrl(url);
	const queryIndex = normalized.indexOf('?');
	if (queryIndex < 0) {
		return normalized;
	}
	const base = normalized.slice(0, queryIndex);
	const params = new URLSearchParams(normalized.slice(queryIndex + 1));
	params.delete(param);
	const query = params.toString();
	return query ? `${base}?${query}` : base;
}

function restWaterfallUrlWithQueryParam(url, param, value) {
	const normalized = normalizeRestUrl(url);
	const queryIndex = normalized.indexOf('?');
	const base = queryIndex >= 0 ? normalized.slice(0, queryIndex) : normalized;
	const params = new URLSearchParams(queryIndex >= 0 ? normalized.slice(queryIndex + 1) : '');
	params.set(param, value);
	return `${base}?${params.toString()}`;
}

function restWaterfallUrlHasQueryParam(url, param) {
	const normalized = normalizeRestUrl(url);
	const queryIndex = normalized.indexOf('?');
	if (queryIndex < 0) {
		return false;
	}
	return new URLSearchParams(normalized.slice(queryIndex + 1)).has(param);
}

function restDiagnosticKey(method, url) {
	return `${normalizeRestMethod(method)} ${normalizeRestUrl(url)}`;
}

function restDiagnosticKeyWithoutQueryParam(row, param) {
	return restDiagnosticKey(row?.method, restWaterfallUrlWithoutQueryParam(row?.url || row?.path || row?.normalizedUrl, param));
}

function restDiagnosticKeyWithQueryParam(row, param, value) {
	return restDiagnosticKey(row?.method, restWaterfallUrlWithQueryParam(row?.url || row?.path || row?.normalizedUrl, param, value));
}

function quotePhpString(value) {
	return `'${String(value || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'")}'`;
}

function phpPreloadDeclaration(method, url) {
	const normalizedUrl = normalizeRestUrl(url);
	if (!normalizedUrl) {
		return '';
	}
	const normalizedMethod = normalizeRestMethod(method);
	if (normalizedMethod === 'GET') {
		return quotePhpString(normalizedUrl);
	}
	return `array( ${quotePhpString(normalizedUrl)}, ${quotePhpString(normalizedMethod)} )`;
}

function compactRestRows(rows) {
	return rows.map((row) => ({
		url: row.url,
		method: row.method,
		status: row.status,
		durationMs: row.durationMs,
		hit: row.hit,
		nextUrl: row.nextUrl || undefined,
		payloadBytes: row.payloadBytes,
		caller: row.caller,
		stackFrames: row.stackFrames,
	}));
}

function uniquePreloadSuggestions(suggestions) {
	const seen = new Set();
	return suggestions.filter((suggestion) => {
		if (!suggestion?.declaration || seen.has(suggestion.declaration)) {
			return false;
		}
		seen.add(suggestion.declaration);
		return true;
	});
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
	const stackFrames = Array.isArray(row?.stackFrames) ? row.stackFrames.filter((frame) => typeof frame === 'string' && frame.trim()).slice(0, 8) : [];
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
		caller: row?.caller || stackFrames[0],
		stackFrames,
	};
}

function normalizeRestPreloadCheck(row) {
	const url = normalizeRestUrl(row?.url || row?.path || row?.route || '');
	const stackFrames = Array.isArray(row?.stackFrames) ? row.stackFrames.filter((frame) => typeof frame === 'string' && frame.trim()).slice(0, 8) : [];
	return {
		source: row?.source || 'preload-check',
		url,
		method: normalizeRestMethod(row?.method),
		nextUrl: normalizeRestUrl(row?.nextUrl || ''),
		startedAtMs: round(row?.startedAtMs ?? row?.startMs),
		resolvedAtMs: round(row?.resolvedAtMs ?? row?.endMs),
		durationMs: round(row?.durationMs),
		hit: Boolean(row?.hit),
		failed: Boolean(row?.failed),
		error: row?.error,
		caller: row?.caller || stackFrames[0],
		stackFrames,
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
			preloads: [],
			preloadChecks: [],
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
		const captureStackFrames = () => {
			const stack = new Error().stack;
			if (typeof stack !== 'string') {
				return [];
			}
			return stack
				.split('\n')
				.map((line) => line.trim())
				.filter(Boolean)
				.filter((line) => !/^Error\b/.test(line))
				.filter((line) => !line.includes('captureStackFrames'))
				.filter((line) => !line.includes('__homeboyWordPressRestProbe'))
				.filter((line) => !line.includes('record ('))
				.filter((line) => !line.includes('wrappedFetch'))
				.filter((line) => !line.includes('wrappedApiFetch'))
				.filter((line) => !line.includes('wrappedNext'))
				.slice(0, 8);
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
				stackFrames: captureStackFrames(),
			};
			entry.caller = entry.stackFrames[0];
			probe.attempts.push(entry);
			return entry;
		};
		const flattenPreloadedData = (preloadedData) => {
			if (!preloadedData || typeof preloadedData !== 'object') {
				return [];
			}
			const rows = [];
			for (const [path, data] of Object.entries(preloadedData)) {
				if (path === 'OPTIONS' && data && typeof data === 'object') {
					for (const [optionsPath, optionsData] of Object.entries(data)) {
						rows.push({ method: 'OPTIONS', path: optionsPath, body: optionsData?.body });
					}
					continue;
				}
				rows.push({ method: 'GET', path, body: data?.body });
			}
			return rows;
		};
		const wrapPreloadingFactory = (apiFetch) => {
			if (typeof apiFetch?.createPreloadingMiddleware !== 'function' || apiFetch.createPreloadingMiddleware.__homeboyRestProbeWrapped) {
				return;
			}
			const originalCreatePreloadingMiddleware = apiFetch.createPreloadingMiddleware.bind(apiFetch);
			const wrappedCreatePreloadingMiddleware = (preloadedData) => {
				probe.preloads.push(...flattenPreloadedData(preloadedData));
				const middleware = originalCreatePreloadingMiddleware(preloadedData);
				return async (options, next) => {
					const checkUrl = normalizePath(options);
					const check = isRest(checkUrl)
						? {
							source: 'preload-check',
							url: checkUrl,
							method: (options?.method || 'GET').toUpperCase(),
							startedAtMs: performance.now() - probe.startedAt,
							stackFrames: captureStackFrames(),
						}
						: null;
					if (check) {
						check.caller = check.stackFrames[0];
					}
					let nextCalled = false;
					const wrappedNext = (nextOptions) => {
						nextCalled = true;
						if (check) {
							check.nextUrl = normalizePath(nextOptions || options);
						}
						return next(nextOptions);
					};
					try {
						const result = await middleware(options, wrappedNext);
						if (check) {
							check.hit = !nextCalled;
							check.resolvedAtMs = performance.now() - probe.startedAt;
							check.durationMs = check.resolvedAtMs - check.startedAtMs;
							probe.preloadChecks.push(check);
						}
						return result;
					} catch (error) {
						if (check) {
							check.failed = true;
							check.error = error?.message || String(error);
							check.hit = !nextCalled;
							check.resolvedAtMs = performance.now() - probe.startedAt;
							check.durationMs = check.resolvedAtMs - check.startedAtMs;
							probe.preloadChecks.push(check);
						}
						throw error;
					}
				};
			};
			wrappedCreatePreloadingMiddleware.__homeboyRestProbeWrapped = true;
			apiFetch.createPreloadingMiddleware = wrappedCreatePreloadingMiddleware;
		};
		const installApiFetchTrap = (wpObject) => {
			if (!wpObject || typeof wpObject !== 'object' || wpObject.__homeboyRestProbeApiFetchTrap) {
				return;
			}
			let currentApiFetch = wpObject.apiFetch;
			try {
				Object.defineProperty(wpObject, 'apiFetch', {
					configurable: true,
					get() {
						return currentApiFetch;
					},
					set(value) {
						currentApiFetch = value;
						wrapPreloadingFactory(currentApiFetch);
					},
				});
				wpObject.__homeboyRestProbeApiFetchTrap = true;
				wrapPreloadingFactory(currentApiFetch);
			} catch {
				// Some environments may expose wp/apiFetch through non-configurable properties.
			}
		};
		let currentWp = window.wp;
		try {
			Object.defineProperty(window, 'wp', {
				configurable: true,
				get() {
					return currentWp;
				},
				set(value) {
					currentWp = value;
					installApiFetchTrap(currentWp);
				},
			});
			installApiFetchTrap(currentWp);
		} catch {
			installApiFetchTrap(window.wp);
		}

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
			wrapPreloadingFactory(apiFetch);
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
			wrapPreloadingFactory(wrappedApiFetch);
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

async function collectWordPressRestPreloadChecks(page) {
	if (!page || typeof page.evaluate !== 'function') {
		throw new TypeError('page must provide evaluate()');
	}
	const checks = await page.evaluate(() => window.__homeboyWordPressRestProbe?.preloadChecks || []);
	return checks.map(normalizeRestPreloadCheck).filter((check) => check.url);
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
	const preloadChecks = Array.isArray(input.preloadChecks) ? input.preloadChecks.map(normalizeRestPreloadCheck).filter((row) => row.url) : [];
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
	const preloadedOrCacheRows = rows.filter((row) => row.source === 'preloaded-or-cache');
	const preloadDiagnostics = diagnoseWordPressRestPreloadMisses({
		networkRows,
		preloads,
		preloadedOrCacheRows,
		apiFetchAttempts,
		preloadChecks,
	});
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
		preloadChecks,
		preloads,
		resourceTimings,
		networkRequests,
		rows,
		networkRows,
		usedPreloadRows,
		unusedPreloadRows,
		remainingRestNetworkRows: networkRows,
		preloadedOrCacheRows,
		afterReadyRows: rows.filter((row) => row.afterReady),
		preloadDiagnostics,
	};
}

function diagnoseWordPressRestPreloadMisses(input = {}) {
	const networkRows = Array.isArray(input.networkRows) ? input.networkRows.map(normalizeRestNetworkRequest).filter((row) => row.url) : [];
	const preloads = Array.isArray(input.preloads) ? input.preloads.map(normalizeRestPreload).filter((row) => row.url) : normalizeRestPreloadList(input.preloads);
	const preloadedOrCacheRows = Array.isArray(input.preloadedOrCacheRows) ? input.preloadedOrCacheRows.map(normalizeApiFetchAttempt).filter((row) => row.url) : [];
	const apiFetchAttempts = Array.isArray(input.apiFetchAttempts) ? input.apiFetchAttempts.map(normalizeApiFetchAttempt).filter((row) => row.url) : [];
	const preloadChecks = Array.isArray(input.preloadChecks) ? input.preloadChecks.map(normalizeRestPreloadCheck).filter((row) => row.url) : [];

	const preloadsByExactKey = new Map();
	const preloadsByLocaleFreeKey = new Map();
	const preloadsByFetchAllKey = new Map();
	const preloadedRowsByExactKey = new Map();
	const preloadedRowsByLocaleFreeKey = new Map();
	const apiFetchByExactKey = new Map();
	const apiFetchByLocaleFreeKey = new Map();
	const preloadChecksByExactKey = new Map();
	const preloadChecksByLocaleFreeKey = new Map();

	const addToMap = (map, key, row) => {
		if (!key) {
			return;
		}
		if (!map.has(key)) {
			map.set(key, []);
		}
		map.get(key).push(row);
	};

	for (const preload of preloads) {
		addToMap(preloadsByExactKey, restKey(preload), preload);
		addToMap(preloadsByLocaleFreeKey, restDiagnosticKeyWithoutQueryParam(preload, '_locale'), preload);
		if (restRouteQuery(preload.url).includes('per_page=-1')) {
			addToMap(preloadsByFetchAllKey, restDiagnosticKeyWithQueryParam(preload, 'per_page', '100'), preload);
		}
	}

	for (const row of preloadedOrCacheRows) {
		addToMap(preloadedRowsByExactKey, restKey(row), row);
		addToMap(preloadedRowsByLocaleFreeKey, restDiagnosticKeyWithoutQueryParam(row, '_locale'), row);
		if (restRouteQuery(row.url).includes('per_page=-1')) {
			addToMap(preloadedRowsByLocaleFreeKey, restDiagnosticKeyWithQueryParam(row, 'per_page', '100'), row);
		}
	}

	for (const attempt of apiFetchAttempts) {
		addToMap(apiFetchByExactKey, restKey(attempt), attempt);
		addToMap(apiFetchByLocaleFreeKey, restDiagnosticKeyWithoutQueryParam(attempt, '_locale'), attempt);
	}

	for (const check of preloadChecks) {
		addToMap(preloadChecksByExactKey, restKey(check), check);
		addToMap(preloadChecksByLocaleFreeKey, restDiagnosticKeyWithoutQueryParam(check, '_locale'), check);
	}

	const rows = networkRows.map((row) => {
		const exactKey = restKey(row);
		const localeFreeKey = restDiagnosticKeyWithoutQueryParam(row, '_locale');
		const fetchAllKey = restDiagnosticKeyWithQueryParam(row, 'per_page', '-1');
		const reasons = [];
		const evidence = {};

		const exactPreloads = preloadsByExactKey.get(exactKey) || [];
		const exactPreloadedRows = preloadedRowsByExactKey.get(exactKey) || [];
		const localeFreePreloads = preloadsByLocaleFreeKey.get(localeFreeKey) || [];
		const localeFreePreloadedRows = preloadedRowsByLocaleFreeKey.get(localeFreeKey) || [];
		const fetchAllPreloads = preloadsByExactKey.get(fetchAllKey) || preloadsByFetchAllKey.get(exactKey) || preloadsByFetchAllKey.get(localeFreeKey) || [];
		const exactPreloadChecks = preloadChecksByExactKey.get(exactKey) || [];
		const localeFreePreloadChecks = preloadChecksByLocaleFreeKey.get(localeFreeKey) || [];
		const exactApiFetchAttempts = apiFetchByExactKey.get(exactKey) || [];
		const localeFreeApiFetchAttempts = apiFetchByLocaleFreeKey.get(localeFreeKey) || [];
		const serverDeclarationSuggestions = uniquePreloadSuggestions([
			...localeFreePreloadChecks.map((check) => ({
				reason: 'api-fetch-pre-middleware-key',
				url: check.url,
				method: check.method,
				declaration: phpPreloadDeclaration(check.method, check.url),
			})),
			...localeFreeApiFetchAttempts.filter((attempt) => attempt.source === 'apiFetch').map((attempt) => ({
				reason: 'api-fetch-pre-middleware-key',
				url: attempt.url,
				method: attempt.method,
				declaration: phpPreloadDeclaration(attempt.method, attempt.url),
			})),
			{
				reason: 'visible-network-url',
				url: row.url,
				method: row.method,
				declaration: phpPreloadDeclaration(row.method, row.url),
			},
		]);
		const attribution = {
			visibleNetwork: {
				url: row.url,
				method: row.method,
				status: row.status,
				durationMs: row.durationMs,
			},
			apiFetchPreMiddleware: compactRestRows(localeFreeApiFetchAttempts.filter((attempt) => attempt.source === 'apiFetch')),
			preloadChecks: compactRestRows([...exactPreloadChecks, ...localeFreePreloadChecks]),
			preloadPayloadEntries: compactRestRows([...exactPreloads, ...localeFreePreloads, ...fetchAllPreloads]),
			serverDeclarationSuggestions,
		};

		if (exactPreloads.length > 0) {
			reasons.push(exactPreloadChecks.some((check) => check.hit === false) ? 'exact-preload-check-missed' : 'exact-preload-still-networked');
			evidence.exactPreloads = exactPreloads.map((preload) => preload.url);
		}
		if (exactPreloadChecks.length > 0) {
			evidence.exactPreloadChecks = exactPreloadChecks.map((check) => ({
				url: check.url,
				method: check.method,
				hit: check.hit,
				nextUrl: check.nextUrl || undefined,
				durationMs: check.durationMs,
			}));
		}
		if (exactPreloadedRows.length > 0) {
			reasons.push('duplicate-or-single-use-consumed');
			evidence.exactPreloadedOrCacheRows = exactPreloadedRows.map((preload) => preload.url);
		}
		if ((localeFreePreloads.length > 0 || fetchAllPreloads.length > 0) && exactPreloads.length === 0 && restWaterfallUrlHasQueryParam(row.url, '_locale')) {
			reasons.push('locale-query-mismatch');
			evidence.localeFreePreloads = localeFreePreloads.map((preload) => preload.url);
		}
		if (localeFreePreloadedRows.length > 0 && exactPreloadedRows.length === 0) {
			reasons.push('locale-query-mismatch-after-preload-hit');
			evidence.localeFreePreloadedOrCacheRows = localeFreePreloadedRows.map((preload) => preload.url);
		}
		if (localeFreePreloadChecks.length > 0 && exactPreloadChecks.length === 0) {
			evidence.localeFreePreloadChecks = localeFreePreloadChecks.map((check) => ({
				url: check.url,
				method: check.method,
				hit: check.hit,
				nextUrl: check.nextUrl || undefined,
				durationMs: check.durationMs,
			}));
		}
		if (fetchAllPreloads.length > 0) {
			reasons.push('fetch-all-per-page-mismatch');
			evidence.fetchAllPreloads = fetchAllPreloads.map((preload) => preload.url);
		}

		if (reasons.length === 0) {
			reasons.push('no-matching-preload');
		}

		return {
			url: row.url,
			method: row.method,
			status: row.status,
			durationMs: row.durationMs,
			reasons,
			primaryReason: reasons[0],
			apiFetchAttempts: exactApiFetchAttempts.map((attempt) => ({
				url: attempt.url,
				method: attempt.method,
				status: attempt.status,
				durationMs: attempt.durationMs,
				caller: attempt.caller,
				stackFrames: attempt.stackFrames,
			})),
			attribution,
			serverDeclarationSuggestions,
			evidence,
		};
	});

	const countsByReason = {};
	for (const row of rows) {
		for (const reason of row.reasons) {
			countsByReason[reason] = (countsByReason[reason] || 0) + 1;
		}
	}

	return {
		count: rows.length,
		countsByReason,
		rows,
	};
}

function restResponseBytes(row) {
	return maxNumber([
		pickNumber(row, ['decodedBodySize']),
		pickNumber(row, ['encodedBodySize']),
		pickNumber(row, ['transferSize']),
		pickNumber(row, ['responseBodyBytes']),
		pickNumber(row, ['preloadPayloadBytes']),
	]) || 0;
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

function createWordPressBudgetFinding({ severity = 'error', contextLabel = 'profile:wordpress-rest', code, message, actual, expected, unit, subject, data = {} }) {
	return {
		category: 'budget',
		context_label: contextLabel,
		passed: false,
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
			const representativeExpectedStatus = Number(expectedStatuses[0]);
			findings.push(createWordPressBudgetFinding({
				contextLabel: 'profile:wordpress-rest-matrix',
				code: 'wordpress.rest_matrix.expected_status',
				message: `REST matrix endpoint returned status ${result.status}`,
				actual: result.status,
				expected: Number.isFinite(representativeExpectedStatus) ? representativeExpectedStatus : 0,
				unit: 'status',
				subject,
				data: { phase: 'rest-matrix', method: result.method, normalizedPath: result.normalizedPath, queryString: result.queryString, expectedStatuses, threshold: expectedStatuses },
			}));
		}
	}
	if (typeof budget.maxBytes === 'number' && result.responseBytes > budget.maxBytes) {
		findings.push(createWordPressBudgetFinding({
			contextLabel: 'profile:wordpress-rest-matrix',
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
			contextLabel: 'profile:wordpress-rest-matrix',
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
			contextLabel: 'profile:wordpress-rest-matrix',
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
		let shape = result.jsonShape?.type || 'unknown';
		if (result.jsonShape?.type === 'object') {
			shape = `object(${(result.topLevelKeys || []).slice(0, 6).join(', ')})`;
		} else if (result.jsonShape?.type === 'array') {
			shape = `array(${result.itemCount || 0})`;
		}
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

	if (method === 'OPTIONS') {
		return { classification: 'options-schema', reason: 'REST schema/options request' };
	}
	const url = normalizeRestWaterfallUrl(row?.url || '');
	const pathOnly = restRoutePath(url);
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
	let sourceRows = [];
	if (Array.isArray(input)) {
		sourceRows = input;
	} else if (Array.isArray(input?.networkRequests)) {
		sourceRows = input.networkRequests;
	} else if (Array.isArray(input?.rows)) {
		sourceRows = input.rows;
	}
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
		let result = 'missing';
		if (row.networkMatched && !candidateMatch?.networkMatched) {
			result = 'removed-network';
		} else if (candidateMatch) {
			result = 'unchanged';
		}
		rows.push({
			url: row.url,
			method: row.method,
			baselineSource: row.source,
			candidateSource: candidateMatch?.source || 'missing',
			baselineNetwork: Boolean(row.networkMatched),
			candidateNetwork: Boolean(candidateMatch?.networkMatched),
			baselineDurationMs: row.durationMs,
			candidateDurationMs: candidateMatch?.durationMs,
			result,
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
	const startedAt = typeof options.startedAt === 'number' ? options.startedAt : Date.now();
	const readiness = {};
	const record = (key) => {
		readiness[key] = Date.now() - startedAt;
	};

	if (spec.state && typeof page.waitForLoadState === 'function') {
		await page.waitForLoadState(spec.state, { timeout });
		record('loadStateMs');
	}

	if (spec.selector) {
		await page.waitForSelector(spec.selector, {
			state: spec.selectorState || 'visible',
			timeout,
		});
		record('selectorMs');
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
			record('frameLoadStateMs');
		}
		if (spec.frameSelector) {
			await frame.waitForSelector(spec.frameSelector, {
				state: spec.frameSelectorState || 'visible',
				timeout,
			});
			record('frameSelectorMs');
		}
		if (spec.frameFunction) {
			if (typeof frame.waitForFunction !== 'function') {
				throw new TypeError('ready.frameFunction requires frame.waitForFunction()');
			}
			await frame.waitForFunction(spec.frameFunction, spec.frameFunctionArg, { timeout });
			record('frameFunctionMs');
		}
	}

	if (spec.function) {
		if (typeof page.waitForFunction !== 'function') {
			throw new TypeError('ready.function requires page.waitForFunction()');
		}
		await page.waitForFunction(spec.function, spec.functionArg, { timeout });
		record('functionMs');
	}

	readiness.readyMs = Date.now() - startedAt;
	return readiness;
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

function readJsonFileIfPresent(filePath) {
	if (!filePath || !fs.existsSync(filePath) || fs.statSync(filePath).size === 0) {
		return null;
	}
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function readJsonlFileIfPresent(filePath) {
	if (!filePath || !fs.existsSync(filePath) || fs.statSync(filePath).size === 0) {
		return [];
	}
	return fs.readFileSync(filePath, 'utf8')
		.split(/\r?\n/)
		.map((line) => line.trim())
		.filter(Boolean)
		.map((line) => JSON.parse(line));
}

function resolveWpCodeboxBrowserDirectory(artifactsDirectory) {
	if (typeof artifactsDirectory !== 'string' || artifactsDirectory.trim() === '') {
		return null;
	}
	const root = nodePath.resolve(artifactsDirectory);
	const candidates = [
		nodePath.join(root, 'files', 'browser'),
		nodePath.basename(root) === 'browser' ? root : undefined,
		nodePath.join(root, 'browser'),
	].filter(Boolean);
	return candidates.find((candidate) => fs.existsSync(candidate) && fs.statSync(candidate).isDirectory()) || null;
}

function timestampOffsetMs(timestamp, startedAt) {
	const value = Date.parse(timestamp || '');
	const start = Date.parse(startedAt || '');
	if (!Number.isFinite(value) || !Number.isFinite(start)) {
		return 0;
	}
	return Math.max(0, value - start);
}

function elapsedMs(startedAt, finishedAt) {
	const start = Date.parse(startedAt || '');
	const finish = Date.parse(finishedAt || '');
	if (!Number.isFinite(start) || !Number.isFinite(finish) || finish < start) {
		return 0;
	}
	return finish - start;
}

function normalizeWpCodeboxNetworkRecord(row, summary = {}) {
	const startMs = timestampOffsetMs(row?.timestamp, summary.startedAt);
	return {
		name: row?.url,
		url: row?.url,
		normalizedUrl: normalizeUrl(row?.url || ''),
		method: normalizeRestMethod(row?.method),
		status: row?.status || 0,
		failed: row?.type === 'requestfailed' || Boolean(row?.failure) || (typeof row?.status === 'number' && row.status >= 400),
		failure: row?.failure,
		initiatorType: row?.resourceType,
		resourceType: row?.resourceType,
		responseContentType: row?.contentType,
		startTime: startMs,
		responseEnd: startMs,
		duration: 0,
		kind: classifyResourceUrl(row?.url),
	};
}

function normalizeWpCodeboxActionRecord(row) {
	return {
		index: row?.index,
		name: row?.action?.name || row?.name || `action_${Number(row?.index || 0) + 1}`,
		type: row?.action?.type || row?.type,
		target: row?.action?.selector || row?.action?.text || row?.action?.url || row?.target,
		status: row?.status === 'ok' ? 'passed' : (row?.status || 'unknown'),
		error: row?.error?.message || row?.error,
	};
}

function resolveWpCodeboxArtifactDirectoryForSpec(input, spec) {
	const byPageId = input.wpCodeboxArtifactsByPageId || input.artifactsByPageId;
	if (byPageId && typeof byPageId === 'object' && spec?.id && byPageId[spec.id]) {
		return byPageId[spec.id];
	}
	return input.wpCodeboxArtifactsDirectory || input.wpCodeboxArtifactDirectory || input.artifactsDirectory || input.artifactDirectory;
}

function readWpCodeboxBrowserProfileArtifacts(artifactsDirectory, wpCodeboxBin = process.env.HOMEBOY_WP_CODEBOX_BIN || 'wp-codebox') {
	const browserDirectory = resolveWpCodeboxBrowserDirectory(artifactsDirectory);
	if (!browserDirectory) {
		throw new Error(`WP Codebox browser artifacts not found under ${artifactsDirectory}`);
	}
	const summary = readJsonFileIfPresent(nodePath.join(browserDirectory, 'summary.json')) || readJsonFileIfPresent(nodePath.join(browserDirectory, 'action-summary.json')) || {};
	const actionSummary = readJsonFileIfPresent(nodePath.join(browserDirectory, 'action-summary.json')) || {};
	const network = readJsonlFileIfPresent(nodePath.join(browserDirectory, 'network.jsonl'));
	const actions = readJsonlFileIfPresent(nodePath.join(browserDirectory, 'actions.jsonl'));
	let parsed = { metrics: {}, artifacts: {} };
	try {
		parsed = runWpCodeboxBrowserMetrics(artifactsDirectory, wpCodeboxBin);
	} catch {
		// Older local wp-codebox fixtures may not expose artifacts browser-metrics.
	}
	return {
		browserDirectory,
		summary,
		actionSummary,
		network,
		actions,
		parsed,
	};
}

function createWordPressPageProfileFromWpCodeboxArtifacts(input, spec) {
	const artifactsDirectory = resolveWpCodeboxArtifactDirectoryForSpec(input, spec);
	const wpCodeboxBin = input.wpCodeboxBin || input.wp_codebox_bin || process.env.HOMEBOY_WP_CODEBOX_BIN || 'wp-codebox';
	const artifactData = readWpCodeboxBrowserProfileArtifacts(artifactsDirectory, wpCodeboxBin);
	const summary = artifactData.summary;
	const actionSummary = artifactData.actionSummary;
	const url = summary.finalUrl || summary.requestedUrl || actionSummary.finalUrl || actionSummary.requestedUrl || resolveWordPressUrl(input.baseUrl || summary.requestedUrl || 'http://example.test/', spec.url);
	const readyMs = round(summary.durationMs || elapsedMs(summary.startedAt, summary.finishedAt));
	const networkRows = artifactData.network.map((row) => normalizeWpCodeboxNetworkRecord(row, summary));
	const resourceSummary = summarizeResourceTimings(networkRows);
	const restPreloads = [
		...normalizeRestPreloadList(input.preloadedRestPaths),
		...normalizeRestPreloadList(input.restPreloads),
		...normalizeRestPreloadList(input.preloadMetadata),
		...normalizeRestPreloadList(spec.preloadedRestPaths),
		...normalizeRestPreloadList(spec.restPreloads),
		...normalizeRestPreloadList(spec.preloadMetadata),
	];
	const restWaterfall = summarizeWordPressRestWaterfall({
		readyMs,
		restPreloads,
		resourceTimings: networkRows,
		networkRequests: [],
	});
	const interactions = {
		actions: artifactData.actions.map(normalizeWpCodeboxActionRecord),
		durationMs: elapsedMs(actionSummary.startedAt, actionSummary.finishedAt),
		failed: artifactData.actions.some((action) => action.status && action.status !== 'ok'),
	};
	const correlation = correlateBrowserAndWordPressTimings({
		browserTimings: networkRows,
		wordpressProfilerRows: input.wordpressProfilerRows || [],
	});
	const profile = {
		id: spec.id,
		label: spec.label,
		url,
		path: new URL(url).pathname + new URL(url).search,
		status: networkRows.find((row) => row.url === normalizeUrl(summary.finalUrl || summary.requestedUrl))?.status || networkRows.find((row) => row.resourceType === 'document')?.status || 0,
		readyMs,
		readiness: {
			readyMs,
			source: 'wp-codebox-browser-artifacts',
			waitFor: summary.waitFor,
		},
		resources: resourceSummary,
		initialResources: resourceSummary,
		interactions,
		interactionResources: summarizeResourceTimings([]),
		restWaterfall,
		interactionRestWaterfall: summarizeWordPressRestWaterfall({ readyMs, restPreloads, resourceTimings: [], networkRequests: [] }),
		budgets: {
			...(input.budgets || {}),
			...(spec.budgets || {}),
		},
		correlation,
		browserMetrics: artifactData.parsed.metrics,
		browserArtifacts: artifactData.parsed.artifacts,
		wpCodebox: {
			artifactBacked: true,
			browserDirectory: artifactData.browserDirectory,
			upstreamGaps: [
				'wordpress.browser-probe network.jsonl does not include request timing or transfer/body size fields, so Homeboy REST waterfall timing/byte gates are partial for WP Codebox artifact-backed runs.',
			],
		},
	};

	return {
		...profile,
		diagnosis: diagnoseWordPressPageProfile(profile, {
			budgets: profile.budgets,
			browserMetrics: profile.browserMetrics,
			networkRequests: networkRows,
		}),
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
	let gates = [];
	if (Array.isArray(recommendations)) {
		gates = recommendations;
	} else if (Array.isArray(recommendations?.recommendations)) {
		gates = recommendations.recommendations;
	}
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

function isFailureFinding(finding) {
	const severity = String(finding?.severity || '').toLowerCase();
	return severity === 'error' || severity === 'fail' || severity === 'failed';
}

function combinedRestWaterfallRows(profile) {
	const rows = [];
	const seen = new Set();
	for (const row of [
		...(Array.isArray(profile?.restWaterfall?.rows) ? profile.restWaterfall.rows : []),
		...(Array.isArray(profile?.interactionRestWaterfall?.rows) ? profile.interactionRestWaterfall.rows : []),
	]) {
		const key = [
			normalizeRestMethod(row?.method),
			normalizeRestWaterfallUrl(row?.url || row?.path || row?.normalizedUrl),
			round(row?.startMs ?? row?.startedAtMs),
			round(row?.durationMs),
			row?.source || '',
		].join('|');
		if (seen.has(key)) {
			continue;
		}
		seen.add(key);
		rows.push(row);
	}
	return rows;
}

function interactionStatus(interactions) {
	const actions = Array.isArray(interactions?.actions) ? interactions.actions : [];
	if (interactions?.failed || actions.some((action) => action.status === 'failed')) {
		return 'failed';
	}
	return actions.length > 0 ? 'passed' : 'none';
}

function summarizeWordPressAdminPageProfile(input = {}) {
	const profile = input.profile || input;
	if (!profile || typeof profile !== 'object' || !profile.resources) {
		throw new TypeError('summarizeWordPressAdminPageProfile requires a page profile object');
	}
	const spec = input.spec || input.pageSpec || {};
	const resources = Array.isArray(profile.resources?.resources) ? profile.resources.resources : [];
	const restRows = combinedRestWaterfallRows(profile);
	const failedRestRows = restRows.filter((row) => row?.failed || row?.status >= 400);
	const failureFindings = (Array.isArray(profile.diagnosis?.findings) ? profile.diagnosis.findings : []).filter(isFailureFinding);
	const readyMs = round(profile.readyMs);

	return {
		id: profile.id || spec.id || profile.path || 'page',
		label: profile.label || spec.label || profile.id || spec.id || profile.path || 'Page',
		url: profile.url || spec.url,
		path: profile.path || spec.path || spec.url || profile.url,
		status: profile.status || 0,
		readyMs,
		resourceCount: profile.resources?.count ?? resources.length,
		restCount: restRows.length || profile.resources?.restCount || 0,
		restBytes: restRows.reduce((total, row) => total + restResponseBytes(row), 0),
		failedRequestCount: failedRestRows.length,
		failureFindingCount: failureFindings.length,
		failureCount: failedRestRows.length + failureFindings.length,
		slowestResources: [...(profile.resources?.slowest || resources)].sort((a, b) => round(b.durationMs) - round(a.durationMs)).slice(0, input.slowestLimit || 5),
		slowestRestRows: [...restRows].sort((a, b) => round(b.durationMs) - round(a.durationMs)).slice(0, input.slowestRestLimit || 5),
		interactionStatus: interactionStatus(profile.interactions),
		interactionActionCount: Array.isArray(profile.interactions?.actions) ? profile.interactions.actions.length : 0,
		findings: failureFindings,
		failedRestRows,
	};
}

function buildWordPressAdminPageSweepSummary(input = {}) {
	let pages = [];
	if (Array.isArray(input)) {
		pages = input;
	} else if (Array.isArray(input.pages)) {
		pages = input.pages;
	}
	if (!Array.isArray(pages)) {
		throw new TypeError('buildWordPressAdminPageSweepSummary requires pages');
	}
	const pageSummaries = pages.map((page) => page?.failureCount !== undefined && page?.restBytes !== undefined
		? page
		: summarizeWordPressAdminPageProfile(page));
	const sortedPages = [...pageSummaries].sort((a, b) => (
		(b.failureCount - a.failureCount)
		|| (b.failedRequestCount - a.failedRequestCount)
		|| (b.readyMs - a.readyMs)
		|| (b.resourceCount - a.resourceCount)
	));
	const slowestRestRows = sortedPages
		.flatMap((page) => page.slowestRestRows.map((row) => ({ ...row, pageId: page.id, pageLabel: page.label })))
		.sort((a, b) => round(b.durationMs) - round(a.durationMs))
		.slice(0, input.slowestRestLimit || 10);

	return {
		pages: sortedPages,
		totals: {
			pageCount: pageSummaries.length,
			failedPageCount: pageSummaries.filter((page) => page.failureCount > 0 || page.interactionStatus === 'failed').length,
			failureCount: pageSummaries.reduce((total, page) => total + page.failureCount, 0),
			failedRequestCount: pageSummaries.reduce((total, page) => total + page.failedRequestCount, 0),
			resourceCount: pageSummaries.reduce((total, page) => total + page.resourceCount, 0),
			restCount: pageSummaries.reduce((total, page) => total + page.restCount, 0),
			restBytes: pageSummaries.reduce((total, page) => total + page.restBytes, 0),
		},
		slowestPages: [...pageSummaries].sort((a, b) => b.readyMs - a.readyMs).slice(0, input.slowestPageLimit || 10),
		slowestRestRows,
	};
}

function escapeMarkdownCell(value) {
	return String(value ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ');
}

function formatRestRowLabel(row) {
	return `\`${escapeMarkdownCell(`${normalizeRestMethod(row?.method)} ${normalizeRestWaterfallUrl(row?.url || row?.path || row?.normalizedUrl)}`)}\``;
}

function formatWordPressAdminPageSweepMarkdownReport(summary, options = {}) {
	const report = summary?.totals ? summary : buildWordPressAdminPageSweepSummary(summary);
	const pageLimit = options.pageLimit || 30;
	const requestLimit = options.requestLimit || 10;
	const lines = [
		`## ${options.title || 'WordPress admin page sweep'}`,
		'',
		`Pages: ${report.totals.pageCount}; failed pages: ${report.totals.failedPageCount}; failures: ${report.totals.failureCount}; REST requests: ${report.totals.restCount}; REST bytes: ${report.totals.restBytes}`,
		'',
		'| Page | Status | Ready ms | Resources | REST | REST bytes | Failed requests | Failures | Interaction |',
		'|---|---:|---:|---:|---:|---:|---:|---:|---|',
	];
	for (const page of report.pages.slice(0, pageLimit)) {
		const pageLabel = page.path ? `${page.label} (${page.path})` : page.label;
		lines.push(`| ${escapeMarkdownCell(pageLabel)} | ${page.status} | ${page.readyMs} | ${page.resourceCount} | ${page.restCount} | ${page.restBytes} | ${page.failedRequestCount} | ${page.failureCount} | ${page.interactionStatus} |`);
	}
	if (report.slowestRestRows.length > 0) {
		lines.push('', '## Slowest REST requests', '', '| Page | Endpoint | Status | Duration ms | Bytes | Source |', '|---|---|---:|---:|---:|---|');
		for (const row of report.slowestRestRows.slice(0, requestLimit)) {
			lines.push(`| ${escapeMarkdownCell(row.pageLabel || row.pageId || '')} | ${formatRestRowLabel(row)} | ${row.status || 0} | ${round(row.durationMs)} | ${restResponseBytes(row)} | ${escapeMarkdownCell(row.source || '')} |`);
		}
	}
	return lines.join('\n');
}

async function runBrowserActions(page, actions, options = {}) {
	if (!Array.isArray(actions) || actions.length === 0) {
		return { actions: [], durationMs: 0, failed: false };
	}
	if (!page || typeof page !== 'object') {
		throw new TypeError('runBrowserActions requires a Playwright page');
	}

	const startedAt = Date.now();
	const evidence = [];
	for (let index = 0; index < actions.length; index += 1) {
		const action = normalizeBrowserAction(actions[index], index, options);
		const actionStartedAt = Date.now();
		const row = {
			index,
			name: action.name,
			type: action.type,
			target: action.target,
			timeoutMs: action.timeout,
			startedAtMs: actionStartedAt - startedAt,
		};
		evidence.push(row);
		try {
			const result = await executeBrowserAction(page, action);
			row.durationMs = Date.now() - actionStartedAt;
			row.status = 'passed';
			if (result) {
				row.result = result;
			}
			if (typeof options.mark === 'function') {
				await options.mark(action.name || `interaction_${index + 1}`);
			}
		} catch (error) {
			row.durationMs = Date.now() - actionStartedAt;
			row.status = 'failed';
			row.error = error?.message || String(error);
			await attachBrowserActionFailureEvidence(page, row, options).catch(() => {});
			const wrapped = new Error(formatBrowserActionFailure(row, error));
			wrapped.cause = error;
			wrapped.action = row;
			wrapped.actions = evidence;
			throw wrapped;
		}
	}

	return {
		actions: evidence,
		durationMs: Date.now() - startedAt,
		failed: false,
	};
}

function normalizeBrowserAction(action, index, options = {}) {
	if (!isPlainObject(action)) {
		throw new TypeError(`browser action ${index} must be an object`);
	}
	const candidates = [
		['click', action.click],
		['clickSelector', action.clickSelector],
		['clickRole', action.clickRole],
		['clickText', action.clickText],
		['fill', action.fill],
		['select', action.select],
		['waitForSelector', action.waitForSelector],
		['waitForResponse', action.waitForResponse],
		['sleep', action.sleep],
	].filter(([, value]) => value !== undefined);
	if (candidates.length !== 1) {
		throw new TypeError(`browser action ${index} must define exactly one supported action`);
	}
	const [type, rawSpec] = candidates[0];
	const spec = typeof rawSpec === 'string' || typeof rawSpec === 'number' ? { value: rawSpec } : { ...(rawSpec || {}) };
	const timeout = Number(action.timeout ?? spec.timeout ?? options.timeout) || 30000;
	const name = action.name || spec.name || `interaction_${index + 1}`;
	return { index, name, type, spec, timeout, target: describeBrowserAction(type, spec) };
}

async function executeBrowserAction(page, action) {
	const { type, spec, timeout } = action;
	if (type === 'sleep') {
		await sleep(Number(spec.ms ?? spec.value ?? 0));
		return null;
	}
	if (type === 'waitForResponse') {
		if (typeof page.waitForResponse !== 'function') {
			throw new TypeError('waitForResponse requires page.waitForResponse()');
		}
		const response = await page.waitForResponse((candidate) => responseMatches(candidate, spec), { timeout });
		return normalizeActionResponse(response);
	}
	if (type === 'waitForSelector') {
		const selector = requiredString(spec.selector ?? spec.value, 'waitForSelector.selector');
		if (Number.isInteger(spec.index)) {
			await selectorLocator(page, selector, spec.index).waitFor({ state: spec.state || 'visible', timeout });
			return null;
		}
		if (typeof page.waitForSelector !== 'function') {
			throw new TypeError('waitForSelector requires page.waitForSelector()');
		}
		await page.waitForSelector(selector, { state: spec.state || 'visible', timeout });
		return null;
	}
	if (type === 'clickRole') {
		const role = requiredString(spec.role, 'clickRole.role');
		await indexedLocator(page.getByRole(role, roleOptions(spec)), spec.index).click({ timeout });
		return null;
	}
	if (type === 'clickText') {
		const text = requiredString(spec.text ?? spec.value, 'clickText.text');
		await indexedLocator(page.getByText(text, { exact: spec.exact }), spec.index).click({ timeout });
		return null;
	}
	if (type === 'click' || type === 'clickSelector') {
		const selector = requiredString(spec.selector ?? spec.value, `${type}.selector`);
		if (Number.isInteger(spec.index)) {
			await selectorLocator(page, selector, spec.index).click({ timeout });
			return null;
		}
		if (typeof page.click === 'function') {
			await page.click(selector, { timeout });
			return null;
		}
		await selectorLocator(page, selector, 0).click({ timeout });
		return null;
	}
	if (type === 'fill') {
		await selectorLocator(page, requiredString(spec.selector, 'fill.selector'), spec.index).fill(String(spec.value ?? ''), { timeout });
		return null;
	}
	if (type === 'select') {
		await selectorLocator(page, requiredString(spec.selector, 'select.selector'), spec.index).selectOption(selectOptionValue(spec), { timeout });
		return null;
	}
	throw new Error(`unsupported browser action type: ${type}`);
}

function selectorLocator(page, selector, index = 0) {
	if (typeof page.locator !== 'function') {
		throw new TypeError('selector actions require page.locator()');
	}
	return indexedLocator(page.locator(selector), index);
}

function indexedLocator(locator, index = 0) {
	return Number.isInteger(index) && index > 0 ? locator.nth(index) : locator;
}

function roleOptions(spec) {
	const options = {};
	if (spec.name !== undefined) {
		options.name = spec.name;
	}
	if (spec.exact !== undefined) {
		options.exact = spec.exact;
	}
	return options;
}

function selectOptionValue(spec) {
	if (spec.optionIndex !== undefined) {
		return { index: spec.optionIndex };
	}
	if (spec.label !== undefined) {
		return { label: spec.label };
	}
	if (spec.value !== undefined) {
		return spec.value;
	}
	throw new TypeError('select requires value, label, or optionIndex');
}

function responseMatches(response, spec) {
	const url = typeof response.url === 'function' ? response.url() : response.url;
	const request = typeof response.request === 'function' ? response.request() : undefined;
	const method = typeof request?.method === 'function' ? request.method() : request?.method;
	const status = typeof response.status === 'function' ? response.status() : response.status;
	if (spec.method && String(method || '').toUpperCase() !== String(spec.method).toUpperCase()) {
		return false;
	}
	if (spec.status !== undefined && Number(status) !== Number(spec.status)) {
		return false;
	}
	if (spec.substring && !String(url).includes(spec.substring)) {
		return false;
	}
	if (spec.url && !String(url).includes(spec.url)) {
		return false;
	}
	if (spec.pattern && !(new RegExp(spec.pattern).test(String(url)))) {
		return false;
	}
	return Boolean(spec.substring || spec.url || spec.pattern || spec.method || spec.status !== undefined);
}

function normalizeActionResponse(response) {
	return response ? {
		url: typeof response.url === 'function' ? response.url() : response.url,
		status: typeof response.status === 'function' ? response.status() : response.status,
	} : null;
}

async function attachBrowserActionFailureEvidence(page, row, options) {
	if (typeof page.screenshot === 'function' && options.failureScreenshotPath) {
		await page.screenshot({ path: options.failureScreenshotPath, fullPage: true });
		row.screenshot = options.failureScreenshotPath;
	}
	if (options.tracePath) {
		row.trace = options.tracePath;
	}
}

function formatBrowserActionFailure(row, error) {
	return [
		`Browser action ${row.index} (${row.name || row.type}) failed`,
		`type=${row.type}`,
		`target=${row.target || 'unknown'}`,
		`timeout=${row.timeoutMs}ms`,
		row.screenshot ? `screenshot=${row.screenshot}` : null,
		row.trace ? `trace=${row.trace}` : null,
		error?.message || String(error),
	].filter(Boolean).join('; ');
}

function describeBrowserAction(type, spec) {
	if (type === 'clickRole') {
		return `role:${spec.role}${spec.name !== undefined ? ` name:${spec.name}` : ''}`;
	}
	if (type === 'clickText') {
		return `text:${spec.text ?? spec.value ?? ''}`;
	}
	if (type === 'waitForResponse') {
		return `response:${spec.substring || spec.url || spec.pattern || spec.method || spec.status || ''}`;
	}
	if (type === 'sleep') {
		return `${spec.ms ?? spec.value ?? 0}ms`;
	}
	return spec.selector ?? spec.value ?? '';
}

function requiredString(value, label) {
	if (typeof value !== 'string' || value.trim() === '') {
		throw new TypeError(`${label} must be a non-empty string`);
	}
	return value;
}

async function profileWordPressPage(input) {
	if (!input || typeof input !== 'object') {
		throw new TypeError('profileWordPressPage requires an input object');
	}
	const { page, baseUrl, wordpressProfilerRows = [], mark } = input;
	const spec = normalizePageSpec(input.spec || input.pageSpec || input.page || {});
	const wpCodeboxArtifactsDirectory = resolveWpCodeboxArtifactDirectoryForSpec(input, spec);
	if (wpCodeboxArtifactsDirectory && (!page || input.useWpCodeboxBrowserArtifacts === true)) {
		return createWordPressPageProfileFromWpCodeboxArtifacts(input, spec);
	}
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
	const commitMs = Date.now() - started;
	if (typeof mark === 'function') {
		await mark(`${spec.id}_commit`);
	}

	const readiness = {
		commitMs,
		...await waitForPageReady(page, spec.ready, { timeout: spec.timeout || 120000, startedAt: started }),
	};
	if (typeof mark === 'function') {
		await mark(`${spec.id}_ready`);
	}

	const readyMs = readiness.readyMs;
	const initialResources = await collectBrowserResourceTimings(page, spec.resources || {}).catch(() => []);
	let interactionActions = [];
	if (Array.isArray(input.interactions)) {
		interactionActions = input.interactions;
	} else if (Array.isArray(spec.interactions)) {
		interactionActions = spec.interactions;
	} else if (Array.isArray(spec.actions)) {
		interactionActions = spec.actions;
	}
	const interactionStartedMs = Date.now() - started;
	const interactions = await runBrowserActions(page, interactionActions, {
		mark,
		timeout: spec.interactionTimeout || input.interactionTimeout,
		failureScreenshotPath: spec.failureScreenshotPath || input.failureScreenshotPath,
		tracePath: spec.tracePath || input.tracePath,
	});
	const restObservationMs = Number(spec.restObservationMs ?? input.restObservationMs ?? DEFAULT_REST_OBSERVATION_MS);
	if (restObservationMs > 0) {
		await sleep(restObservationMs);
	}
	const resources = await collectBrowserResourceTimings(page, spec.resources || {});
	const interactionResources = resources.filter((resource) => typeof resource.startTime === 'number' && resource.startTime >= interactionStartedMs);
	const resourceSummary = summarizeResourceTimings(resources);
	const initialResourceSummary = summarizeResourceTimings(initialResources);
	const interactionResourceSummary = summarizeResourceTimings(interactionResources);
	const apiFetchAttempts = typeof page.evaluate === 'function'
		? await collectWordPressRestAttempts(page).catch(() => [])
		: [];
	const preloadChecks = typeof page.evaluate === 'function'
		? await collectWordPressRestPreloadChecks(page).catch(() => [])
		: [];
	const interactionApiFetchAttempts = apiFetchAttempts.filter((attempt) => typeof attempt.startedAtMs === 'number' && attempt.startedAtMs >= interactionStartedMs);
	const interactionPreloadChecks = preloadChecks.filter((check) => typeof check.startedAtMs === 'number' && check.startedAtMs >= interactionStartedMs);
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
		preloadChecks,
		restPreloads,
		resourceTimings: resources,
		networkRequests: input.networkRequests || [],
	});
	const interactionRestWaterfall = summarizeWordPressRestWaterfall({
		readyMs: interactionStartedMs,
		apiFetchAttempts: interactionApiFetchAttempts,
		preloadChecks: interactionPreloadChecks,
		restPreloads,
		resourceTimings: interactionResources,
		networkRequests: (input.networkRequests || []).filter((request) => Number(request?.start_ms ?? request?.startMs ?? request?.startTime) >= interactionStartedMs),
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
		readiness,
		resources: resourceSummary,
		initialResources: initialResourceSummary,
		interactions,
		interactionResources: interactionResourceSummary,
		restWaterfall,
		interactionRestWaterfall,
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
	const fixtureSetup = input.fixtures || input.setupWordPressFixture
		? await runWordPressFixtureSetup(input)
		: undefined;
	const results = [];
	for (const spec of specs) {
		results.push(await profileWordPressPage({ ...input, spec }));
	}
	return {
		pages: results,
		...(fixtureSetup ? { fixtureSetup } : {}),
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
	DEFAULT_REST_OBSERVATION_MS,
	WORDPRESS_RESOURCE_INCLUDE,
	classifyResourceUrl,
	classifyWordPressRestPreloadOpportunities,
	compareWordPressRestNetworkWaterfalls,
	collectBrowserResourceTimings,
	collectWordPressRestAttempts,
	collectWordPressRestPreloads,
	collectWordPressRestPreloadChecks,
	compareWordPressRestWaterfalls,
	createWordPressPageProfileFromWpCodeboxArtifacts,
	diagnoseWordPressPageProfile,
	diagnoseWordPressRestPreloadMisses,
	evaluateWordPressRestPayloadBudgets,
	buildWordPressAdminPageSweepSummary,
	formatWordPressAdminPageSweepMarkdownReport,
	formatWordPressPerformanceGateReport,
	formatWordPressRestMatrixMarkdownReport,
	formatWordPressRestNetworkDiffMarkdownReport,
	formatWordPressRestPayloadBudgetMarkdownReport,
	formatWordPressRestWaterfallMarkdownReport,
	installWordPressRestInstrumentation,
	normalizeBrowserAction,
	runWordPressFixtureSetup,
	normalizePageManifest,
	normalizePageSpec,
	profileWordPressRestMatrix,
	resourceFamily,
	profileWordPressPage,
	profileWordPressPages,
	recommendWordPressPerformanceGates,
	resolveWordPressUrl,
	runBrowserActions,
	summarizeWordPressAdminPageProfile,
	summarizeWordPressRestNetworkRows,
	summarizeWordPressRestWaterfall,
	summarizeResourceTimings,
	waitForPageReady,
};
