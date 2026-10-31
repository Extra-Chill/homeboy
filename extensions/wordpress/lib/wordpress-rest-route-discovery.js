'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');
const {
	classifyRestRoute,
	normalizeRestRouteMethod,
	restRouteMatrixKey,
	summarizeRouteArgs,
	summarizeSchema,
} = require('./rest-route-matrix');

const WORDPRESS_REST_ROUTE_DISCOVERY_SCHEMA = 'homeboy/wordpress-rest-route-discovery/v1';
const DEFAULT_REST_BASE_PATH = 'wp-json/';

function normalizeRoutePath(routeKey) {
	const raw = String(routeKey || '').trim();
	if (!raw) {
		throw new TypeError('REST route discovery route keys must be non-empty strings');
	}
	return raw.startsWith('/') ? raw : `/${raw}`;
}

function routeDisplayPath(routeKey) {
	return normalizeRoutePath(routeKey)
		.replace(/\(\?P<([^>]+)>[^)]+\)/g, '{$1}')
		.replace(/\(\?:([^)]*)\)/g, '{$1}')
		.replace(/[?+*]/g, '')
		.replace(/\/+$/g, '') || '/';
}

function routeNamespace(routeKey, route = {}) {
	if (typeof route.namespace === 'string' && route.namespace.trim() !== '') {
		return route.namespace.trim();
	}
	const match = String(routeKey || '').match(/^\/?([^/]+\/v\d+(?:\.\d+)?)/i);
	return match ? match[1] : '';
}

function normalizeRoutesObject(input = {}) {
	if (isPlainObject(input?.routes)) {
		return input.routes;
	}
	if (isPlainObject(input)) {
		return input;
	}
	return {};
}

function normalizeMethods(value) {
	const methods = new Set();
	const add = (method) => methods.add(normalizeRestRouteMethod(method));
	if (Array.isArray(value)) {
		value.forEach(add);
	} else if (typeof value === 'string') {
		value.split(',').forEach(add);
	}
	return [...methods].filter(Boolean).sort();
}

function normalizeEndpointMethods(endpoint = {}, fallbackMethods = []) {
	const methods = normalizeMethods(endpoint.methods);
	return methods.length > 0 ? methods : fallbackMethods;
}

function endpointMatchesMethod(endpoint = {}, method) {
	return normalizeEndpointMethods(endpoint).includes(normalizeRestRouteMethod(method));
}

function mergeArgsForMethod(route = {}, method) {
	const args = {};
	for (const [name, arg] of Object.entries(route.args || {})) {
		args[name] = arg;
	}
	for (const endpoint of route.endpoints || []) {
		if (!endpointMatchesMethod(endpoint, method)) {
			continue;
		}
		for (const [name, arg] of Object.entries(endpoint.args || {})) {
			args[name] = arg;
		}
	}
	return args;
}

function normalizeAuthMetadata(endpoint = {}, route = {}) {
	const explicit = endpoint.auth || endpoint.authentication || endpoint.authorization || route.auth || route.authentication || route.authorization;
	if (isPlainObject(explicit)) {
		return {
			required: Boolean(explicit.required ?? explicit.requires_authentication ?? explicit.requiresAuthentication),
			source: explicit.source || 'metadata',
			capability: explicit.capability || explicit.capabilities,
		};
	}

	const permission = endpoint.permission_callback ?? endpoint.permissionCallback ?? route.permission_callback ?? route.permissionCallback;
	if (permission === '__return_true' || permission === true) {
		return { required: false, source: 'permission_callback' };
	}
	if (typeof permission === 'string' && permission.trim() !== '') {
		return { required: true, source: 'permission_callback', callback: permission };
	}
	if (endpoint.requires_authentication !== undefined || endpoint.requiresAuthentication !== undefined) {
		return { required: Boolean(endpoint.requires_authentication ?? endpoint.requiresAuthentication), source: 'endpoint' };
	}
	return { required: undefined, source: 'unknown' };
}

function routeSchemaForMethod(route = {}, method) {
	for (const endpoint of route.endpoints || []) {
		if (endpointMatchesMethod(endpoint, method) && isPlainObject(endpoint.schema)) {
			return endpoint.schema;
		}
	}
	if (isPlainObject(route.schema)) {
		return route.schema;
	}
	return undefined;
}

function mergeRouteMetadata(route = {}, optionsPayload = {}, schemaPayload = {}) {
	const merged = { ...route };
	const optionRoute = isPlainObject(optionsPayload?.route) ? optionsPayload.route : optionsPayload;
	if (isPlainObject(optionRoute)) {
		if (Array.isArray(optionRoute.endpoints)) {
			merged.endpoints = optionRoute.endpoints;
		}
		if (isPlainObject(optionRoute.args)) {
			merged.args = { ...(merged.args || {}), ...optionRoute.args };
		}
		if (isPlainObject(optionRoute.schema)) {
			merged.schema = optionRoute.schema;
		}
	}
	if (isPlainObject(schemaPayload?.schema)) {
		merged.schema = schemaPayload.schema;
	} else if (isPlainObject(schemaPayload)) {
		merged.schema = schemaPayload;
	}
	return merged;
}

