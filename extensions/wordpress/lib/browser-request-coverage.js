'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');

function numericValue(value, fallback = 0) {
	const number = Number(value);
	return Number.isFinite(number) ? number : fallback;
}

function sortText(a, b) {
	return String(a || '').localeCompare(String(b || ''));
}

function requestHost(url) {
	try {
		return new URL(String(url)).host || 'unknown';
	} catch {
		return 'unknown';
	}
}

function incrementGroup(group, key, request) {
	const name = key || 'unknown';
	if (!group[name]) {
		group[name] = { requests: 0, responses: 0, failures: 0, transferSizeBytes: 0, responseBodySizeBytes: 0 };
	}
	group[name].requests += 1;
	if (request.type === 'response') {
		group[name].responses += 1;
	}
	if (request.type === 'requestfailed') {
		group[name].failures += 1;
	}
	group[name].transferSizeBytes += numericValue(request.transferSize, 0);
	group[name].responseBodySizeBytes += numericValue(request.responseBodySize, 0);
}

function sortedGroup(group) {
	return Object.fromEntries(Object.entries(group).sort(([a], [b]) => sortText(a, b)));
}

function normalizeBrowserRequest(record = {}) {
	if (!isPlainObject(record)) {
		throw new TypeError('Browser request coverage records must be objects');
	}
	const url = String(record.url || '').trim();
	if (!url) {
		throw new TypeError('Browser request coverage records require url');
	}
	return {
		type: record.type === 'requestfailed' ? 'requestfailed' : 'response',
		method: String(record.method || 'GET').toUpperCase(),
		url,
		host: String(record.host || requestHost(url)),
		resourceType: String(record.resourceType || 'unknown'),
		status: record.status === undefined ? undefined : numericValue(record.status),
		ok: typeof record.ok === 'boolean' ? record.ok : undefined,
		transferSize: numericValue(record.transferSize, 0),
		responseBodySize: numericValue(record.responseBodySize, 0),
		timestamp: String(record.timestamp || ''),
	};
}

function buildBrowserRequestCoverageArtifact(input = {}) {
	const requests = (Array.isArray(input.requests) ? input.requests : []).map(normalizeBrowserRequest).sort((a, b) => sortText(a.timestamp, b.timestamp) || sortText(a.url, b.url));
	const byHost = {};
	const byResourceType = {};
	const byMethod = {};
	for (const request of requests) {
		incrementGroup(byHost, request.host, request);
		incrementGroup(byResourceType, request.resourceType, request);
		incrementGroup(byMethod, request.method, request);
	}
	return {
		schema: 'homeboy/browser-request-coverage/v1',
		type: 'browser-request-coverage',
		totals: {
			requests: requests.length,
			responses: requests.filter((request) => request.type === 'response').length,
			failures: requests.filter((request) => request.type === 'requestfailed').length,
			hosts: Object.keys(byHost).length,
			resourceTypes: Object.keys(byResourceType).length,
			methods: Object.keys(byMethod).length,
			transferSizeBytes: requests.reduce((sum, request) => sum + request.transferSize, 0),
			responseBodySizeBytes: requests.reduce((sum, request) => sum + request.responseBodySize, 0),
		},
		byHost: sortedGroup(byHost),
		byResourceType: sortedGroup(byResourceType),
		byMethod: sortedGroup(byMethod),
		requests,
	};
}

function escapeMarkdownCell(value) {
	return String(value ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ');
}

function formatBrowserRequestCoverageMarkdownReport(input = {}, options = {}) {
	const artifact = input?.schema === 'homeboy/browser-request-coverage/v1'
		? input
		: buildBrowserRequestCoverageArtifact(input);
	const lines = [
		`## ${options.title || 'Browser request coverage'}`,
		'',
		`Requests: ${artifact.totals.requests}; responses: ${artifact.totals.responses}; failures: ${artifact.totals.failures}; hosts: ${artifact.totals.hosts}; transfer bytes: ${artifact.totals.transferSizeBytes}`,
		'',
		'| Host | Requests | Responses | Failures | Transfer bytes |',
		'| --- | ---: | ---: | ---: | ---: |',
	];
	for (const [host, row] of Object.entries(artifact.byHost)) {
		lines.push(`| ${escapeMarkdownCell(host)} | ${row.requests} | ${row.responses} | ${row.failures} | ${row.transferSizeBytes} |`);
	}
	return lines.join('\n');
}

module.exports = {
	buildBrowserRequestCoverageArtifact,
	formatBrowserRequestCoverageMarkdownReport,
	normalizeBrowserRequestCoverageRecord: normalizeBrowserRequest,
};
