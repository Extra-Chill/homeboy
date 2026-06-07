'use strict';

/**
 * Internal dependencies
 */
const { groupWordPressRequestProfilerRows } = require('./request-profiler');
const { normalizeUrl } = require('./timing-correlator');

function numericValue(value, fallback = 0) {
	const number = Number(value);
	return Number.isFinite(number) ? number : fallback;
}

function average(values) {
	const numericValues = values.map((value) => numericValue(value, NaN)).filter(Number.isFinite);
	return numericValues.length ? numericValues.reduce((sum, value) => sum + value, 0) / numericValues.length : 0;
}

function normalizeWordPressRouteUri(uri, options = {}) {
	const normalized = normalizeUrl(String(uri || '').replace(/([?&])_locale=user(&?)/g, (match, prefix, suffix) => suffix ? prefix : ''), options);
	return normalized.replace(/[?&]$/, '');
}

function routeLabel(route) {
	const value = typeof route === 'string' ? route : route?.route || route?.url || route?.path || route?.label || 'route';
	return String(value)
		.replace(/^https?:\/\/[^/]+/i, '')
		.replace(/^\/+|\/+$/g, '')
		.replace(/\?.*$/, '')
		.replace(/[^A-Za-z0-9]+/g, '_')
		.replace(/^_+|_+$/g, '')
		.toLowerCase() || 'front_page';
}

function normalizeRouteSpec(route) {
	if (typeof route === 'string') {
		return {
			route,
			label: routeLabel(route),
			match: route,
			exact: false,
		};
	}
	if (!route || typeof route !== 'object') {
		throw new TypeError('route entries must be strings or objects');
	}

	const value = route.route || route.url || route.path || route.match;
	if (typeof value !== 'string' || value.trim() === '') {
		throw new TypeError('route entries require route, url, path, or match');
	}

	return {
		...route,
		route: route.route || value,
		label: route.label || routeLabel(value),
		match: route.match || value,
		exact: route.exact === true,
	};
}

function normalizeRouteSpecs(routes) {
	if (!Array.isArray(routes)) {
		throw new TypeError('routes must be an array');
	}
	return routes.map(normalizeRouteSpec);
}

function routeMatches(route, uri, options = {}) {
	const spec = normalizeRouteSpec(route);
	const normalized = normalizeWordPressRouteUri(uri, options);

	if (spec.regex) {
		const regex = spec.regex instanceof RegExp ? spec.regex : new RegExp(String(spec.regex));
		return regex.test(normalized);
	}

	const key = normalizeWordPressRouteUri(spec.match, options);
	if (spec.exact || key === '/') {
		return normalized === key;
	}
	return normalized === key || normalized.startsWith(key.endsWith('/') ? key : `${key}/`) || normalized.startsWith(`${key}?`);
}

function summarizePriorityBands(rows = []) {
	if (!Array.isArray(rows)) {
		throw new TypeError('rows must be an array');
	}

	const bands = [];
	for (const row of rows) {
		if (Array.isArray(row?.priority_bands)) {
			bands.push(...row.priority_bands);
		}
		if (Array.isArray(row?.priorityBands)) {
			bands.push(...row.priorityBands);
		}
	}

	if (bands.length > 0) {
		return bands
			.map((band) => ({
				hook: band.hook || '',
				duration_ms: numericValue(band.duration_ms ?? band.durationMs, 0),
			}))
			.sort((a, b) => b.duration_ms - a.duration_ms);
	}

	return groupWordPressRequestProfilerRows(rows)
		.flatMap(({ rows: requestRows }) => {
			const sortedRows = [...requestRows].sort((a, b) => numericValue(a?.t_ms, 0) - numericValue(b?.t_ms, 0));
			return sortedRows
				.filter((event) => event?.event === 'hook.priority_band.start')
				.map((start) => {
					const end = sortedRows.find((event) => (
						event?.event === 'hook.priority_band.end' &&
						event?.data?.hook === start?.data?.hook &&
						numericValue(event?.t_ms, 0) >= numericValue(start?.t_ms, 0)
					));
					return end ? {
						hook: start.data?.hook || '',
						duration_ms: numericValue(end.t_ms, 0) - numericValue(start.t_ms, 0),
					} : null;
				})
				.filter(Boolean);
		})
		.sort((a, b) => b.duration_ms - a.duration_ms);
}

