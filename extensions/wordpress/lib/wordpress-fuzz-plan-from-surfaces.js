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
const {
	WORDPRESS_SURFACE_COLLECTION_KEYS,
	wordpressSurfaceTypeFromCollectionKey,
} = require('./wordpress-surface-types');

const SAFE_REST_METHODS = new Set(['GET', 'HEAD', 'OPTIONS']);

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
	for (const key of WORDPRESS_SURFACE_COLLECTION_KEYS) {
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
	return wordpressSurfaceTypeFromCollectionKey(key);
}

function targetFromSurface(surface, options = {}) {
	if (surface.type === 'rest-route') {
		return restRouteTargetFromSurface(surface, options);
	}
	return genericTargetFromSurface(surface, options);
}

function restRouteTargetFromSurface(surface, options = {}) {
	const methods = restMethodsForSurface(surface);
	if (methods.length === 0) {
		return genericTargetFromSurface(surface, options);
	}

	const operationId = surface.operation_id || surface.operationId || `${surface.id}:request-rest-route`;
	const surfaceSkipReasons = reasonList(surface.skip_reasons || surface.skipReasons || surface.skip_reason || surface.skipReason);
	const surfaceDestructiveReasons = reasonList(surface.destructive_reasons || surface.destructiveReasons || surface.destructive_reason || surface.destructiveReason || surface.unsafeReasons);
	return {
		id: surface.id,
		surface_id: surface.id,
		type: surface.type,
		operation_id: operationId,
		cases: methods.map((method) => restRouteCaseFromMethod(surface, method, operationId, surfaceSkipReasons, surfaceDestructiveReasons, options)),
		metadata: {
			label: surface.label,
			type: surface.type,
			skip_reasons: surfaceSkipReasons,
			destructive_reasons: surfaceDestructiveReasons,
			methods,
			...(surface.metadata || {}),
		},
	};
}

function genericTargetFromSurface(surface, options = {}) {
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

function restRouteCaseFromMethod(surface, method, operationId, surfaceSkipReasons, surfaceDestructiveReasons, options = {}) {
	const safeMethod = SAFE_REST_METHODS.has(method);
	const operation = { ...operationForSurface(surface), method };
	const skipReasons = safeMethod ? surfaceSkipReasons : reasonList([...surfaceSkipReasons, 'mutating_rest_method_requires_explicit_opt_in']);
	const destructiveReasons = safeMethod ? surfaceDestructiveReasons : reasonList([...surfaceDestructiveReasons, 'rest_method_mutates_state']);
	return {
		id: `${surface.id}-${method.toLowerCase()}-generic-fuzz`,
		intent: 'request-rest-route',
		operation_id: operationId,
		operation,
		seed: options.seed,
		skip_reasons: skipReasons,
		destructive_reasons: destructiveReasons,
		metadata: {
			surface,
			rest_method: method,
			auth: surface.auth || surface.authentication || surface.authorization || null,
			safety: safeMethod ? { level: 'safe', mutates: false } : { level: 'mutating', mutates: true, requires_explicit_opt_in: true },
			planned: !safeMethod,
			gated: !safeMethod,
		},
	};
}

function restMethodsForSurface(surface) {
	const rawMethods = surface.methods === undefined ? surface.method : surface.methods;
	return [...new Set((Array.isArray(rawMethods) ? rawMethods : String(rawMethods || '').split(','))
		.map((method) => String(method).trim().toUpperCase())
		.filter(Boolean))].sort();
}

function operationForSurface(surface) {
	const operation = { id: surface.operation_id || surface.operationId, surface_type: surface.type };
	for (const key of ['id', 'name', 'hook', 'action', 'event', 'option', 'post_type', 'taxonomy', 'block_name', 'path', 'route', 'method', 'url', 'role', 'capability', 'table', 'query', 'request', 'endpoint']) {
		if (surface[key] !== undefined) {
			operation[key] = surface[key];
		}
	}
	return stripUndefined(operation);
}

function caseIntent(type) {
	return {
		'admin-page': 'request-admin-page',
		'ajax-action': 'exercise-ajax-action',
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