function normalizeWordPressRestRouteDiscovery(input = {}, options = {}) {
	const restIndex = input.restIndex || input.rest_index || input.index || input;
	const optionsByRoute = input.optionsByRoute || input.options_by_route || options.optionsByRoute || options.options_by_route || {};
	const schemasByRoute = input.schemasByRoute || input.schemas_by_route || options.schemasByRoute || options.schemas_by_route || {};
	const routes = normalizeRoutesObject(restIndex);
	const discovered = [];

	for (const routeKey of Object.keys(routes).sort()) {
		const route = normalizeRoutePath(routeKey);
		const mergedRoute = mergeRouteMetadata(routes[routeKey] || {}, optionsByRoute[routeKey] || optionsByRoute[route], schemasByRoute[routeKey] || schemasByRoute[route]);
		const methods = normalizeMethods(mergedRoute.methods);
		for (const endpoint of mergedRoute.endpoints || []) {
			for (const method of normalizeEndpointMethods(endpoint)) {
				methods.push(method);
			}
		}
		const uniqueMethods = [...new Set(methods)].sort();
		const namespace = routeNamespace(route, mergedRoute);
		const classification = classifyRestRoute(route, { ...mergedRoute, namespace });

		for (const method of uniqueMethods) {
			const endpoint = (mergedRoute.endpoints || []).find((entry) => endpointMatchesMethod(entry, method)) || {};
			const methodRoute = {
				...mergedRoute,
				endpoints: (mergedRoute.endpoints || []).filter((entry) => endpointMatchesMethod(entry, method)),
				args: mergeArgsForMethod(mergedRoute, method),
				schema: routeSchemaForMethod(mergedRoute, method),
			};
			const id = restRouteMatrixKey({ method, route });
			discovered.push({
				id,
				key: id,
				method,
				route,
				path: routeDisplayPath(route),
				namespace,
				classification,
				auth: normalizeAuthMetadata(endpoint, mergedRoute),
				args: methodRoute.args,
				argsSummary: summarizeRouteArgs(methodRoute, options),
				responseSchema: methodRoute.schema,
				schemaSummary: summarizeSchema(methodRoute.schema, options),
				sources: ['rest-index', ...(optionsByRoute[routeKey] || optionsByRoute[route] ? ['options'] : []), ...(schemasByRoute[routeKey] || schemasByRoute[route] ? ['schema'] : [])],
			});
		}
	}

	return {
		schema: WORDPRESS_REST_ROUTE_DISCOVERY_SCHEMA,
		type: 'wordpress-rest-route-discovery',
		generated_at: options.generatedAt || options.generated_at || new Date(0).toISOString(),
		totals: {
			routes: Object.keys(routes).length,
			entries: discovered.length,
			namespaces: new Set(discovered.map((entry) => entry.namespace).filter(Boolean)).size,
			methods: new Set(discovered.map((entry) => entry.method).filter(Boolean)).size,
		},
		routes: discovered,
	};
}

function normalizeRestBaseUrl(baseUrl, restBasePath = DEFAULT_REST_BASE_PATH) {
	if (typeof baseUrl !== 'string' || baseUrl.trim() === '') {
		throw new TypeError('REST route discovery requires baseUrl for fetch discovery');
	}
	return new URL(restBasePath, baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`).toString();
}

function routeDiscoveryUrl(baseUrl, route, search = '') {
	const url = new URL(route.replace(/^\/+/, ''), normalizeRestBaseUrl(baseUrl));
	if (search) {
		url.search = search;
	}
	return url.toString();
}

async function readJsonResponse(response) {
	if (!response || response.ok === false) {
		return null;
	}
	if (typeof response.json === 'function') {
		return response.json();
	}
	return response;
}

async function discoverWordPressRestRoutes(input = {}, options = {}) {
	if (input.restIndex || input.rest_index || input.index || input.routes) {
		return normalizeWordPressRestRouteDiscovery(input, options);
	}

	const fetcher = options.fetch || input.fetch || globalThis.fetch;
	if (typeof fetcher !== 'function') {
		throw new TypeError('REST route discovery requires fetch when no restIndex is provided');
	}
	const baseUrl = input.baseUrl || input.base_url || options.baseUrl || options.base_url;
	const restIndex = await readJsonResponse(await fetcher(normalizeRestBaseUrl(baseUrl, options.restBasePath || options.rest_base_path)));
	const routes = normalizeRoutesObject(restIndex);
	const optionsByRoute = {};
	const schemasByRoute = {};

	for (const route of Object.keys(routes).sort()) {
		if (route.includes('(?P<')) {
			continue;
		}
		if (options.includeOptions !== false && options.include_options !== false) {
			optionsByRoute[route] = await readJsonResponse(await fetcher(routeDiscoveryUrl(baseUrl, route), { method: 'OPTIONS' }));
		}
		if (options.includeSchemas !== false && options.include_schemas !== false) {
			schemasByRoute[route] = await readJsonResponse(await fetcher(routeDiscoveryUrl(baseUrl, route, 'context=help')));
		}
	}

	return normalizeWordPressRestRouteDiscovery({ restIndex, optionsByRoute, schemasByRoute }, options);
}

module.exports = {
	DEFAULT_REST_BASE_PATH,
	WORDPRESS_REST_ROUTE_DISCOVERY_SCHEMA,
	discoverWordPressRestRoutes,
	normalizeWordPressRestRouteDiscovery,
	routeDiscoveryUrl,
};
