'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
	WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA,
	buildWordPressPerformanceObservation,
} = require('../lib/wordpress-performance-observation-aggregate');

function writeJson(filePath, value) {
	fs.writeFileSync(filePath, `${JSON.stringify(value)}\n`, 'utf8');
}

function writeJsonl(filePath, rows) {
	fs.writeFileSync(filePath, `${rows.map((row) => JSON.stringify(row)).join('\n')}\n`, 'utf8');
}

const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-performance-observation-'));

try {
	writeJsonl(path.join(tmpDir, 'rest-db.jsonl'), [
		{ schema: 'homeboy/wordpress-rest-db-query-profile/v1', timestamp: '2026-01-01T00:00:00Z', method: 'GET', route: '/wp/v2/posts', status: 200, duration_ms: 25.5, query_count: 7, query_time_ms: 4.25, total_queries: 20, top_query_shapes: [{ sql: 'SELECT * FROM wp_posts WHERE ID = ?', count: 2, time_ms: 3 }] },
		{ schema: 'homeboy/wordpress-rest-db-query-profile/v1', timestamp: '2026-01-01T00:00:01Z', method: 'POST', route: '/wp/v2/posts', status: 201, duration_ms: 40, query_count: 11, query_time_ms: 9.5, total_queries: 31 },
	]);
	writeJson(path.join(tmpDir, 'pages.json'), {
		pages: [
			{
				id: 'plugins-root',
				path: '/wp-admin/plugins.php',
				status: 200,
				readyMs: 875,
				resources: { count: 7, restCount: 1 },
				browserMetrics: { browser_resource_count: 7, browser_request_count: 3, browser_network_idle_ms: 1200 },
			},
		],
	});

	const observation = buildWordPressPerformanceObservation({
		id: 'fuzz-case-performance',
		operation_id: 'fuzz-case:rest-posts',
		artifactRefs: [
			{ role: 'rest_db_query_profile', path: 'rest-db.jsonl' },
			{ role: 'page_profiles', path: 'pages.json' },
		],
		artifactsBaseDirectory: tmpDir,
		httpCalls: [{ id: 'update-check', duration_ms: 15 }],
		memoryObservations: [{ peak_bytes: 128000000 }],
		browserTimings: [{ id: 'domcontentloaded', duration_ms: 300 }],
		adminPageTimings: [{ id: 'plugins-menu', duration_ms: 875 }],
		hookTimings: [{ id: 'admin_init', hook: 'admin_init', duration_ms: 12 }],
	});

	assert.equal(observation.schema, WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA);
	assert.equal(observation.metrics.query_count, 18);
	assert.equal(observation.metrics.query_time_ms, 13.75);
	assert.equal(observation.metrics.rest_db_profile_count, 2);
	assert.equal(observation.metrics.admin_page_count, 1);
	assert.equal(observation.metrics.admin_page_max_ready_ms, 875);
	assert.equal(observation.metrics.browser_resource_count, 7);
	assert.equal(observation.metrics.browser_network_idle_ms, 1200);
	assert.equal(observation.metrics.http_call_count, 1);
	assert.equal(observation.metrics.memory_peak_bytes, 128000000);
	assert.equal(observation.metrics.hook_timing_count, 1);
	assert.equal(observation.samples.length, 8);
	assert.equal(observation.samples[0].metrics.query_count, 7);
	assert.equal(observation.samples[2].metrics.admin_page_ready_ms, 875);
	assert.equal(observation.metadata.sources.rest_db_profiles, 2);
	assert.equal(observation.metadata.sources.page_profiles, 1);
} finally {
	fs.rmSync(tmpDir, { recursive: true, force: true });
}

console.log('wordpress performance observation aggregate smoke passed');
