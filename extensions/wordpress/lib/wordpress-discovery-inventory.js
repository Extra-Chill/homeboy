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

function normalizeToken(value) {
	return String(value || '')
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '') || 'surface';
}

function normalizeSurfaceType(value) {
	const type = String(value || '').trim();
	return {
		action: 'hook',
		admin: 'admin-page',
		admin_page: 'admin-page',
		cron: 'cron-event',
		database: 'database-table',
		db: 'database-table',
		db_query: 'db-query',
		database_query: 'db-query',
		external_http: 'external-http',
		http: 'external-http',
		filter: 'hook',
		frontend: 'frontend-url',
		frontend_url: 'frontend-url',
		option_setting: 'option',
		post_type: 'post-type',
		rest: 'rest-route',
		rest_route: 'rest-route',
	}[type] || type;
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

function normalizeSurface(surface = {}, defaultType) {
	if (typeof surface === 'string') {
		return normalizeSurface({ name: surface, id: surface }, defaultType);
	}
	if (!isPlainObject(surface)) {
		throw new TypeError('Surface discovery entries must be objects');
	}
	const type = normalizeSurfaceType(surface.type || surface.kind || defaultType);
	if (!type) {
		throw new TypeError('Surface discovery entries require a type');
	}
	const identity = surface.id || surface.name || surface.hook || surface.option || surface.table || surface.query || surface.route || surface.path || surface.url || surface.endpoint;
	const id = String(identity || '').trim() || `${type}:${normalizeToken(surface.label || type)}`;
	return {
		...surface,
		id,
		type,
		label: String(surface.label || surface.title || surface.name || identity || id).trim() || id,
		metadata: isPlainObject(surface.metadata) ? surface.metadata : {},
	};
}

function appendSurfaceCollection(surfaces, value, defaultType) {
	if (Array.isArray(value)) {
		for (const item of value) {
			surfaces.push(normalizeSurface(item, defaultType));
		}
		return;
	}
	if (!isPlainObject(value)) {
		return;
	}
	if (defaultType === 'hook') {
		appendSurfaceCollection(surfaces, value.actions, 'hook');
		appendSurfaceCollection(surfaces, value.filters, 'hook');
	}
	if (defaultType === 'rest-route') {
		appendSurfaceCollection(surfaces, value.routes, 'rest-route');
	}
	if (defaultType === 'database-table') {
		appendSurfaceCollection(surfaces, value.tables, 'database-table');
		appendSurfaceCollection(surfaces, value.queries, 'db-query');
	}
	if (defaultType === 'external-http') {
		appendSurfaceCollection(surfaces, value.requests, 'external-http');
	}
	for (const [key, item] of Object.entries(value)) {
		if (
			(defaultType === 'hook' && ['actions', 'filters'].includes(key))
			|| (defaultType === 'rest-route' && key === 'routes')
			|| (defaultType === 'database-table' && ['tables', 'queries'].includes(key))
			|| (defaultType === 'external-http' && key === 'requests')
		) {
			continue;
		}
		surfaces.push(normalizeSurface(isPlainObject(item) ? { id: key, name: key, ...item } : { id: key, name: key, value: item }, defaultType));
	}
}

function buildGenericSurfaces(input, blocks, optionSettings) {
	const surfaces = [];
	appendSurfaceCollection(surfaces, input.surfaces, undefined);
	appendSurfaceCollection(surfaces, input.rest || input.routes || input.restRoutes || input.rest_routes, 'rest-route');
	appendSurfaceCollection(surfaces, input.admin || input.adminPages || input.admin_pages, 'admin-page');
	appendSurfaceCollection(surfaces, input.frontend || input.frontendUrls || input.frontend_urls, 'frontend-url');
	appendSurfaceCollection(surfaces, input.hooks || input.hookSurfaces || input.hook_surfaces, 'hook');
	appendSurfaceCollection(surfaces, input.database || input.db || input.databaseTables || input.database_tables, 'database-table');
	appendSurfaceCollection(surfaces, input.dbQueries || input.db_queries, 'db-query');
	appendSurfaceCollection(surfaces, input.externalHttp || input.external_http || input.httpRequests || input.http_requests, 'external-http');

	for (const block of blocks) {
		surfaces.push(normalizeSurface({ id: `block:${block.name}`, name: block.name, block_name: block.name, label: block.title || block.name, metadata: block }, 'block'));
	}
	for (const setting of optionSettings) {
		surfaces.push(normalizeSurface({ id: `option:${setting.name}`, name: setting.name, option: setting.name, metadata: setting }, 'option'));
	}

	return surfaces.sort((a, b) => sortText(a.type, b.type) || sortText(a.id, b.id));
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
	const surfaces = buildGenericSurfaces(input, blocks, optionSettings);
	const surfaceCounts = surfaces.reduce((counts, surface) => {
		counts[surface.type] = (counts[surface.type] || 0) + 1;
		return counts;
	}, {});
	return {
		schema: 'homeboy/wordpress-discovery-inventory/v1',
		type: 'wordpress-discovery-inventory',
		totals: {
			blockCount: blocks.length,
			shortcodeCount: shortcodes.length,
			optionSettingCount: optionSettings.length,
			surfaceCount: surfaces.length,
		},
		surfaceCounts,
		surfaces,
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
		`Blocks: ${artifact.totals.blockCount}; shortcodes: ${artifact.totals.shortcodeCount}; options/settings: ${artifact.totals.optionSettingCount}; surfaces: ${artifact.totals.surfaceCount || 0}`,
	];
	appendSection(lines, 'Generic surfaces', ['Type', 'ID', 'Label'], artifact.surfaces || [], limit, (surface) => [surface.type, surface.id, surface.label]);
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
	normalizeWordPressDiscoverySurface: normalizeSurface,
};