function summarizeProfilerRows(rows = []) {
	return groupWordPressRequestProfilerRows(rows).map(({ rows: requestRows }) => {
		const sortedRows = [...requestRows].sort((a, b) => numericValue(a?.t_ms, 0) - numericValue(b?.t_ms, 0));
		const first = sortedRows[0] || {};
		const last = sortedRows[sortedRows.length - 1] || {};
		return {
			request_id: last.request_id || first.request_id || 'unknown',
			uri: last.uri || first.uri || '',
			method: last.method || first.method || '',
			duration_ms: numericValue(last.t_ms, 0),
			priority_bands: summarizePriorityBands(sortedRows),
		};
	});
}

function averageBootstrapDeltas(rows = [], options = {}) {
	const totals = new Map();
	const counts = new Map();
	const limit = Math.max(0, Math.floor(numericValue(options.limit, 8)));

	for (const row of rows || []) {
		for (const event of row?.events || []) {
			const name = event.event || event.name;
			if (!name) {
				continue;
			}
			const delta = numericValue(event.deltaFromPreviousMs ?? event.delta_from_previous_ms ?? event.delta_ms, NaN);
			if (!Number.isFinite(delta)) {
				continue;
			}
			totals.set(name, (totals.get(name) || 0) + delta);
			counts.set(name, (counts.get(name) || 0) + 1);
		}
	}

	const summaries = [...totals.entries()]
		.map(([event, total]) => ({ event, avg_delta_ms: total / counts.get(event) }))
		.sort((a, b) => b.avg_delta_ms - a.avg_delta_ms);

	return limit > 0 ? summaries.slice(0, limit) : summaries;
}

function browserRouteValue(row) {
	return row?.route || row?.url || row?.uri || row?.name || row?.normalizedUrl || '';
}

function browserRowsForRoute(route, browserResults, options = {}) {
	return (browserResults || []).filter((row) => {
		if (row?.route && row.route === route.route) {
			return true;
		}
		return routeMatches(route, browserRouteValue(row), options);
	});
}

function summarizeWordPressRouteLatency(input = {}, options = {}) {
	const routes = normalizeRouteSpecs(input.routes || []);
	const browserResults = Array.isArray(input.browserResults) ? input.browserResults : [];
	const wordpressSummaries = Array.isArray(input.wordpressSummaries)
		? input.wordpressSummaries
		: summarizeProfilerRows(input.wordpressProfilerRows || []);
	const bootstrapSummaries = Array.isArray(input.bootstrapSummaries) ? input.bootstrapSummaries : [];
	const priorityBandLimit = Math.max(0, Math.floor(numericValue(options.priorityBandLimit, 5)));

	return routes.map((route) => {
		const browserRows = browserRowsForRoute(route, browserResults, options);
		const wpRows = wordpressSummaries.filter((row) => routeMatches(route, row.uri, options));
		const bootRows = bootstrapSummaries.filter((row) => routeMatches(route, row.uri, options));
		const priorityBands = summarizePriorityBands(wpRows);

		return {
			route: route.route,
			label: route.label,
			n: browserRows.length,
			status_codes: [...new Set(browserRows.map((row) => row.status ?? row.statusCode).filter((status) => status !== undefined && status !== null))],
			avg_total_ms: average(browserRows.map((row) => row.total_ms ?? row.totalMs ?? row.duration_ms ?? row.durationMs)),
			avg_headers_ms: average(browserRows.map((row) => row.headers_ms ?? row.headersMs ?? row.ttfb_ms ?? row.ttfbMs)),
			avg_body_bytes: average(browserRows.map((row) => row.body_bytes ?? row.bodyBytes ?? row.transferSizeBytes ?? row.transfer_size_bytes)),
			avg_wordpress_muplugin_to_shutdown_ms: average(wpRows.map((row) => row.duration_ms ?? row.durationMs)),
			avg_entry_to_shutdown_ms: average(bootRows.map((row) => row.duration_ms ?? row.durationMs)),
			avg_outer_ms: bootRows.length
				? average(browserRows.map((row) => row.total_ms ?? row.totalMs ?? row.duration_ms ?? row.durationMs)) - average(bootRows.map((row) => row.duration_ms ?? row.durationMs))
				: 0,
			wordpress_profile_count: wpRows.length,
			bootstrap_profile_count: bootRows.length,
			slowest_bootstrap_deltas: averageBootstrapDeltas(bootRows, options),
			slowest_priority_bands: priorityBandLimit > 0 ? priorityBands.slice(0, priorityBandLimit) : priorityBands,
		};
	});
}

