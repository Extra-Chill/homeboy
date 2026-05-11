'use strict';

/**
 * Correlate browser resource timing entries with WordPress request profiler
 * rows so callers can compare per-request browser TTFB / duration against the
 * WordPress app duration captured by `lib/request-profiler.js`.
 *
 * Inputs are kept generic on purpose so this helper can be reused by any
 * Homeboy-rigs workload that already has access to:
 *
 *   - browser resource timings: anything that exposes a `name` (URL) and
 *     numeric timing fields. Plain `PerformanceResourceTiming` JSON,
 *     Playwright `page.evaluate(() => performance.getEntriesByType('resource'))`
 *     output, Puppeteer / DevTools `Network.responseReceived` derivatives,
 *     or a hand-rolled bag of `{ url, startTime, responseStart, responseEnd }`.
 *   - WordPress profiler rows: rows produced by
 *     `parseWordPressRequestProfileJsonl` / `collectWordPressRequestProfiles`.
 *
 * The helper does not bake in Studio paths, Site Editor URLs, or any other
 * caller-specific assumption.
 */

const REQUEST_START_EVENT = 'request.start';
const REQUEST_END_EVENTS = new Set(['shutdown', 'request.end']);

/**
 * Normalize a URL so equivalent requests collapse onto the same key.
 *
 * Rules:
 *   - Drop the origin (scheme + host + port) so `http://localhost:8881/wp-json`
 *     and `/wp-json` correlate.
 *   - Lowercase the path (URLs are case-sensitive in spec but WordPress REST
 *     routes are conventionally lowercased; callers can opt out via options).
 *   - Drop common cache-busting query params (`_=`, `v=`, `_wpnonce`,
 *     `nocache`, `cb`, `t`) but keep semantically meaningful ones.
 *   - Sort remaining query params alphabetically so order does not matter.
 *   - Strip a trailing slash unless the path is just `/`.
 *   - Drop the URL fragment.
 *
 * Returns the normalized string. For non-string / empty input returns `''`.
 *
 * @param {string} url
 * @param {object} [options]
 * @param {boolean} [options.lowercasePath=true]
 * @param {string[]} [options.dropQueryParams]
 * @returns {string}
 */
function normalizeUrl(url, options = {}) {
	if (typeof url !== 'string' || url.trim() === '') {
		return '';
	}

	const lowercasePath = options.lowercasePath !== false;
	const dropQueryParams = new Set(
		Array.isArray(options.dropQueryParams) && options.dropQueryParams.length > 0
			? options.dropQueryParams
			: ['_', 'v', '_wpnonce', 'nocache', 'cb', 't']
	);

	let path;
	let search = '';
	const trimmed = url.trim();

	try {
		// Use a stub base so URL() accepts paths and absolute URLs alike.
		const parsed = new URL(trimmed, 'http://__homeboy_stub__');
		path = parsed.pathname || '/';
		const params = [];
		for (const [key, value] of parsed.searchParams.entries()) {
			if (dropQueryParams.has(key)) {
				continue;
			}
			params.push([key, value]);
		}
		params.sort((a, b) => (a[0] === b[0] ? a[1].localeCompare(b[1]) : a[0].localeCompare(b[0])));
		if (params.length > 0) {
			search = '?' + params.map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join('&');
		}
	} catch (error) {
		// Fall back to a manual strip for inputs URL() refuses.
		const fragmentIndex = trimmed.indexOf('#');
		const noFragment = fragmentIndex >= 0 ? trimmed.slice(0, fragmentIndex) : trimmed;
		const queryIndex = noFragment.indexOf('?');
		path = queryIndex >= 0 ? noFragment.slice(0, queryIndex) : noFragment;
	}

	if (lowercasePath) {
		path = path.toLowerCase();
	}

	if (path.length > 1 && path.endsWith('/')) {
		path = path.slice(0, -1);
	}

	if (path === '') {
		path = '/';
	}

	return path + search;
}

