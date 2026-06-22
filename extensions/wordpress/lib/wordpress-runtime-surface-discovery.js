'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');
const { normalizeWordPressFuzzSurfaceType } = require('./wordpress-fuzz-schemas');

const WORDPRESS_RUNTIME_SURFACE_DISCOVERY_SCHEMA = 'homeboy/wordpress-surface-discovery/v1';
const WORDPRESS_RUNTIME_SURFACE_COVERAGE_MANIFEST_SCHEMA = 'homeboy/wordpress-fuzz-coverage-manifest/v1';

const SURFACE_TYPE_ALIASES = new Map([
	['admin', 'admin_page'],
	['admin-page', 'admin_page'],
	['admin_page', 'admin_page'],
	['ajax', 'ajax_action'],
	['ajax-action', 'ajax_action'],
	['ajax_action', 'ajax_action'],
	['block', 'block'],
	['block-type', 'block'],
	['block_type', 'block'],
	['database', 'db_table'],
	['database-table', 'db_table'],
	['database_table', 'db_table'],
	['db', 'db_table'],
	['db-table', 'db_table'],
	['db_table', 'db_table'],
	['frontend', 'frontend_url'],
	['frontend-url', 'frontend_url'],
	['frontend_url', 'frontend_url'],
	['rest', 'rest_route'],
	['rest-route', 'rest_route'],
	['rest_route', 'rest_route'],
]);

const COVERAGE_ID_PREFIXES = Object.freeze({
	admin_page: 'admin',
	ajax_action: 'ajax',
	block: 'block',
	db_table: 'db',
	frontend_url: 'frontend',
	rest_route: 'rest',
});

const FUZZ_TO_RUNTIME_SURFACE_TYPES = new Map([
	['admin-page', 'admin_page'],
	['ajax-action', 'ajax_action'],
	['block', 'block'],
	['database-table', 'db_table'],
	['frontend-url', 'frontend_url'],
	['rest-route', 'rest_route'],
]);

function normalizeWordPressRuntimeSurfaceDiscovery(input = {}, options = {}) {
	const surfaces = collectRuntimeSurfaceInputs(input, options)
		.map((surface, index) => normalizeRuntimeSurface(surface, index))
		.filter(Boolean);

	return {
		schema: WORDPRESS_RUNTIME_SURFACE_DISCOVERY_SCHEMA,
		type: 'wordpress-surface-discovery',
		id: stringValue(input.id || options.id, 'wordpress-surface-discovery'),
		generated_at: input.generated_at || input.generatedAt || options.generated_at || options.generatedAt || null,
		source: stringValue(input.source || options.source, 'wordpress-runtime'),
		surfaces: dedupeRuntimeSurfaces(surfaces),
		metadata: isPlainObject(input.metadata) ? input.metadata : {},
	};
}

function buildWordPressRuntimeSurfaceCoverageManifest(input = {}, options = {}) {
	const discovery = input.schema === WORDPRESS_RUNTIME_SURFACE_DISCOVERY_SCHEMA
		? input
		: normalizeWordPressRuntimeSurfaceDiscovery(input, options);
	return {
		schema: WORDPRESS_RUNTIME_SURFACE_COVERAGE_MANIFEST_SCHEMA,
		type: 'wordpress-fuzz-coverage-manifest',
		surfaces: discovery.surfaces.map((surface) => ({
			id: surface.id,
			type: surface.type,
			label: surface.label,
			required: surface.required !== false,
			metadata: surface.metadata || {},
		})),
	};
}

function collectRuntimeSurfaceInputs(input, options = {}) {
	const artifacts = artifactInputs(input, options);
	const surfaces = [];
	for (const artifact of artifacts) {
		appendArtifactSurfaces(surfaces, artifact);
	}
	return surfaces;
}

