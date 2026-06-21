'use strict';

/**
 * Internal dependencies
 */
const {
	WORDPRESS_FUZZ_PLAN_SCHEMA,
	WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
	normalizeWordPressFuzzPlan,
	normalizeWordPressSurfaceDiscovery,
} = require('./wordpress-fuzz-schemas');

const SURFACE_COLLECTION_KEYS = [
	'surfaces',
	'hooks',
	'cron',
	'cron_events',
	'cronEvents',
	'capabilities',
	'database',
	'db',
	'databaseTables',
	'database_tables',
	'dbQueries',
	'db_queries',
	'options',
	'post_types',
	'postTypes',
	'taxonomies',
	'media',
	'users',
	'roles',
	'blocks',
	'frontend',
	'frontendUrls',
	'frontend_urls',
	'admin',
	'adminPages',
	'admin_pages',
	'externalHttp',
	'external_http',
	'http',
	'httpRequests',
	'http_requests',
	'rest',
	'restRoutes',
	'routes',
];

function buildWordPressFuzzPlanFromSurfaces(input = {}, options = {}) {
	const discovery = normalizeWordPressSurfaceDiscovery({
		schema: WORDPRESS_SURFACE_DISCOVERY_SCHEMA,
		id: input.id || input.discovery_id || input.discoveryId || options.discoveryId || 'wordpress-surface-discovery',
		label: input.label || options.label || 'WordPress surface discovery',
		surfaces: collectWordPressFuzzPlanSurfaces(input),
	});

	return normalizeWordPressFuzzPlan({
		schema: WORDPRESS_FUZZ_PLAN_SCHEMA,
		id: options.id || input.plan_id || input.planId || `${discovery.id}-fuzz-plan`,
		discovery_id: discovery.id,
		targets: discovery.surfaces.map((surface) => targetFromSurface(surface, options)),
		budget: options.budget || input.budget || {},
		metadata: {
			...(input.metadata || {}),
			planner: 'homeboy/wordpress-fuzz-plan-from-surfaces/v1',
		},
	});
}

function collectWordPressFuzzPlanSurfaces(input = {}) {
	if (Array.isArray(input)) {
		return input.flatMap((surface) => collectWordPressFuzzPlanSurfaces(surface));
	}
	if (!isObject(input)) {
		return [];
	}
	if (Array.isArray(input.surfaces)) {
		return input.surfaces;
	}

	const surfaces = [];
	for (const key of SURFACE_COLLECTION_KEYS) {
		if (key === 'surfaces') {
			appendSurfaceMap(surfaces, input.surfaces, undefined);
			continue;
		}
		appendSurfaceMap(surfaces, input[key], surfaceTypeFromCollectionKey(key));
	}
	return surfaces;
}

function appendSurfaceMap(surfaces, value, defaultType) {
	if (Array.isArray(value)) {
		for (const item of value) {
			surfaces.push(surfaceFromValue(item, defaultType));
		}
		return;
	}
	if (!isObject(value)) {
		return;
	}
	if (defaultType === 'hook') {
		appendSurfaceMap(surfaces, value.actions, 'hook');
		appendSurfaceMap(surfaces, value.filters, 'hook');
	}
	if (defaultType === 'rest-route') {
		appendSurfaceMap(surfaces, value.routes, 'rest-route');
	}
	if (defaultType === 'database-table') {
		appendSurfaceMap(surfaces, value.tables, 'database-table');
		appendSurfaceMap(surfaces, value.queries, 'db-query');
	}
	if (defaultType === 'external-http') {
		appendSurfaceMap(surfaces, value.requests, 'external-http');
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
		surfaces.push(surfaceFromValue(isObject(item) ? { id: key, name: key, ...item } : { id: key, name: key, value: item }, defaultType));
	}
}

function surfaceFromValue(value, defaultType) {
	if (typeof value === 'string') {
		return { id: value, name: value, type: defaultType };
	}
	return { ...value, type: value.type || value.kind || defaultType };
}

function surfaceTypeFromCollectionKey(key) {
	return {
		hooks: 'hook',
		cron: 'cron-event',
		cron_events: 'cron-event',
		cronEvents: 'cron-event',
		capabilities: 'capability',
		database: 'database-table',
		db: 'database-table',
		databaseTables: 'database-table',
		database_tables: 'database-table',
		dbQueries: 'db-query',
		db_queries: 'db-query',
		options: 'option',
		post_types: 'post-type',
		postTypes: 'post-type',
		taxonomies: 'taxonomy',
		media: 'media',
		users: 'user',
		roles: 'role',
		blocks: 'block',
		frontend: 'frontend-url',
		frontendUrls: 'frontend-url',
		frontend_urls: 'frontend-url',
		admin: 'admin-page',
		adminPages: 'admin-page',
		admin_pages: 'admin-page',
		externalHttp: 'external-http',
		external_http: 'external-http',
		http: 'external-http',
		httpRequests: 'external-http',
		http_requests: 'external-http',
		rest: 'rest-route',
		restRoutes: 'rest-route',
		routes: 'rest-route',
	}[key];
}

function targetFromSurface(surface, options = {}) {
	const operation = operationForSurface(surface);
	const operationId = surface.operation_id || surface.operationId || `${surface.id}:${caseIntent(surface.type)}`;
	const caseId = `${surface.id}-generic-fuzz`;
	return {
		id: surface.id,
		surface_id: surface.id,
		type: surface.type,
		operation_id: operationId,
		cases: [{
			id: caseId,
			intent: caseIntent(surface.type),
			operation_id: operationId,
			operation,
			seed: options.seed,
			skip_reasons: reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
			destructive_reasons: reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons),
			metadata: { surface },
		}],
		metadata: {
			label: surface.label,
			type: surface.type,
			skip_reasons: reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason),
			destructive_reasons: reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons),
			...(surface.metadata || {}),
		},
	};
}

function operationForSurface(surface) {
	const operation = { id: surface.operation_id || surface.operationId, surface_type: surface.type };
	for (const key of ['id', 'name', 'hook', 'event', 'option', 'post_type', 'taxonomy', 'block_name', 'path', 'route', 'method', 'url', 'role', 'capability', 'table', 'query', 'request', 'endpoint']) {
		if (surface[key] !== undefined) {
			operation[key] = surface[key];
		}
	}
	return stripUndefined(operation);
}

function caseIntent(type) {
	return {
		'admin-page': 'request-admin-page',
		block: 'render-block',
		'cron-event': 'inspect-cron-event',
		capability: 'check-capability-boundary',
		'database-table': 'inspect-database-table',
		'db-query': 'profile-database-query',
		'external-http': 'exercise-external-http-guardrail',
		'frontend-url': 'request-frontend-url',
		hook: 'exercise-hook',
		media: 'query-media',
		option: 'read-option',
		'post-type': 'query-post-type',
		'rest-route': 'request-rest-route',
		role: 'check-role-boundary',
		taxonomy: 'query-taxonomy',
		user: 'query-user',
	}[type] || 'exercise-wordpress-surface';
}

function reasonList(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return [...new Set((Array.isArray(value) ? value : [value]).map(String).filter(Boolean))].sort();
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function isObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
	buildWordPressFuzzPlanFromSurfaces,
	collectWordPressFuzzPlanSurfaces,
};
