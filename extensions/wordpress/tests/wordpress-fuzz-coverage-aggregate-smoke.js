'use strict';

const assert = require('node:assert/strict');

const fixture = require('./fixtures/wordpress-fuzz-coverage-aggregate.json');
const {
	aggregateWordPressFuzzCoverage,
	collectWordPressFuzzCoverageItems,
	formatWordPressFuzzCoverageMarkdownReport,
	normalizeWordPressFuzzCoverageManifest,
	normalizeWordPressFuzzCoverageItem,
} = require('../lib/wordpress-fuzz-coverage-aggregate');

const normalized = normalizeWordPressFuzzCoverageItem({ id: 'hook:init', type: 'hook', status: 'covered' });
assert.equal(normalized.status, 'exercised');
assert.equal(normalized.type, 'hook');

const collected = collectWordPressFuzzCoverageItems(fixture.artifacts);
assert.equal(collected.some((item) => item.id === 'action:save_post' && item.status === 'exercised'), true);
assert.equal(collected.some((item) => item.id === 'route:/wp-json/wp/v2/users' && item.status === 'skipped'), true);
assert.equal(collected.some((item) => item.type === 'php_error' && item.status === 'failed'), true);

const aggregate = aggregateWordPressFuzzCoverage(fixture);
assert.equal(aggregate.schema, 'homeboy/wordpress-fuzz-coverage-aggregate/v1');
assert.equal(aggregate.totals.exercised, 9);
assert.equal(aggregate.totals.skipped, 1);
assert.equal(aggregate.totals.failed, 2);
assert.equal(aggregate.totals.discovered, 0);
assert.equal(aggregate.coverage_summary.surface_count, 12);
assert.equal(aggregate.coverage_summary.exercised_count, 9);
assert.equal(aggregate.coverage_summary.skipped_count, 1);
assert.equal(aggregate.coverage_summary.failed_count, 2);
assert.equal(aggregate.coverage_gaps.length, 3);
assert.equal(aggregate.metadata.surface_count, 12);
assert.equal(aggregate.metadata.exercised_count, 9);
assert.equal(aggregate.metadata.skipped_count, 1);
assert.equal(aggregate.metadata.failed_count, 2);
assert.equal(aggregate.byType.rest_route.exercised, 1);
assert.equal(aggregate.byType.rest_route.skipped, 1);
assert.equal(aggregate.byType.rest_route.failed, 1);
assert.equal(aggregate.byType.action.exercised, 1);
assert.equal(aggregate.byType.filter.exercised, 1);
assert.equal(aggregate.byType.db_table.exercised, 1);
assert.equal(aggregate.gapReport.totals.skipped, 1);
assert.equal(aggregate.gapReport.totals.failed, 2);
assert.equal(aggregate.gapReport.items.some((item) => item.id === 'route:/wp-json/wp/v2/comments'), true);

const markdown = formatWordPressFuzzCoverageMarkdownReport(aggregate);
assert.match(markdown, /Discovered: 0; exercised: 9; skipped: 1; failed: 2/);
assert.match(markdown, /\| rest_route \| 0 \| 1 \| 1 \| 1 \| 3 \|/);
assert.match(markdown, /## Gaps/);
assert.match(markdown, /\/wp\/v2\/comments/);

const manifest = normalizeWordPressFuzzCoverageManifest({
	surfaces: [
		{ type: 'rest', route: '/wp/v2/posts' },
		{ type: 'admin-page', path: 'tools.php' },
		{ type: 'ajax', action: 'heartbeat' },
		{ type: 'db', table: 'wp_posts' },
		{ type: 'frontend', url: '/' },
		{ type: 'block', name: 'core/paragraph' },
		{ type: 'crud-resource', name: 'wp/v2/posts' },
		{ type: 'hook', hook: 'init' },
		{ type: 'capability', name: 'edit_posts' },
		{ type: 'cron', hook: 'wp_version_check' },
		{ type: 'media', name: 'attachment:sample-image' },
		{ type: 'option', name: 'blogname' },
		{ type: 'setting', name: 'permalink_structure' },
		{ type: 'post-type', name: 'post' },
		{ type: 'taxonomy', name: 'category' },
		{ type: 'user', name: 'administrator' },
		{ type: 'wp-cli', command: 'wp option list' },
	],
});
assert.deepEqual(manifest.surfaces.map((surface) => surface.id), [
	'rest:/wp/v2/posts',
	'admin:tools.php',
	'ajax:heartbeat',
	'db:wp_posts',
	'frontend:/',
	'block:core/paragraph',
	'crud:wp/v2/posts',
	'hook:init',
	'capability:edit_posts',
	'cron:wp_version_check',
	'media:attachment:sample-image',
	'option:blogname',
	'setting:permalink_structure',
	'post-type:post',
	'taxonomy:category',
	'user:administrator',
	'wp-cli:wp option list',
]);

const manifestAggregate = aggregateWordPressFuzzCoverage({
	coverage_manifest: manifest,
	artifacts: [{
		exercised: [{ id: 'rest:/wp/v2/posts', type: 'rest_route' }],
		failed: [{ id: 'ajax:heartbeat', type: 'ajax_action' }],
	}],
});
assert.equal(manifestAggregate.coverage_manifest.schema, 'homeboy/wordpress-fuzz-coverage-manifest/v1');
assert.equal(manifestAggregate.totals.discovered, 15);
assert.equal(manifestAggregate.totals.exercised, 1);
assert.equal(manifestAggregate.totals.failed, 1);
assert.equal(manifestAggregate.coverage_gaps.some((item) => item.id === 'admin:tools.php'), true);
assert.equal(manifestAggregate.coverage_gaps.some((item) => item.id === 'ajax:heartbeat' && item.status === 'failed'), true);

console.log('WordPress fuzz coverage aggregate smoke passed.');
