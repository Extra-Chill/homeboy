'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	generateRestDbQueryProfilerPlugin,
	resolveRestDbQueryProfilerPaths,
} = require('../lib/rest-db-query-profiler');

const plugin = generateRestDbQueryProfilerPlugin({ artifactRelativePath: 'wp-content/profiles/rest-db.jsonl' });

assert.match(plugin, /Plugin Name: Homeboy REST DB Query Profiler/);
assert.match(plugin, /rest_pre_dispatch/);
assert.match(plugin, /rest_post_dispatch/);
assert.match(plugin, /\$wpdb->save_queries = true/);
assert.match(plugin, /homeboy\/wordpress-rest-db-query-profile\/v1/);
assert.match(plugin, /'query_count'\s+=> max\( 0, \$end_count - \$start_count \)/);
assert.match(plugin, /homeboy_rest_db_query_profiler_normalize_sql/);
assert.match(plugin, /'top_query_shapes'\s+=> homeboy_rest_db_query_profiler_top_query_shapes\( \$start_count, 5 \)/);
assert.match(plugin, /ABSPATH \. 'wp-content\/profiles\/rest-db\.jsonl'/);

const paths = resolveRestDbQueryProfilerPaths('/tmp/site', { artifactRelativePath: 'wp-content/profiles/rest-db.jsonl' });
assert.equal(paths.artifactRelativePath, 'wp-content/profiles/rest-db.jsonl');
assert.equal(paths.pluginPath.endsWith('wp-content/mu-plugins/homeboy-rest-db-query-profiler.php'), true);

assert.throws(() => resolveRestDbQueryProfilerPaths('/tmp/site', { artifactRelativePath: '../outside.jsonl' }), /inside/);
assert.throws(() => generateRestDbQueryProfilerPlugin({ artifactRelativePath: '/tmp/outside.jsonl' }), /relative/);

console.log('REST DB query profiler smoke passed.');
