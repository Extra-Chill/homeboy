'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	buildBrowserRequestCoverageArtifact,
	formatBrowserRequestCoverageMarkdownReport,
	normalizeBrowserRequestCoverageRecord,
} = require('../lib/browser-request-coverage');

const artifact = buildBrowserRequestCoverageArtifact({
	requests: [{
		type: 'response',
		method: 'get',
		url: 'https://example.test/wp-json/wp/v2/posts?search=REDACTED',
		resourceType: 'fetch',
		status: 200,
		transferSize: 120,
		responseBodySize: 80,
		timestamp: '2026-01-01T00:00:01Z',
	}, {
		type: 'requestfailed',
		method: 'POST',
		url: 'https://api.example.test/submit?token=REDACTED',
		resourceType: 'xhr',
		timestamp: '2026-01-01T00:00:02Z',
	}],
});

assert.equal(artifact.schema, 'homeboy/browser-request-coverage/v1');
assert.equal(artifact.totals.requests, 2);
assert.equal(artifact.totals.responses, 1);
assert.equal(artifact.totals.failures, 1);
assert.equal(artifact.totals.hosts, 2);
assert.equal(artifact.byHost['example.test'].responses, 1);
assert.equal(artifact.byResourceType.xhr.failures, 1);
assert.equal(artifact.byMethod.POST.requests, 1);
assert.equal(normalizeBrowserRequestCoverageRecord({ url: 'https://example.test/', method: 'post' }).method, 'POST');
assert.throws(() => normalizeBrowserRequestCoverageRecord({ method: 'GET' }), /url/);

const markdown = formatBrowserRequestCoverageMarkdownReport(artifact);
assert.match(markdown, /Requests: 2; responses: 1; failures: 1; hosts: 2; transfer bytes: 120/);
assert.match(markdown, /\| example.test \| 1 \| 1 \| 0 \| 120 \|/);

console.log('Browser request coverage smoke passed.');