function pickFirstNumber(source, keys) {
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

function pickFirstString(source, keys) {
	if (!source || typeof source !== 'object') {
		return undefined;
	}
	for (const key of keys) {
		const value = source[key];
		if (typeof value === 'string' && value.trim() !== '') {
			return value.trim();
		}
	}
	return undefined;
}

function pickFirstBoolean(source, keys) {
	if (!source || typeof source !== 'object') {
		return undefined;
	}
	for (const key of keys) {
		const value = source[key];
		if (typeof value === 'boolean') {
			return value;
		}
	}
	return undefined;
}

/**
 * Convert a single browser resource timing entry into a normalized record.
 * Accepts native `PerformanceResourceTiming`-shaped objects as well as
 * looser `{ url, startTime, ttfbMs, durationMs }` shapes.
 *
 * @param {object} entry
 * @param {object} [options]
 * @returns {object|null}
 */
function normalizeBrowserTiming(entry, options = {}) {
	if (!entry || typeof entry !== 'object') {
		return null;
	}

	const rawUrl = pickFirstString(entry, ['name', 'url', 'request_url', 'requestUrl']);
	if (!rawUrl) {
		return null;
	}

	const startTime = pickFirstNumber(entry, ['startTime', 'fetchStart', 'requestStart', 'start_time_ms', 'start_ms', 'startMs']);
	const responseStart = pickFirstNumber(entry, ['responseStart', 'ttfb_ms', 'ttfbMs', 'response_start']);
	const responseEnd = pickFirstNumber(entry, ['responseEnd', 'response_end', 'endMs', 'end_ms', 'response_end_ms']);
	const duration = pickFirstNumber(entry, ['duration', 'duration_ms', 'durationMs']);

	let computedDuration = duration;
	if (computedDuration === undefined && startTime !== undefined && responseEnd !== undefined) {
		computedDuration = Math.max(0, responseEnd - startTime);
	}

	let computedTtfb;
	if (responseStart !== undefined && startTime !== undefined) {
		computedTtfb = Math.max(0, responseStart - startTime);
	} else {
		computedTtfb = pickFirstNumber(entry, ['ttfb', 'ttfb_ms', 'ttfbMs']);
	}

	const initiator = pickFirstString(entry, ['initiatorType', 'initiator_type', 'initiator', 'resourceType']);
	const phase = pickFirstString(entry, ['phase', 'phase_label', 'phaseLabel']);
	const method = pickFirstString(entry, ['method', 'request_method', 'requestMethod']);
	const status = pickFirstNumber(entry, ['status', 'statusCode', 'status_code', 'http_status']);
	const failed = pickFirstBoolean(entry, ['failed', 'error']);

	return {
		url: rawUrl,
		normalizedUrl: normalizeUrl(rawUrl, options),
		method: method ? method.toUpperCase() : undefined,
		status: typeof status === 'number' ? status : undefined,
		failed,
		startTime: typeof startTime === 'number' ? startTime : undefined,
		ttfbMs: typeof computedTtfb === 'number' ? computedTtfb : undefined,
		durationMs: typeof computedDuration === 'number' ? computedDuration : undefined,
		initiatorType: initiator,
		phase,
		raw: entry,
	};
}

function normalizeBrowserProfileTimings(profile, options = {}) {
	if (!profile || typeof profile !== 'object') {
		return [];
	}

	const phases = normalizeProfilePhases(profile);
	const resources = Array.isArray(profile.resources) ? profile.resources : [];
	const network = Array.isArray(profile.network) ? profile.network : [];
	const resourcesByUrl = new Map();

	for (const resource of resources) {
		const rawUrl = pickFirstString(resource, ['name', 'url']);
		const normalizedUrl = normalizeUrl(rawUrl, options);
		if (!normalizedUrl) {
			continue;
		}
		if (!resourcesByUrl.has(normalizedUrl)) {
			resourcesByUrl.set(normalizedUrl, []);
		}
		resourcesByUrl.get(normalizedUrl).push(resource);
	}

	const rows = [];
	for (const entry of network) {
		const rawUrl = pickFirstString(entry, ['url', 'name']);
		const normalizedUrl = normalizeUrl(rawUrl, options);
		const resource = normalizedUrl ? (resourcesByUrl.get(normalizedUrl) || []).shift() : undefined;
		const startTime = pickFirstNumber(entry, ['start_time_ms', 'startTime', 'start_ms', 'startMs'])
			?? pickFirstNumber(resource, ['startTime', 'fetchStart', 'requestStart']);
		const phase = pickFirstString(entry, ['phase', 'phase_label', 'phaseLabel'])
			|| pickFirstString(resource, ['phase', 'phase_label', 'phaseLabel'])
			|| phaseForStartTime(phases, startTime);

		rows.push({
			...resource,
			...entry,
			name: rawUrl,
			url: rawUrl,
			startTime,
			responseStart: pickFirstNumber(resource, ['responseStart']) ?? pickFirstNumber(entry, ['responseStart', 'ttfb_ms', 'ttfbMs']),
			responseEnd: pickFirstNumber(resource, ['responseEnd']) ?? pickFirstNumber(entry, ['response_end_ms', 'responseEnd']),
			durationMs: pickFirstNumber(entry, ['duration_ms', 'durationMs']) ?? pickFirstNumber(resource, ['duration', 'duration_ms', 'durationMs']),
			phase,
		});
	}

	for (const entries of resourcesByUrl.values()) {
		for (const resource of entries) {
			const startTime = pickFirstNumber(resource, ['startTime', 'fetchStart', 'requestStart']);
			rows.push({
				...resource,
				phase: pickFirstString(resource, ['phase', 'phase_label', 'phaseLabel']) || phaseForStartTime(phases, startTime),
			});
		}
	}

	return rows;
}

function normalizeProfilePhases(profile) {
	const phases = [];
	if (profile.phases && typeof profile.phases === 'object' && !Array.isArray(profile.phases)) {
		for (const [name, phase] of Object.entries(profile.phases)) {
			if (!phase || typeof phase !== 'object') {
				continue;
			}
			const startMs = pickFirstNumber(phase, ['start_time_ms', 'startTime', 'start_ms', 'startMs']);
			const endMs = pickFirstNumber(phase, ['end_time_ms', 'endTime', 'end_ms', 'endMs']);
			if (startMs !== undefined) {
				phases.push({ name, startMs, endMs });
			}
		}
	} else if (Array.isArray(profile.phase_marks)) {
		const marks = profile.phase_marks
			.map((mark) => ({
				name: pickFirstString(mark, ['name', 'phase', 'label']),
				startMs: pickFirstNumber(mark, ['start_time_ms', 'startTime', 'start_ms', 'startMs']),
			}))
			.filter((mark) => mark.name && mark.startMs !== undefined)
			.sort((a, b) => a.startMs - b.startMs);
		for (let i = 0; i < marks.length; i++) {
			phases.push({
				name: marks[i].name,
				startMs: marks[i].startMs,
				endMs: marks[i + 1]?.startMs,
			});
		}
	}

	return phases.sort((a, b) => a.startMs - b.startMs);
}

function phaseForStartTime(phases, startTime) {
	if (!Array.isArray(phases) || typeof startTime !== 'number') {
		return undefined;
	}
	let matched;
	for (const phase of phases) {
		const endMs = typeof phase.endMs === 'number' ? phase.endMs : Infinity;
		if (startTime >= phase.startMs && startTime < endMs) {
			matched = phase.name;
		}
	}
	return matched;
}

/**
 * Reduce raw WordPress profiler JSONL rows into per-request summaries keyed
 * by `request_id`. Emits one record per request with the captured URI,
 * method, and observed app duration (last event `t_ms` minus 0, or the
 * delta between the explicit `request.start` and `shutdown` events when
 * present).
 *
 * @param {object[]} rows
 * @param {object} [options]
 * @returns {object[]}
 */
function summarizeWordPressProfilerRows(rows, options = {}) {
	if (!Array.isArray(rows)) {
		throw new TypeError('rows must be an array of WordPress profiler entries');
	}

	const byRequest = new Map();

	for (const row of rows) {
		if (!row || typeof row !== 'object') {
			continue;
		}
		const requestId = typeof row.request_id === 'string' && row.request_id.trim() !== ''
			? row.request_id.trim()
			: '__no_request_id__';

		let bucket = byRequest.get(requestId);
		if (!bucket) {
			bucket = {
				requestId,
				uri: typeof row.uri === 'string' ? row.uri : undefined,
				method: typeof row.method === 'string' ? row.method.toUpperCase() : undefined,
				startMs: undefined,
				endMs: undefined,
				maxTMs: undefined,
				eventCount: 0,
				firstSeenIndex: byRequest.size,
			};
			byRequest.set(requestId, bucket);
		}

		bucket.eventCount += 1;

		if (typeof row.uri === 'string' && row.uri !== '' && !bucket.uri) {
			bucket.uri = row.uri;
		}
		if (typeof row.method === 'string' && !bucket.method) {
			bucket.method = row.method.toUpperCase();
		}

		const tMs = typeof row.t_ms === 'number' ? row.t_ms : undefined;
		if (tMs !== undefined) {
			if (row.event === REQUEST_START_EVENT) {
				bucket.startMs = tMs;
			}
			if (REQUEST_END_EVENTS.has(row.event)) {
				bucket.endMs = tMs;
			}
			if (bucket.maxTMs === undefined || tMs > bucket.maxTMs) {
				bucket.maxTMs = tMs;
			}
		}
	}

	const summaries = [];
	for (const bucket of byRequest.values()) {
		if (bucket.requestId === '__no_request_id__' && bucket.eventCount === 0) {
			continue;
		}
		let durationMs;
		if (bucket.startMs !== undefined && bucket.endMs !== undefined) {
			durationMs = Math.max(0, bucket.endMs - bucket.startMs);
		} else if (bucket.maxTMs !== undefined) {
			durationMs = Math.max(0, bucket.maxTMs - (bucket.startMs ?? 0));
		}
		summaries.push({
			requestId: bucket.requestId === '__no_request_id__' ? undefined : bucket.requestId,
			uri: bucket.uri,
			normalizedUri: bucket.uri ? normalizeUrl(bucket.uri, options) : '',
			method: bucket.method,
			durationMs,
			eventCount: bucket.eventCount,
			firstSeenIndex: bucket.firstSeenIndex,
		});
	}

	// Preserve observation order so repeated endpoints pair in arrival order.
	summaries.sort((a, b) => a.firstSeenIndex - b.firstSeenIndex);
	return summaries;
}

/**
 * Correlate browser resource timings with WordPress profiler rows.
 *
 * Matching strategy:
 *   1. Normalize both sides via `normalizeUrl`.
 *   2. Group WordPress summaries by `${method}::${normalizedUri}` (method
 *      defaults to `GET` when unknown). Browser entries match by the same
 *      key; entries without a method fall back to method-agnostic match.
 *   3. Repeated requests to the same endpoint are paired in arrival order
 *      (FIFO within each URL bucket). Surplus browser entries fall through
 *      to the unmatched bucket; surplus WordPress summaries are reported
 *      under `unmatchedWordPress`.
 *
 * Returned rows include browser duration / TTFB, WordPress duration, and
 * deltas (`browserDurationMs - wordpressDurationMs`,
 * `browserTtfbMs - wordpressDurationMs`). The TTFB-vs-WP-duration delta is
 * the canonical "transport / bootstrap overhead" signal called out in
 * Extra-Chill/homeboy-extensions#451.
 *
 * @param {object} input
 * @param {object[]} input.browserTimings
 * @param {object[]} input.wordpressProfilerRows
 * @param {object} [options]
 * @returns {object}
 */
function correlateBrowserAndWordPressTimings(input, options = {}) {
	if (!input || typeof input !== 'object') {
		throw new TypeError('correlateBrowserAndWordPressTimings requires an input object');
	}
	const { browserTimings, browserProfile, wordpressProfilerRows } = input;

	const rawBrowserTimings = Array.isArray(browserTimings)
		? browserTimings
		: normalizeBrowserProfileTimings(browserProfile, options);

	if (!Array.isArray(rawBrowserTimings)) {
		throw new TypeError('input.browserTimings must be an array');
	}
	if (!Array.isArray(browserTimings) && !browserProfile) {
		throw new TypeError('input.browserTimings must be an array');
	}
	if (!Array.isArray(wordpressProfilerRows)) {
		throw new TypeError('input.wordpressProfilerRows must be an array');
	}

	const normalizedBrowser = rawBrowserTimings
		.map((entry) => normalizeBrowserTiming(entry, options))
		.filter((entry) => entry !== null && entry.normalizedUrl !== '');

	const wpSummaries = summarizeWordPressProfilerRows(wordpressProfilerRows, options);

	const buckets = new Map();
	for (const summary of wpSummaries) {
		if (!summary.normalizedUri) {
			continue;
		}
		const method = (summary.method || 'GET').toUpperCase();
		const key = `${method}::${summary.normalizedUri}`;
		if (!buckets.has(key)) {
			buckets.set(key, []);
		}
		buckets.get(key).push(summary);
	}

	const correlated = [];
	const unmatchedBrowser = [];

	for (const browser of normalizedBrowser) {
		const method = (browser.method || 'GET').toUpperCase();
		const primaryKey = `${method}::${browser.normalizedUrl}`;
		let bucket = buckets.get(primaryKey);

		// If the browser entry didn't carry a method, fall back to any
		// bucket sharing the URL regardless of method.
		if ((!bucket || bucket.length === 0) && !browser.method) {
			for (const [key, value] of buckets) {
				if (value.length > 0 && key.endsWith(`::${browser.normalizedUrl}`)) {
					bucket = value;
					break;
				}
			}
		}

		if (bucket && bucket.length > 0) {
			const summary = bucket.shift();
			const browserMethod = browser.method || method;
			const browserDurationMs = browser.durationMs;
			const browserTtfbMs = browser.ttfbMs;
			const wordpressDurationMs = summary.durationMs;
			const transportDeltaMs = (browserTtfbMs !== undefined && wordpressDurationMs !== undefined)
				? browserTtfbMs - wordpressDurationMs
				: undefined;
			const totalDeltaMs = (browserDurationMs !== undefined && wordpressDurationMs !== undefined)
				? browserDurationMs - wordpressDurationMs
				: undefined;

			correlated.push({
				url: browser.url,
				normalizedUrl: browser.normalizedUrl,
				method,
				browserUrl: browser.url,
				browserMethod,
				browserStatus: browser.status,
				phase: browser.phase,
				initiatorType: browser.initiatorType,
				browserDurationMs,
				browserTtfbMs,
				wordpressDurationMs,
				wordpressRequestId: summary.requestId,
				wordpressUri: summary.uri,
				wordpressNormalizedUri: summary.normalizedUri,
				wordpressMethod: summary.method,
				transportDeltaMs,
				totalDeltaMs,
			});
		} else {
			unmatchedBrowser.push(browser);
		}
	}

	const unmatchedWordPress = [];
	for (const remaining of buckets.values()) {
		for (const summary of remaining) {
			unmatchedWordPress.push(summary);
		}
	}

	const phaseGroups = groupByPhase(correlated);
	const topDeltasByPhase = buildTopDeltasByPhase(correlated, options.topDeltaLimit || 5);
	const metrics = buildCorrelationMetrics(phaseGroups);

	return {
		correlated,
		matchedRows: correlated,
		unmatchedBrowser,
		unmatchedWordPress,
		phaseGroups,
		topDeltasByPhase,
		metrics,
	};
}

function groupByPhase(correlated) {
	const groups = new Map();
	for (const row of correlated) {
		const key = row.phase || '__unphased__';
		if (!groups.has(key)) {
			groups.set(key, {
				phase: row.phase,
				count: 0,
				totalBrowserDurationMs: 0,
				totalBrowserTtfbMs: 0,
				totalWordPressDurationMs: 0,
				totalTransportDeltaMs: 0,
				transportDeltaCount: 0,
				maxTransportDeltaMs: undefined,
			});
		}
		const group = groups.get(key);
		group.count += 1;
		if (typeof row.browserDurationMs === 'number') {
			group.totalBrowserDurationMs += row.browserDurationMs;
		}
		if (typeof row.browserTtfbMs === 'number') {
			group.totalBrowserTtfbMs += row.browserTtfbMs;
		}
		if (typeof row.wordpressDurationMs === 'number') {
			group.totalWordPressDurationMs += row.wordpressDurationMs;
		}
		if (typeof row.transportDeltaMs === 'number') {
			group.totalTransportDeltaMs += row.transportDeltaMs;
			group.transportDeltaCount += 1;
			if (group.maxTransportDeltaMs === undefined || row.transportDeltaMs > group.maxTransportDeltaMs) {
				group.maxTransportDeltaMs = row.transportDeltaMs;
			}
		}
	}

	const result = [];
	for (const group of groups.values()) {
		const count = group.count || 1;
		result.push({
			phase: group.phase,
			count: group.count,
			avgBrowserDurationMs: group.totalBrowserDurationMs / count,
			avgBrowserTtfbMs: group.totalBrowserTtfbMs / count,
			avgWordPressDurationMs: group.totalWordPressDurationMs / count,
			avgTransportDeltaMs: group.transportDeltaCount > 0 ? group.totalTransportDeltaMs / group.transportDeltaCount : undefined,
			maxTransportDeltaMs: group.maxTransportDeltaMs,
		});
	}
	return result;
}

function buildTopDeltasByPhase(correlated, limit) {
	const groups = new Map();
	for (const row of correlated) {
		if (typeof row.transportDeltaMs !== 'number') {
			continue;
		}
		const key = row.phase || '__unphased__';
		if (!groups.has(key)) {
			groups.set(key, { phase: row.phase, rows: [] });
		}
		groups.get(key).rows.push(row);
	}

	return [...groups.values()].map((group) => ({
		phase: group.phase,
		rows: group.rows
			.sort((a, b) => Math.abs(b.transportDeltaMs) - Math.abs(a.transportDeltaMs))
			.slice(0, limit),
	}));
}

function buildCorrelationMetrics(phaseGroups) {
	const phases = {};
	for (const group of phaseGroups) {
		const key = group.phase || 'unphased';
		phases[key] = {
			count: group.count,
			avg_transport_delta_ms: group.avgTransportDeltaMs,
			max_transport_delta_ms: group.maxTransportDeltaMs,
			avg_browser_ttfb_ms: group.avgBrowserTtfbMs,
			avg_browser_duration_ms: group.avgBrowserDurationMs,
			avg_wordpress_duration_ms: group.avgWordPressDurationMs,
		};
	}
	return { phases };
}

function formatNumber(value) {
	return typeof value === 'number' && Number.isFinite(value) ? String(Math.round(value)) : '';
}

function escapeMarkdownCell(value) {
	return String(value ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ');
}

function formatEndpoint(method, url) {
	const endpoint = `${method || ''} ${url || ''}`.trim();
	return endpoint ? `\`${escapeMarkdownCell(endpoint)}\`` : '';
}

function formatTimingCorrelationMarkdownReport(correlation, options = {}) {
	const limit = options.limit || 10;
	const lines = [
		'## WordPress timing correlation',
		'',
		'| Phase | Count | Avg transport delta | Max transport delta | Avg browser TTFB | Avg WP app |',
		'|---|---:|---:|---:|---:|---:|',
	];

	for (const group of correlation.phaseGroups || []) {
		lines.push(`| ${escapeMarkdownCell(group.phase || 'unphased')} | ${group.count} | ${formatNumber(group.avgTransportDeltaMs)} | ${formatNumber(group.maxTransportDeltaMs)} | ${formatNumber(group.avgBrowserTtfbMs)} | ${formatNumber(group.avgWordPressDurationMs)} |`);
	}

	const topDeltas = correlation.topDeltasByPhase || buildTopDeltasByPhase(correlation.correlated || [], limit);
	for (const group of topDeltas) {
		if (!Array.isArray(group.rows) || group.rows.length === 0) {
			continue;
		}
		lines.push(
			'',
			`## Largest transport deltas: ${escapeMarkdownCell(group.phase || 'unphased')}`,
			'',
			'| Browser request | Status | WordPress request | Browser TTFB | Browser duration | WP app | Transport delta |',
			'|---|---:|---|---:|---:|---:|---:|'
		);
		for (const row of group.rows.slice(0, limit)) {
			lines.push(`| ${formatEndpoint(row.browserMethod || row.method, row.browserUrl || row.url)} | ${formatNumber(row.browserStatus)} | ${formatEndpoint(row.wordpressMethod, row.wordpressNormalizedUri || row.normalizedUrl)} | ${formatNumber(row.browserTtfbMs)} | ${formatNumber(row.browserDurationMs)} | ${formatNumber(row.wordpressDurationMs)} | ${formatNumber(row.transportDeltaMs)} |`);
		}
	}

	const unmatchedBrowser = correlation.unmatchedBrowser || [];
	if (unmatchedBrowser.length > 0) {
		lines.push('', '## Unmatched browser rows', '', '| Browser request | Status | Phase | Duration |', '|---|---:|---|---:|');
		for (const row of unmatchedBrowser.slice(0, limit)) {
			lines.push(`| ${formatEndpoint(row.method, row.url)} | ${formatNumber(row.status)} | ${escapeMarkdownCell(row.phase || 'unphased')} | ${formatNumber(row.durationMs)} |`);
		}
	}

	const unmatchedWordPress = correlation.unmatchedWordPress || [];
	if (unmatchedWordPress.length > 0) {
		lines.push('', '## Unmatched WordPress rows', '', '| WordPress request | App duration | Request ID |', '|---|---:|---|');
		for (const row of unmatchedWordPress.slice(0, limit)) {
			lines.push(`| ${formatEndpoint(row.method, row.normalizedUri || row.uri)} | ${formatNumber(row.durationMs)} | ${escapeMarkdownCell(row.requestId || '')} |`);
		}
	}

	return lines.join('\n');
}

module.exports = {
	correlateBrowserAndWordPressTimings,
	formatTimingCorrelationMarkdownReport,
	normalizeBrowserTiming,
	normalizeBrowserProfileTimings,
	normalizeUrl,
	summarizeWordPressProfilerRows,
};
