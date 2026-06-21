'use strict';

const assert = require('node:assert/strict');

const fixture = require('./fixtures/wordpress-fuzz-coverage-aggregate.json');
const {
	aggregateWordPressFuzzCoverage,
	collectWordPressFuzzCoverageItems,
	formatWordPressFuzzCoverageMarkdownReport,
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

console.log('WordPress fuzz coverage aggregate smoke passed.');
