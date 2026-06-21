'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
	WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA,
	normalizeWordPressPerformanceObservation,
} = require('./wordpress-generic-fuzz-primitives');

function asArray(value) {
	if (value === undefined || value === null || value === '') {
		return [];
	}
	return Array.isArray(value) ? value : [value];
}

function isPlainObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value);
}

function numberOrNull(value) {
	if (value === undefined || value === null || value === '') {
		return null;
	}
	const parsed = Number(value);
	return Number.isFinite(parsed) ? parsed : null;
}

function sum(values) {
	return values.reduce((total, value) => total + (numberOrNull(value) || 0), 0);
}

function max(values) {
	const numbers = values.map(numberOrNull).filter((value) => value !== null);
	return numbers.length > 0 ? Math.max(...numbers) : null;
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

function readPageProfileFile(filePath) {
	const parsed = readJsonFileIfPresent(filePath);
	return asArray(parsed?.pages || parsed);
}

function artifactPath(value, baseDirectory) {
	if (typeof value !== 'string' || value.trim() === '') {
		return null;
	}
	return path.isAbsolute(value) || !baseDirectory ? value : path.resolve(baseDirectory, value);
}

function normalizeArtifactRole(value) {
	return String(value || '').trim().toLowerCase().replace(/[-\s]+/g, '_');
}

function loadArtifactRows(input = {}) {
	const baseDirectory = input.artifactsBaseDirectory || input.artifactBaseDirectory || input.artifactsDirectory || input.artifactDirectory;
	const restDbProfiles = [
		...asArray(input.restDbQueryProfiles || input.restDbQueryRows || input.rest_db_query_profiles),
		...asArray(input.restDbQueryProfileArtifactPath || input.restDbQueryProfilePath || input.rest_db_query_profile_path)
			.flatMap((filePath) => readJsonlFileIfPresent(artifactPath(filePath, baseDirectory))),
	];
	const pageProfiles = [
		...asArray(input.pageProfiles || input.adminPageProfiles || input.page_profiles),
		...asArray(input.pageProfileArtifactPath || input.pageProfilePath || input.page_profile_path)
			.flatMap((filePath) => readPageProfileFile(artifactPath(filePath, baseDirectory))),
	];

	for (const artifact of asArray(input.artifactRefs || input.artifact_refs || input.artifacts)) {
		if (!isPlainObject(artifact)) {
			continue;
		}
		const role = normalizeArtifactRole(artifact.role || artifact.artifact_role || artifact.kind || artifact.type);
		const filePath = artifactPath(artifact.path || artifact.file, baseDirectory);
		if (!filePath) {
			continue;
		}
		if (['rest_db_query_profile', 'rest_db_queries', 'wordpress_rest_db_query_profile'].includes(role)) {
			restDbProfiles.push(...readJsonlFileIfPresent(filePath));
		}
		if (['page_profile', 'page_profiles', 'admin_page_profile', 'admin_page_profiles'].includes(role)) {
			const parsed = readJsonFileIfPresent(filePath);
			pageProfiles.push(...asArray(parsed?.pages || parsed));
		}
	}

	return { restDbProfiles, pageProfiles };
}

function restRouteId(row, index) {
	const method = row.method || row.request_method || 'GET';
	const route = row.route || row.path || row.uri || row.url || `rest-${index + 1}`;
	return `${method} ${route}`;
}

function pageProfileId(profile, index) {
	return profile.id || profile.path || profile.url || `page-${index + 1}`;
}

function normalizeRestDbSamples(rows) {
	return rows.map((row, index) => ({
		id: `rest-db-${index + 1}`,
		operation_id: row.operation_id || row.operationId || restRouteId(row, index),
		started_at: row.timestamp || row.started_at || row.startedAt,
		duration_ms: numberOrNull(row.duration_ms ?? row.durationMs),
		metrics: {
			query_count: numberOrNull(row.query_count ?? row.queryCount) || 0,
			query_time_ms: numberOrNull(row.query_time_ms ?? row.queryTimeMs) || 0,
			total_queries: numberOrNull(row.total_queries ?? row.totalQueries),
			status_code: numberOrNull(row.status),
		},
		metadata: {
			source: 'rest-db-query-profiler',
			method: row.method || row.request_method,
			route: row.route || row.path || row.uri || row.url,
			top_query_shapes: asArray(row.top_query_shapes || row.topQueryShapes),
		},
	}));
}

function normalizePageSamples(profiles) {
	return profiles.map((profile, index) => ({
		id: `page-${index + 1}`,
		operation_id: profile.operation_id || profile.operationId || pageProfileId(profile, index),
		duration_ms: numberOrNull(profile.readyMs ?? profile.ready_ms ?? profile.duration_ms ?? profile.durationMs),
		metrics: {
			admin_page_ready_ms: numberOrNull(profile.readyMs ?? profile.ready_ms),
			admin_page_resource_count: numberOrNull(profile.resources?.count ?? profile.resourceCount),
			admin_page_rest_count: numberOrNull(profile.resources?.restCount ?? profile.restCount),
			admin_page_failed_request_count: numberOrNull(profile.failedRequestCount ?? profile.diagnosis?.summary?.failedRequestCount),
			browser_resource_count: numberOrNull(profile.browserMetrics?.browser_resource_count ?? profile.resources?.count),
			browser_request_count: numberOrNull(profile.browserMetrics?.browser_request_count),
			browser_network_idle_ms: numberOrNull(profile.browserMetrics?.browser_network_idle_ms),
		},
		metadata: {
			source: 'page-profiler',
			id: profile.id,
			url: profile.url,
			path: profile.path,
			status: profile.status,
		},
	}));
}

function normalizeTimingSamples(rows, prefix, metricName) {
	return rows.map((row, index) => ({
		id: `${prefix}-${index + 1}`,
		operation_id: row.operation_id || row.operationId || row.id || `${prefix}-${index + 1}`,
		started_at: row.started_at || row.startedAt || row.timestamp,
		finished_at: row.finished_at || row.finishedAt,
		duration_ms: numberOrNull(row.duration_ms ?? row.durationMs ?? row.duration),
		metrics: {
			[metricName]: numberOrNull(row.duration_ms ?? row.durationMs ?? row.duration),
			count: numberOrNull(row.count) || 1,
		},
		metadata: {
			source: prefix,
			...row,
		},
	}));
}

function aggregateWordPressPerformanceMetrics({ restDbProfiles, pageProfiles, httpCalls, memoryObservations, browserTimings, adminPageTimings, hookTimings }) {
	const queryCounts = restDbProfiles.map((row) => row.query_count ?? row.queryCount);
	const queryTimes = restDbProfiles.map((row) => row.query_time_ms ?? row.queryTimeMs);
	const pageReadyTimes = pageProfiles.map((profile) => profile.readyMs ?? profile.ready_ms);
	const pageRestCounts = pageProfiles.map((profile) => profile.resources?.restCount ?? profile.restCount);
	const browserMetricRows = pageProfiles.map((profile) => profile.browserMetrics).filter(isPlainObject);

	return {
		query_count: sum(queryCounts),
		query_time_ms: sum(queryTimes),
		rest_db_profile_count: restDbProfiles.length,
		http_call_count: httpCalls.length,
		memory_peak_bytes: max(memoryObservations.map((row) => row.peak_bytes ?? row.peakBytes ?? row.memory_peak_bytes ?? row.memoryPeakBytes)),
		browser_timing_count: browserTimings.length,
		browser_resource_count: sum(browserMetricRows.map((row) => row.browser_resource_count)),
		browser_request_count: sum(browserMetricRows.map((row) => row.browser_request_count)),
		browser_network_idle_ms: max(browserMetricRows.map((row) => row.browser_network_idle_ms)),
		admin_page_count: pageProfiles.length,
		admin_page_ready_ms: sum(pageReadyTimes),
		admin_page_max_ready_ms: max(pageReadyTimes),
		admin_page_rest_count: sum(pageRestCounts),
		admin_page_timing_count: adminPageTimings.length,
		hook_timing_count: hookTimings.length,
		hook_timing_ms: sum(hookTimings.map((row) => row.duration_ms ?? row.durationMs ?? row.duration)),
	};
}

function buildWordPressPerformanceObservation(input = {}) {
	const { restDbProfiles, pageProfiles } = loadArtifactRows(input);
	const httpCalls = asArray(input.httpCalls || input.http_calls || input.externalHttpCalls || input.external_http_calls);
	const memoryObservations = asArray(input.memoryObservations || input.memory || input.memory_observations);
	const browserTimings = asArray(input.browserTimings || input.browser_timing || input.browser_timings);
	const adminPageTimings = asArray(input.adminPageTimings || input.admin_page_timings);
	const hookTimings = asArray(input.hookTimings || input.hook_timing || input.hook_timings);
	const samples = [
		...normalizeRestDbSamples(restDbProfiles),
		...normalizePageSamples(pageProfiles),
		...normalizeTimingSamples(httpCalls, 'http-call', 'http_call_duration_ms'),
		...normalizeTimingSamples(memoryObservations, 'memory', 'memory_duration_ms'),
		...normalizeTimingSamples(browserTimings, 'browser-timing', 'browser_duration_ms'),
		...normalizeTimingSamples(adminPageTimings, 'admin-page-timing', 'admin_page_duration_ms'),
		...normalizeTimingSamples(hookTimings, 'hook-timing', 'hook_duration_ms'),
	];

	return normalizeWordPressPerformanceObservation({
		schema: WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA,
		id: input.id || 'wordpress-performance-observation',
		status: input.status || 'passed',
		operation_id: input.operation_id || input.operationId,
		fixture_id: input.fixture_id || input.fixtureId,
		persona_id: input.persona_id || input.personaId,
		started_at: input.started_at || input.startedAt,
		finished_at: input.finished_at || input.finishedAt,
		duration_ms: input.duration_ms ?? input.durationMs,
		metrics: {
			...aggregateWordPressPerformanceMetrics({ restDbProfiles, pageProfiles, httpCalls, memoryObservations, browserTimings, adminPageTimings, hookTimings }),
			...(isPlainObject(input.metrics) ? input.metrics : {}),
		},
		budgets: input.budgets,
		regressions: asArray(input.regressions),
		samples,
		runtime: input.runtime,
		metadata: {
			...(isPlainObject(input.metadata) ? input.metadata : {}),
			sources: {
				rest_db_profiles: restDbProfiles.length,
				page_profiles: pageProfiles.length,
				http_calls: httpCalls.length,
				memory_observations: memoryObservations.length,
				browser_timings: browserTimings.length,
				admin_page_timings: adminPageTimings.length,
				hook_timings: hookTimings.length,
			},
		},
	});
}

module.exports = {
	WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA,
	aggregateWordPressPerformanceMetrics,
	buildWordPressPerformanceObservation,
};
