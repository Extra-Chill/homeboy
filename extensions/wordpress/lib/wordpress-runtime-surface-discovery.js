'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');
const {
	WORDPRESS_RUNTIME_SURFACE_ID_PREFIXES,
	normalizeWordPressCoverageSurfaceType,
	normalizeWordPressRuntimeSurfaceType,
	normalizeWordPressSurfaceType,
} = require('./wordpress-surface-types');

const WORDPRESS_RUNTIME_SURFACE_DISCOVERY_SCHEMA = 'homeboy/wordpress-surface-discovery/v1';
const WORDPRESS_RUNTIME_SURFACE_COVERAGE_MANIFEST_SCHEMA = 'homeboy/wordpress-fuzz-coverage-manifest/v1';
const WORDPRESS_UNSUPPORTED_RUNTIME_DISCOVERY_TYPES = new Set([
	'capability',
	'cron_event',
	'crud_resource',
	'db_query',
	'external_http',
	'hook',
	'media',
	'option',
	'post_type',
	'role',
	'setting',
	'taxonomy',
	'user',
	'wp_cli_command',
]);

function normalizeWordPressRuntimeSurfaceDiscovery(input = {}, options = {}) {
	const inputs = collectRuntimeSurfaceInputs(input, options);
	const surfaces = inputs.map((surface, index) => normalizeRuntimeSurface(surface, index)).filter(Boolean);
	const unsupportedSurfaces = inputs.map((surface, index) => normalizeUnsupportedSurface(surface, index)).filter(Boolean);

	return {
		schema: WORDPRESS_RUNTIME_SURFACE_DISCOVERY_SCHEMA,
		type: 'wordpress-surface-discovery',
		id: stringValue(input.id || options.id, 'wordpress-surface-discovery'),
		generated_at: input.generated_at || input.generatedAt || options.generated_at || options.generatedAt || null,
		source: stringValue(input.source || options.source, 'wordpress-runtime'),
		surfaces: dedupeRuntimeSurfaces(surfaces),
		unsupported_surfaces: dedupeUnsupportedSurfaces(unsupportedSurfaces),
		diagnostics: unsupportedSurfaces.map((surface) => ({
			severity: 'info',
			code: 'wordpress_surface_discovered_without_executable_runtime_collector',
			message: `${surface.type} surface \`${surface.id}\` was discovered for diagnostics but is not counted as executable runtime coverage.`,
			surface,
		})),
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
	appendArraySurfaces(surfaces, artifact.dbQueries || artifact.db_queries || artifact.queries || artifact.querySamples || artifact.query_samples, 'db_query', 'db-query');
	appendArraySurfaces(surfaces, artifact.frontend || artifact.frontendUrls || artifact.frontend_urls || artifact.urls, 'frontend_url', 'frontend');
	appendArraySurfaces(surfaces, artifact.blocks || artifact.blockTypes || artifact.block_types, 'block', 'blocks');
	appendArraySurfaces(surfaces, artifact.hooks || artifact.hookCallbacks || artifact.hook_callbacks, 'hook', 'hooks');
	appendArraySurfaces(surfaces, artifact.cron || artifact.cronEvents || artifact.cron_events, 'cron_event', 'cron');
	appendArraySurfaces(surfaces, artifact.options, 'option', 'options');
	appendArraySurfaces(surfaces, artifact.settings, 'setting', 'settings');
	appendArraySurfaces(surfaces, artifact.roles, 'role', 'roles');
	appendArraySurfaces(surfaces, artifact.capabilities || artifact.caps, 'capability', 'capabilities');
	appendArraySurfaces(surfaces, artifact.users, 'user', 'users');
	appendArraySurfaces(surfaces, artifact.media || artifact.attachments, 'media', 'media');
	appendArraySurfaces(surfaces, artifact.postTypes || artifact.post_types, 'post_type', 'post-types');
	appendArraySurfaces(surfaces, artifact.taxonomies, 'taxonomy', 'taxonomies');
	appendArraySurfaces(surfaces, artifact.wpCli || artifact.wp_cli || artifact.wpCliCommands || artifact.wp_cli_commands || artifact.commands, 'wp_cli_command', 'wp-cli');
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
			const type = source === 'settings' && defaultType === 'setting' ? defaultType : undefined;
			surfaces.push(isPlainObject(item) ? { ...item, type: type || item.type || item.kind || defaultType, source: item.source || source } : { label: item, value: item, type: defaultType, source });
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

function normalizeUnsupportedSurface(surface, index) {
	if (!isPlainObject(surface)) {
		return null;
	}
	const runtimeType = normalizeRuntimeSurfaceType(surface.type || surface.kind || surface.category);
	if (runtimeType) {
		return null;
	}
	const coverageType = normalizeWordPressCoverageSurfaceType(surface.type || surface.kind || surface.category);
	const canonicalType = normalizeWordPressSurfaceType(surface.type || surface.kind || surface.category) || coverageType.replace(/_/g, '-');
	if (!coverageType || !WORDPRESS_UNSUPPORTED_RUNTIME_DISCOVERY_TYPES.has(coverageType)) {
		return null;
	}
	const value = runtimeSurfaceValue(surface, coverageType, index);
	return {
		id: runtimeSurfaceId(surface, coverageType, value),
		type: coverageType,
		canonical_type: canonicalType,
		label: stringValue(surface.label || surface.title || surface.name || surface.path || surface.url || surface.route || surface.action || surface.hook || surface.option || surface.setting || surface.role || surface.capability || surface.table || surface.value, value),
		executable: false,
		coverage_counted: false,
		metadata: {
			...(isPlainObject(surface.metadata) ? surface.metadata : {}),
			source: stringValue(surface.source || surface.artifact, 'input'),
			value,
		},
	};
}

function normalizeRuntimeSurfaceType(value) {
	return normalizeWordPressRuntimeSurfaceType(value);
}

function runtimeSurfaceValue(surface, type, index) {
	return stringValue(
		surface.coverage_id
		|| surface.coverageId
		|| surface.route
		|| surface.path
		|| surface.url
		|| surface.action
		|| surface.hook
		|| surface.event
		|| surface.option
		|| surface.setting
		|| surface.role
		|| surface.capability
		|| surface.user
		|| surface.media
		|| surface.query
		|| surface.table
		|| surface.command
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
	const prefix = WORDPRESS_RUNTIME_SURFACE_ID_PREFIXES[type];
	const explicitCoverageId = stringValue(surface.coverage_id || surface.coverageId);
	if (explicitCoverageId) {
		return explicitCoverageId.includes(':') ? explicitCoverageId : `${prefix}:${explicitCoverageId}`;
	}
	const explicitId = stringValue(surface.id);
	if (explicitId.startsWith(`${prefix}:`)) {
		return explicitId;
	}
	return `${prefix || type}:${value}`;
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

function dedupeUnsupportedSurfaces(surfaces) {
	const byId = new Map();
	for (const surface of surfaces) {
		const existing = byId.get(surface.id);
		if (!existing) {
			byId.set(surface.id, surface);
			continue;
		}
		byId.set(surface.id, {
			...existing,
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