function artifactInputs(input, options) {
	const artifacts = [];
	appendArtifactInput(artifacts, options.artifacts || options.artifact);
	appendArtifactInput(artifacts, input.artifacts || input.artifact);
	appendArtifactInput(artifacts, input.discovery || input.manifest);
	appendArtifactInput(artifacts, input);
	return artifacts;
}

function appendArtifactInput(artifacts, value) {
	if (Array.isArray(value)) {
		for (const item of value) {
			appendArtifactInput(artifacts, item);
		}
		return;
	}
	if (isPlainObject(value)) {
		artifacts.push(value);
	}
}

function appendArtifactSurfaces(surfaces, artifact) {
	if (!isPlainObject(artifact)) {
		return;
	}

	if (artifact.schema === 'homeboy/wordpress-rest-fuzz-surface-discovery/v1' && isPlainObject(artifact.artifact)) {
		appendArtifactSurfaces(surfaces, artifact.artifact);
		return;
	}

	if (artifact.schema === 'homeboy/wordpress-fuzz-surfaces/v1') {
		appendFuzzSurfaceArtifact(surfaces, artifact);
		return;
	}

	if (artifact.schema === 'homeboy/wordpress-admin-page-fuzz-surface-discovery/v1') {
		appendArraySurfaces(surfaces, artifact.surfaces, 'admin_page', 'admin-page-fuzz-surface-discovery');
		return;
	}

	if (artifact.schema === 'homeboy/wordpress-ajax-action-surface/v1' || artifact.schema === 'homeboy/wordpress-ajax-action-plan/v1') {
		appendArraySurfaces(surfaces, artifact.actions || artifact.plannedActions, 'ajax_action', 'ajax-action-surface');
		return;
	}

	if (artifact.schema === 'homeboy/wordpress-db-inventory/v1') {
		appendArraySurfaces(surfaces, artifact.tables, 'db_table', 'db-inventory');
		return;
	}

	if (artifact.schema === 'homeboy/wordpress-discovery-inventory/v1') {
		appendArraySurfaces(surfaces, artifact.surfaces, undefined, 'discovery-inventory');
		appendArraySurfaces(surfaces, artifact.blocks, 'block', 'discovery-inventory');
		return;
	}

	appendArraySurfaces(surfaces, artifact.expectedSurfaces || artifact.expected_surfaces || artifact.surfaces || artifact.targets, undefined, 'surface-list');
	appendArraySurfaces(surfaces, artifact.rest || artifact.routes || artifact.restRoutes || artifact.rest_routes, 'rest_route', 'rest');
	appendArraySurfaces(surfaces, artifact.admin || artifact.adminPages || artifact.admin_pages, 'admin_page', 'admin');
	appendArraySurfaces(surfaces, artifact.ajax || artifact.actions || artifact.ajaxActions || artifact.ajax_actions, 'ajax_action', 'ajax');
	appendArraySurfaces(surfaces, artifact.database || artifact.db || artifact.tables || artifact.databaseTables || artifact.database_tables, 'db_table', 'db');
	appendArraySurfaces(surfaces, artifact.frontend || artifact.frontendUrls || artifact.frontend_urls || artifact.urls, 'frontend_url', 'frontend');
	appendArraySurfaces(surfaces, artifact.blocks || artifact.blockTypes || artifact.block_types, 'block', 'blocks');
}

function appendFuzzSurfaceArtifact(surfaces, artifact) {
	for (const group of Array.isArray(artifact.surfaces) ? artifact.surfaces : []) {
		const type = normalizeRuntimeSurfaceType(group.kind || group.type);
		if (type === 'rest_route') {
			appendArraySurfaces(surfaces, group.routes, 'rest_route', artifact.source || 'wordpress-fuzz-surfaces');
			continue;
		}
		appendArraySurfaces(surfaces, group.surfaces || group.items || group.entries, type, artifact.source || 'wordpress-fuzz-surfaces');
	}
}

