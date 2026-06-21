'use strict';

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
	'options',
	'post_types',
	'postTypes',
	'taxonomies',
	'media',
	'users',
	'blocks',
	'frontend',
	'frontendUrls',
	'admin',
	'adminPages',
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
	for (const [key, item] of Object.entries(value)) {
		if ((defaultType === 'hook' && ['actions', 'filters'].includes(key)) || (defaultType === 'rest-route' && key === 'routes')) {
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
		options: 'option',
		post_types: 'post-type',
		postTypes: 'post-type',
		taxonomies: 'taxonomy',
		media: 'media',
		users: 'user',
		blocks: 'block',
		frontend: 'frontend-url',
		frontendUrls: 'frontend-url',
		admin: 'admin-page',
		adminPages: 'admin-page',
		rest: 'rest-route',
		restRoutes: 'rest-route',
		routes: 'rest-route',
	}[key];
}

function targetFromSurface(surface, options = {}) {
	const caseId = `${surface.id}-generic-fuzz`;
	return {
		id: surface.id,
		surface_id: surface.id,
		type: surface.type,
		cases: [{
			id: caseId,
			intent: caseIntent(surface.type),
			operation: operationForSurface(surface),
			seed: options.seed,
			metadata: { surface },
		}],
		metadata: {
			label: surface.label,
			type: surface.type,
			...(surface.metadata || {}),
		},
	};
}

function operationForSurface(surface) {
	const operation = { surface_type: surface.type };
	for (const key of ['id', 'name', 'hook', 'event', 'option', 'post_type', 'taxonomy', 'block_name', 'path', 'route', 'method', 'url']) {
		if (surface[key] !== undefined) {
			operation[key] = surface[key];
		}
	}
	return operation;
}

function caseIntent(type) {
	return {
		'admin-page': 'request-admin-page',
		block: 'render-block',
		'cron-event': 'inspect-cron-event',
		'frontend-url': 'request-frontend-url',
		hook: 'exercise-hook',
		media: 'query-media',
		option: 'read-option',
		'post-type': 'query-post-type',
		'rest-route': 'request-rest-route',
		taxonomy: 'query-taxonomy',
		user: 'query-user',
	}[type] || 'exercise-wordpress-surface';
}

function isObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
	buildWordPressFuzzPlanFromSurfaces,
	collectWordPressFuzzPlanSurfaces,
};
