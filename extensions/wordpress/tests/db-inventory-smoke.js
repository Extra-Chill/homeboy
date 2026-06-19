'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	buildWordPressDbInventoryArtifact,
	formatWordPressDbInventoryMarkdownReport,
	normalizeWordPressDbInventoryTable,
} = require('../lib/db-inventory');

const artifact = buildWordPressDbInventoryArtifact({
	tables: [{
		Name: 'wp_posts',
		Rows: '12',
		Data_length: '1024',
		Index_length: '256',
		columns: [
			{ Field: 'ID', Type: 'bigint unsigned', Null: 'NO', Key: 'PRI' },
			{ Field: 'post_title', Type: 'text', Null: 'NO' },
		],
		indexes: [
			{ Key_name: 'PRIMARY', Column_name: 'ID', Non_unique: 0, Seq_in_index: 1 },
		],
	}, {
		name: 'wp_options',
		rowCount: 4,
		columns: [{ name: 'option_name', type: 'varchar(191)' }],
		indexes: [{ name: 'option_name', column: 'option_name', unique: true }],
	}],
});

assert.equal(artifact.schema, 'homeboy/wordpress-db-inventory/v1');
assert.equal(artifact.totals.tableCount, 2);
assert.equal(artifact.totals.rowCount, 16);
assert.equal(artifact.totals.columnCount, 3);
assert.equal(artifact.totals.indexCount, 2);
assert.deepEqual(artifact.tables.map((table) => table.name), ['wp_options', 'wp_posts']);
assert.equal(artifact.tables[1].totalBytes, 1280);
assert.equal(artifact.tables[1].columns[0].name, 'ID');

assert.deepEqual(normalizeWordPressDbInventoryTable({ name: 'wp_postmeta', rows: 2 }).columns, []);
assert.throws(() => normalizeWordPressDbInventoryTable({ rows: 2 }), /name/);

const markdown = formatWordPressDbInventoryMarkdownReport(artifact);
assert.match(markdown, /Tables: 2; rows: 16; columns: 3; indexes: 2; bytes: 1280/);
assert.match(markdown, /\| wp_posts \| 12 \| 2 \| 1 \| 1280 \|/);

console.log('DB inventory smoke passed.');
