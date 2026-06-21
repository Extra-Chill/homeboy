'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');

function sortText(a, b) {
	return String(a || '').localeCompare(String(b || ''));
}

function normalizeStringArray(value) {
	return [...new Set((Array.isArray(value) ? value : [])
		.map((item) => String(item || '').trim())
		.filter(Boolean))].sort(sortText);
}

function optionalNumber(value) {
	const number = Number(value);
	return Number.isFinite(number) ? number : undefined;
}

function normalizeBlockType(block = {}) {
	if (!isPlainObject(block)) {
		throw new TypeError('Block discovery entries must be objects');
	}
	const name = String(block.name || block.id || '').trim();
	if (!name) {
		throw new TypeError('Block discovery entries require a name');
	}
	const providesContext = isPlainObject(block.providesContext)
		? block.providesContext
		: block.provides_context;
	return {
		name,
		title: String(block.title || '').trim() || undefined,
		category: String(block.category || '').trim() || undefined,
		icon: typeof block.icon === 'string' ? block.icon : undefined,
		parent: normalizeStringArray(block.parent),
		ancestor: normalizeStringArray(block.ancestor),
		keywords: normalizeStringArray(block.keywords),
		supports: isPlainObject(block.supports) ? block.supports : {},
		attributes: Object.keys(isPlainObject(block.attributes) ? block.attributes : {}).sort(sortText),
		providesContext: Object.keys(isPlainObject(providesContext) ? providesContext : {}).sort(sortText),
		usesContext: normalizeStringArray(block.usesContext || block.uses_context),
		apiVersion: optionalNumber(block.apiVersion ?? block.api_version),
		source: String(block.source || '').trim() || undefined,
	};
}

function firstArray(...values) {
	return values.find(Array.isArray) || [];
}

function normalizeShortcode(shortcode = {}) {
	if (typeof shortcode === 'string') {
		return normalizeShortcode({ tag: shortcode });
	}
	if (!isPlainObject(shortcode)) {
		throw new TypeError('Shortcode discovery entries must be objects');
	}
	const tag = String(shortcode.tag || shortcode.name || shortcode.id || '').trim();
	if (!tag) {
		throw new TypeError('Shortcode discovery entries require a tag');
	}
	return {
		tag,
		callback: String(shortcode.callback || shortcode.handler || '').trim() || undefined,
		source: String(shortcode.source || '').trim() || undefined,
	};
}

function normalizeOptionSetting(setting = {}) {
	if (typeof setting === 'string') {
		return normalizeOptionSetting({ name: setting });
	}
	if (!isPlainObject(setting)) {
		throw new TypeError('Option/settings discovery entries must be objects');
	}
	const name = String(setting.name || setting.option_name || setting.optionName || setting.id || '').trim();
	if (!name) {
		throw new TypeError('Option/settings discovery entries require a name');
	}
	return {
		name,
		surface: String(setting.surface || setting.kind || 'option').trim() || 'option',
		group: String(setting.group || setting.option_group || setting.optionGroup || '').trim() || undefined,
		default: setting.default ?? setting.default_value ?? setting.defaultValue,
		valueType: String(setting.valueType || setting.value_type || setting.type || '').trim() || undefined,
		description: String(setting.description || '').trim() || undefined,
		restVisible: setting.restVisible ?? setting.show_in_rest ?? setting.showInRest,
		autoload: setting.autoload === undefined ? undefined : String(setting.autoload),
		source: String(setting.source || '').trim() || undefined,
	};
}

function buildWordPressDiscoveryInventoryArtifact(input = {}) {
	const blocks = firstArray(input.blocks, input.blockTypes)
		.map(normalizeBlockType)
		.sort((a, b) => sortText(a.name, b.name));
	const shortcodes = firstArray(input.shortcodes)
		.map(normalizeShortcode)
		.sort((a, b) => sortText(a.tag, b.tag));
	const optionSettings = firstArray(input.optionSettings, input.options, input.settings)
		.map(normalizeOptionSetting)
		.sort((a, b) => sortText(a.name, b.name) || sortText(a.surface, b.surface));
	return {
		schema: 'homeboy/wordpress-discovery-inventory/v1',
		type: 'wordpress-discovery-inventory',
		totals: {
			blockCount: blocks.length,
			shortcodeCount: shortcodes.length,
			optionSettingCount: optionSettings.length,
		},
		blocks,
		shortcodes,
		optionSettings,
	};
}

function escapeMarkdownCell(value) {
	return String(value ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ');
}

function formatWordPressDiscoveryInventoryMarkdownReport(input = {}, options = {}) {
	const artifact = input?.schema === 'homeboy/wordpress-discovery-inventory/v1'
		? input
		: buildWordPressDiscoveryInventoryArtifact(input);
	const limit = Math.max(0, Math.floor(Number(options.limit ?? 25)) || 0);
	const lines = [
		`## ${options.title || 'WordPress discovery inventory'}`,
		'',
		`Blocks: ${artifact.totals.blockCount}; shortcodes: ${artifact.totals.shortcodeCount}; options/settings: ${artifact.totals.optionSettingCount}`,
	];
	appendSection(lines, 'Blocks', ['Name', 'Title', 'Category'], artifact.blocks, limit, (block) => [block.name, block.title, block.category]);
	appendSection(lines, 'Shortcodes', ['Tag', 'Callback', 'Source'], artifact.shortcodes, limit, (shortcode) => [shortcode.tag, shortcode.callback, shortcode.source]);
	appendSection(lines, 'Options/settings', ['Name', 'Surface', 'Group'], artifact.optionSettings, limit, (setting) => [setting.name, setting.surface, setting.group]);
	return lines.join('\n');
}

function appendSection(lines, title, headers, rows, limit, mapRow) {
	const visibleRows = limit > 0 ? rows.slice(0, limit) : rows;
	lines.push('', `### ${title}`, '', `| ${headers.join(' | ')} |`, `| ${headers.map(() => '---').join(' | ')} |`);
	for (const row of visibleRows) {
		lines.push(`| ${mapRow(row).map(escapeMarkdownCell).join(' | ')} |`);
	}
}

module.exports = {
	buildWordPressDiscoveryInventoryArtifact,
	formatWordPressDiscoveryInventoryMarkdownReport,
	normalizeWordPressDiscoveryBlockType: normalizeBlockType,
	normalizeWordPressDiscoveryOptionSetting: normalizeOptionSetting,
	normalizeWordPressDiscoveryShortcode: normalizeShortcode,
};