function appendArraySurfaces(surfaces, value, defaultType, source) {
	if (Array.isArray(value)) {
		for (const item of value) {
			surfaces.push(isPlainObject(item) ? { ...item, type: item.type || item.kind || defaultType, source: item.source || source } : { label: item, value: item, type: defaultType, source });
		}
		return;
	}
	if (!isPlainObject(value)) {
		return;
	}
	for (const [key, item] of Object.entries(value)) {
		surfaces.push(isPlainObject(item) ? { id: key, name: key, ...item, type: item.type || item.kind || defaultType, source: item.source || source } : { id: key, name: key, value: item, type: defaultType, source });
	}
}

function normalizeRuntimeSurface(surface, index) {
	if (!isPlainObject(surface)) {
		return null;
	}
	const type = normalizeRuntimeSurfaceType(surface.type || surface.kind || surface.category);
	if (!type) {
		return null;
	}
	const value = runtimeSurfaceValue(surface, type, index);
	const id = runtimeSurfaceId(surface, type, value);
	return {
		id,
		type,
		label: stringValue(surface.label || surface.title || surface.name || surface.path || surface.url || surface.route || surface.action || surface.table || surface.value, value),
		required: surface.required !== false,
		metadata: {
			...(isPlainObject(surface.metadata) ? surface.metadata : {}),
			source: stringValue(surface.source || surface.artifact, 'input'),
			value,
		},
	};
}

function normalizeRuntimeSurfaceType(value) {
	const key = String(value || '').trim().toLowerCase().replace(/\s+/g, '-');
	const alias = SURFACE_TYPE_ALIASES.get(key);
	if (alias) {
		return alias;
	}
	return FUZZ_TO_RUNTIME_SURFACE_TYPES.get(normalizeWordPressFuzzSurfaceType(value)) || '';
}

function runtimeSurfaceValue(surface, type, index) {
	return stringValue(
		surface.coverage_id
		|| surface.coverageId
		|| surface.route
		|| surface.path
		|| surface.url
		|| surface.action
		|| surface.table
		|| surface.name
		|| surface.block
		|| surface.block_name
		|| surface.blockName
		|| surface.id
		|| surface.value,
		`${type}-${index + 1}`
	);
}

function runtimeSurfaceId(surface, type, value) {
	const prefix = COVERAGE_ID_PREFIXES[type];
	const explicitCoverageId = stringValue(surface.coverage_id || surface.coverageId);
	if (explicitCoverageId) {
		return explicitCoverageId.includes(':') ? explicitCoverageId : `${prefix}:${explicitCoverageId}`;
	}
	const explicitId = stringValue(surface.id);
	if (explicitId.startsWith(`${prefix}:`)) {
		return explicitId;
	}
	return `${prefix}:${value}`;
}

function dedupeRuntimeSurfaces(surfaces) {
	const byId = new Map();
	for (const surface of surfaces) {
		const existing = byId.get(surface.id);
		if (!existing) {
			byId.set(surface.id, surface);
			continue;
		}
		byId.set(surface.id, {
			...existing,
			required: existing.required !== false || surface.required !== false,
			metadata: {
				...existing.metadata,
				...surface.metadata,
				sources: [...new Set([existing.metadata?.source, surface.metadata?.source, ...(existing.metadata?.sources || []), ...(surface.metadata?.sources || [])].filter(Boolean))].sort(),
			},
		});
	}
	return [...byId.values()].sort((a, b) => a.type.localeCompare(b.type) || a.id.localeCompare(b.id));
}

function stringValue(value, fallback = '') {
	const normalized = String(value ?? '').trim();
	return normalized || fallback;
}

module.exports = {
	WORDPRESS_RUNTIME_SURFACE_COVERAGE_MANIFEST_SCHEMA,
	WORDPRESS_RUNTIME_SURFACE_DISCOVERY_SCHEMA,
	buildWordPressRuntimeSurfaceCoverageManifest,
	collectRuntimeSurfaceInputs,
	normalizeWordPressRuntimeSurfaceDiscovery,
};
