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

function normalizeColumn(column = {}) {
	if (!isPlainObject(column)) {
		throw new TypeError('DB inventory columns must be objects');
	}
	const name = String(column.name || column.Field || '').trim();
	if (!name) {
		throw new TypeError('DB inventory columns require a name');
	}
	return {
		name,
		type: String(column.type || column.Type || '').trim(),
		nullable: column.nullable ?? column.Null === 'YES' ?? undefined,
		key: String(column.key || column.Key || '').trim() || undefined,
		default: column.default ?? column.Default,
		extra: String(column.extra || column.Extra || '').trim() || undefined,
	};
}

function normalizeIndex(index = {}) {
	if (!isPlainObject(index)) {
		throw new TypeError('DB inventory indexes must be objects');
	}
	const name = String(index.name || index.Key_name || '').trim();
	if (!name) {
		throw new TypeError('DB inventory indexes require a name');
	}
	return {
		name,
		column: String(index.column || index.Column_name || '').trim() || undefined,
		unique: Boolean(index.unique ?? numericValue(index.Non_unique, 1) === 0),
		sequence: numericValue(index.sequence ?? index.Seq_in_index, 0),
	};
}

function normalizeTable(table = {}) {
	if (!isPlainObject(table)) {
		throw new TypeError('DB inventory tables must be objects');
	}
	const name = String(table.name || table.Name || '').trim();
	if (!name) {
		throw new TypeError('DB inventory tables require a name');
	}
	const columns = (Array.isArray(table.columns) ? table.columns : []).map(normalizeColumn).sort((a, b) => sortText(a.name, b.name));
	const indexes = (Array.isArray(table.indexes) ? table.indexes : []).map(normalizeIndex).sort((a, b) => sortText(a.name, b.name) || a.sequence - b.sequence);
	const rowCount = numericValue(table.rowCount ?? table.rows ?? table.Rows, 0);
	const dataBytes = numericValue(table.dataBytes ?? table.Data_length, 0);
	const indexBytes = numericValue(table.indexBytes ?? table.Index_length, 0);
	return {
		name,
		engine: String(table.engine || table.Engine || '').trim() || undefined,
		rowCount,
		dataBytes,
		indexBytes,
		totalBytes: numericValue(table.totalBytes, dataBytes + indexBytes),
		columns,
		indexes,
	};
}

function buildWordPressDbInventoryArtifact(input = {}) {
	const tables = (Array.isArray(input.tables) ? input.tables : []).map(normalizeTable).sort((a, b) => sortText(a.name, b.name));
	return {
		schema: 'homeboy/wordpress-db-inventory/v1',
		type: 'wordpress-db-inventory',
		totals: {
			tableCount: tables.length,
			rowCount: tables.reduce((sum, table) => sum + table.rowCount, 0),
			columnCount: tables.reduce((sum, table) => sum + table.columns.length, 0),
			indexCount: tables.reduce((sum, table) => sum + table.indexes.length, 0),
			dataBytes: tables.reduce((sum, table) => sum + table.dataBytes, 0),
			indexBytes: tables.reduce((sum, table) => sum + table.indexBytes, 0),
			totalBytes: tables.reduce((sum, table) => sum + table.totalBytes, 0),
		},
		tables,
	};
}

function escapeMarkdownCell(value) {
	return String(value ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ');
}

function formatWordPressDbInventoryMarkdownReport(input = {}, options = {}) {
	const artifact = input?.schema === 'homeboy/wordpress-db-inventory/v1'
		? input
		: buildWordPressDbInventoryArtifact(input);
	const limit = Math.max(0, Math.floor(numericValue(options.limit ?? 25)));
	const tables = limit > 0 ? artifact.tables.slice(0, limit) : artifact.tables;
	const lines = [
		`## ${options.title || 'WordPress DB inventory'}`,
		'',
		`Tables: ${artifact.totals.tableCount}; rows: ${artifact.totals.rowCount}; columns: ${artifact.totals.columnCount}; indexes: ${artifact.totals.indexCount}; bytes: ${artifact.totals.totalBytes}`,
		'',
		'| Table | Rows | Columns | Indexes | Bytes |',
		'| --- | ---: | ---: | ---: | ---: |',
	];
	for (const table of tables) {
		lines.push(`| ${escapeMarkdownCell(table.name)} | ${table.rowCount} | ${table.columns.length} | ${table.indexes.length} | ${table.totalBytes} |`);
	}
	return lines.join('\n');
}

module.exports = {
	buildWordPressDbInventoryArtifact,
	formatWordPressDbInventoryMarkdownReport,
	normalizeWordPressDbInventoryTable: normalizeTable,
};
