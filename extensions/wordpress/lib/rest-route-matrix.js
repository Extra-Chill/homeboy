'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');

const DEFAULT_METHODS = Object.freeze(['GET']);
const DEFAULT_ARG_LIMIT = 12;
const DEFAULT_SCHEMA_PROPERTY_LIMIT = 12;

function normalizeRestRouteMethod(method) {
	const normalized = String(method || 'GET').trim().toUpperCase();
	return normalized || 'GET';
}

function normalizeStringList(value) {
	if (value === undefined || value === null || value === '') {
		return [];
	}
	if (Array.isArray(value)) {
		return value.flatMap(normalizeStringList);
	}
	return [String(value).trim()].filter(Boolean);
}

function normalizeMethodFilter(value) {
	const methods = normalizeStringList(value === undefined ? DEFAULT_METHODS : value).map(normalizeRestRouteMethod);
	return new Set(methods.length > 0 ? methods : DEFAULT_METHODS);
}

function routeNamespace(routeKey, route = {}) {
	if (typeof route.namespace === 'string' && route.namespace.trim() !== '') {
		return route.namespace.trim();
	}
	const match = String(routeKey || '').match(/^\/?([^/]+\/v\d+(?:\.\d+)?)/i);
	return match ? match[1] : '';
}

function matchesNamespaceFilter(namespace, filters) {
	if (!filters.length) {
		return false;
	}
	return filters.some((filter) => namespace === filter || namespace.startsWith(`${filter}/`));
}

function shouldIncludeNamespace(namespace, options = {}) {
	const include = normalizeStringList(options.includeNamespaces ?? options.namespaces);
	const exclude = normalizeStringList(options.excludeNamespaces);
	if (include.length > 0 && !matchesNamespaceFilter(namespace, include)) {
		return false;
	}
	if (matchesNamespaceFilter(namespace, exclude)) {
		return false;
	}
	return true;
}

function normalizeRoutePath(routeKey) {
	const raw = String(routeKey || '').trim();
	if (!raw) {
		throw new TypeError('REST route keys must be non-empty strings');
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

function restRouteMatrixKey({ method, route }) {
	const routePart = routeDisplayPath(route)
		.replace(/^\/+/, '')
		.replace(/\{([^}]+)\}/g, '-$1-')
		.replace(/[^A-Za-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '')
		.toLowerCase() || 'index';
	return `rest:${normalizeRestRouteMethod(method).toLowerCase()}:${routePart}`;
}

function routeHasPathParams(routeKey) {
	const route = String(routeKey || '');
	return /\(\?P<[^>]+>/.test(route) || /\([^)]*[+*?][^)]*\)/.test(route);
}

function classifyRestRoute(routeKey, route = {}) {
	const path = normalizeRoutePath(routeKey);
	const namespace = routeNamespace(path, route);
	const pathAfterNamespace = namespace && path.startsWith(`/${namespace}`)
		? path.slice(namespace.length + 1)
		: path;
	const segments = pathAfterNamespace.split('/').filter(Boolean);
	const hasPathParams = routeHasPathParams(path);

	return {
		namespace,
		family: namespace === 'wp/v2' ? 'wordpress-core' : 'extension',
		kind: path === '/' || path === '/wp-json' ? 'index' : hasPathParams ? 'item' : 'collection',
		segments,
		hasPathParams,
	};
}

function normalizeRouteMethods(route = {}) {
	const methods = new Set();
	for (const method of route.methods || []) {
		methods.add(normalizeRestRouteMethod(method));
	}
	for (const endpoint of route.endpoints || []) {
		for (const method of endpoint?.methods || []) {
			methods.add(normalizeRestRouteMethod(method));
		}
	}
	return [...methods].sort();
}

function summarizeArg(name, arg = {}) {
	const enumValues = Array.isArray(arg.enum) ? arg.enum : undefined;
	return {
		name,
		type: arg.type || arg.schema?.type || 'unknown',
		required: arg.required === true,
		hasDefault: Object.prototype.hasOwnProperty.call(arg, 'default'),
		enumCount: enumValues ? enumValues.length : undefined,
	};
}

function summarizeRouteArgs(route = {}, options = {}) {
	const limit = Math.max(0, Math.floor(Number(options.maxArgs ?? DEFAULT_ARG_LIMIT)));
	const argsByName = new Map();
	for (const endpoint of route.endpoints || []) {
		for (const [name, arg] of Object.entries(endpoint?.args || {})) {
			if (!argsByName.has(name)) {
				argsByName.set(name, arg);
			}
		}
	}
	for (const [name, arg] of Object.entries(route.args || {})) {
		if (!argsByName.has(name)) {
			argsByName.set(name, arg);
		}
	}
	const args = [...argsByName.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([name, arg]) => summarizeArg(name, arg));
	return {
		count: args.length,
		truncated: limit > 0 && args.length > limit,
		args: limit > 0 ? args.slice(0, limit) : [],
	};
}

function summarizeSchema(schema, options = {}) {
	if (!isPlainObject(schema)) {
		return { type: 'unknown', properties: [], propertyCount: 0, truncated: false };
	}
	const limit = Math.max(0, Math.floor(Number(options.maxSchemaProperties ?? DEFAULT_SCHEMA_PROPERTY_LIMIT)));
	const properties = Object.entries(schema.properties || {})
		.sort(([a], [b]) => a.localeCompare(b))
		.map(([name, property]) => ({
			name,
			type: property?.type || 'unknown',
		}));
	return {
		type: schema.type || 'object',
		propertyCount: properties.length,
		truncated: limit > 0 && properties.length > limit,
		properties: limit > 0 ? properties.slice(0, limit) : [],
	};
}

function routeSchema(route = {}) {
	if (isPlainObject(route.schema)) {
		return route.schema;
	}
	for (const endpoint of route.endpoints || []) {
		if (isPlainObject(endpoint?.schema)) {
			return endpoint.schema;
		}
	}
	return undefined;
}

function extractRestRoutes(input) {
	if (isPlainObject(input?.routes)) {
		return input.routes;
	}
	if (isPlainObject(input)) {
		return input;
	}
	throw new TypeError('REST route matrix input must be a WP REST index or routes object');
}

function normalizeWordPressRestRouteMatrix(input, options = {}) {
	const routes = extractRestRoutes(input);
	const methodFilter = normalizeMethodFilter(options.methods ?? options.method);
	const cases = [];

	for (const routeKey of Object.keys(routes).sort()) {
		const route = routes[routeKey] || {};
		const normalizedRoute = normalizeRoutePath(routeKey);
		const namespace = routeNamespace(normalizedRoute, route);
		if (!shouldIncludeNamespace(namespace, options)) {
			continue;
		}
		const classification = classifyRestRoute(normalizedRoute, route);
		const routeMethods = normalizeRouteMethods(route).filter((method) => methodFilter.has(method));
		for (const method of routeMethods) {
			const id = restRouteMatrixKey({ method, route: normalizedRoute });
			cases.push({
				id,
				key: id,
				label: `${method} ${routeDisplayPath(normalizedRoute)}`,
				method,
				path: routeDisplayPath(normalizedRoute),
				route: normalizedRoute,
				namespace,
				classification,
				argsSummary: summarizeRouteArgs(route, options),
				schemaSummary: summarizeSchema(routeSchema(route), options),
			});
		}
	}

	return cases;
}

module.exports = {
	classifyRestRoute,
	normalizeRestRouteMethod,
	normalizeWordPressRestRouteMatrix,
	restRouteMatrixKey,
	summarizeRouteArgs,
	summarizeSchema,
};