async function profileWordPressRoutes(options = {}) {
	const routes = normalizeRouteSpecs(options.routes || []);
	if (typeof options.requestRoute !== 'function') {
		throw new TypeError('requestRoute must be a function');
	}

	const iterations = Math.max(1, Math.floor(numericValue(options.iterations, 1)));
	const warmupIterations = Math.max(0, Math.floor(numericValue(options.warmupIterations, 0)));
	const browserResults = [];

	for (const route of routes) {
		for (let iteration = 0; iteration < warmupIterations; iteration += 1) {
			await options.requestRoute(route.route, { route, iteration, warmup: true });
		}
		for (let iteration = 0; iteration < iterations; iteration += 1) {
			const result = await options.requestRoute(route.route, { route, iteration, warmup: false });
			browserResults.push({ route: route.route, iteration, ...(result || {}) });
		}
	}

	const wordpressProfilerRows = typeof options.collectWordPressProfilerRows === 'function'
		? await options.collectWordPressProfilerRows()
		: options.wordpressProfilerRows;
	const bootstrapSummaries = typeof options.collectBootstrapSummaries === 'function'
		? await options.collectBootstrapSummaries()
		: options.bootstrapSummaries;

	return {
		browserResults,
		routes: summarizeWordPressRouteLatency({
			routes,
			browserResults,
			wordpressProfilerRows,
			bootstrapSummaries,
		}, options),
	};
}

function formatNumber(value, digits = 1) {
	const number = numericValue(value, NaN);
	return Number.isFinite(number) ? number.toFixed(digits) : '0.0';
}

function formatWordPressRouteLatencyMarkdown(summary, options = {}) {
	const rows = Array.isArray(summary) ? summary : summary?.routes;
	if (!Array.isArray(rows)) {
		throw new TypeError('summary must be an array or object with routes array');
	}

	const title = options.title || 'WordPress Route Latency';
	const lines = [
		`## ${title}`,
		'',
		'| Route | Browser n | Status | Avg total ms | Avg headers ms | Avg WP ms | Avg bootstrap ms | Avg outer ms |',
		'| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |',
	];

	for (const row of rows) {
		lines.push([
			`| \`${row.route}\``,
			row.n || 0,
			(row.status_codes || []).join(', ') || '-',
			formatNumber(row.avg_total_ms),
			formatNumber(row.avg_headers_ms),
			formatNumber(row.avg_wordpress_muplugin_to_shutdown_ms),
			formatNumber(row.avg_entry_to_shutdown_ms),
			formatNumber(row.avg_outer_ms),
		].join(' | ') + ' |');
	}

	return lines.join('\n');
}

module.exports = {
	averageBootstrapDeltas,
	formatWordPressRouteLatencyMarkdown,
	normalizeRouteSpec,
	normalizeRouteSpecs,
	normalizeWordPressRouteUri,
	profileWordPressRoutes,
	routeLabel,
	routeMatches,
	summarizePriorityBands,
	summarizeProfilerRows,
	summarizeWordPressRouteLatency,
};
