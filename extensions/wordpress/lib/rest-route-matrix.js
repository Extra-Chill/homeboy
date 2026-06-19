'use strict';

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');

const DEFAULT_METHODS = Object.freeze(['GET']);
const DEFAULT_ARG_LIMIT = 12;
const DEFAULT_SCHEMA_PROPERTY_LIMIT = 12;
const DEFAULT_REST_REQUEST_CASE_LIMIT = 24;
const PAGINATION_ARG_NAMES = Object.freeze(['page', 'per_page', 'offset']);

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

function routePathParamNames(routeKey) {
	return [...String(routeKey || '').matchAll(/\(\?P<([^>]+)>[^)]+\)/g)]
		.map((match) => match[1])
		.filter(Boolean);
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

function argType(arg = {}) {
	const type = arg.type || arg.schema?.type;
	return Array.isArray(type) ? type[0] : type || 'unknown';
}

function argEnumValues(arg = {}) {
	if (Array.isArray(arg.enum)) {
		return arg.enum;
	}
	if (Array.isArray(arg.schema?.enum)) {
		return arg.schema.enum;
	}
	return [];
}

function hasOwn(value, key) {
	return Object.prototype.hasOwnProperty.call(value, key);
}

function seededScore(value, seed) {
	const input = `${seed}:${value}`;
	let hash = 2166136261;
	for (let index = 0; index < input.length; index += 1) {
		hash ^= input.charCodeAt(index);
		hash = Math.imul(hash, 16777619);
	}
	return hash >>> 0;
}

function stableSeededSort(values, seed) {
	return [...values].sort((a, b) => {
		const scoreA = seededScore(JSON.stringify(a), seed);
		const scoreB = seededScore(JSON.stringify(b), seed);
		return scoreA === scoreB ? JSON.stringify(a).localeCompare(JSON.stringify(b)) : scoreA - scoreB;
	});
}

function safeValueForArg(name, arg = {}, options = {}) {
	const enums = argEnumValues(arg);
	if (enums.length > 0) {
		return stableSeededSort(enums, options.seed ?? 'rest-request-cases')[0];
	}
	if (hasOwn(arg, 'default')) {
		return arg.default;
	}
	if (hasOwn(arg.schema || {}, 'default')) {
		return arg.schema.default;
	}

	const type = argType(arg);
	if (type === 'integer') {
		return Number.isFinite(Number(arg.minimum)) ? Number(arg.minimum) : 1;
	}
	if (type === 'number') {
		return Number.isFinite(Number(arg.minimum)) ? Number(arg.minimum) : 1;
	}
	if (type === 'boolean') {
		return true;
	}
	if (type === 'array') {
		return [];
	}
	if (type === 'object') {
		return {};
	}
	if (type === 'string') {
		return name === 'context' ? 'view' : 'test';
	}
	return undefined;
}

function endpointSupportsMethod(endpoint = {}, method) {
	const methods = endpoint?.methods || [];
	return methods.map(normalizeRestRouteMethod).includes(normalizeRestRouteMethod(method));
}

function routeArgsForMethod(route = {}, method) {
	const argsByName = new Map();
	for (const [name, arg] of Object.entries(route.args || {})) {
		argsByName.set(name, arg);
	}
	for (const endpoint of route.endpoints || []) {
		if (!endpointSupportsMethod(endpoint, method)) {
			continue;
		}
		for (const [name, arg] of Object.entries(endpoint?.args || {})) {
			argsByName.set(name, arg);
		}
	}
	return [...argsByName.entries()].sort(([a], [b]) => a.localeCompare(b));
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

function caseKey(baseId, variant, parts = []) {
	return [baseId, ...[variant, ...parts]
		.map((part) => String(part).trim().toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, ''))]
		.filter(Boolean)
		.join(':');
}

function applyPathParams(path, pathParams = {}) {
	return String(path).replace(/\{([^}]+)\}/g, (match, name) => {
		const value = pathParams[name];
		return value === undefined ? match : encodeURIComponent(String(value));
	});
}

function splitArgsByLocation(routeKey, args) {
	const pathParamNames = new Set(routePathParamNames(routeKey));
	const pathArgs = [];
	const requestArgs = [];
	for (const entry of args) {
		(pathParamNames.has(entry[0]) ? pathArgs : requestArgs).push(entry);
	}
	return { pathArgs, requestArgs };
}

function buildRequestCase(matrixEntry, variant, values = {}, metadata = {}) {
	const path = applyPathParams(matrixEntry.path, values.pathParams);
	const method = normalizeRestRouteMethod(matrixEntry.method);
	const query = method === 'GET' || method === 'DELETE' ? values.args || {} : {};
	const body = method === 'GET' || method === 'DELETE' ? undefined : values.args || {};
	const { parts, ...caseMetadata } = metadata;
	return {
		id: caseKey(matrixEntry.id, variant, parts || []),
		key: caseKey(matrixEntry.id, variant, parts || []),
		matrixId: matrixEntry.id,
		variant,
		label: `${matrixEntry.label} (${variant})`,
		method,
		path,
		route: matrixEntry.route,
		namespace: matrixEntry.namespace,
		request: {
			method,
			path,
			...(Object.keys(query).length > 0 ? { query } : {}),
			...(body === undefined || Object.keys(body).length === 0 ? {} : { body }),
		},
		metadata: caseMetadata,
	};
}

function plannedCase(kind, argName, reason, details = {}) {
	return {
		kind,
		arg: argName,
		reason,
		...details,
	};
}

function normalizeCaseLimit(value) {
	const limit = Math.floor(Number(value ?? DEFAULT_REST_REQUEST_CASE_LIMIT));
	return Number.isFinite(limit) && limit > 0 ? limit : DEFAULT_REST_REQUEST_CASE_LIMIT;
}

function generateWordPressRestRequestCasesForEntry(matrixEntry, route = {}, options = {}) {
	if (!isPlainObject(matrixEntry)) {
		throw new TypeError('REST request case entries must be normalized route matrix objects');
	}
	const seed = String(options.seed ?? 'rest-request-cases');
	const maxCases = normalizeCaseLimit(options.maxCases);
	const args = routeArgsForMethod(route, matrixEntry.method);
	const { pathArgs, requestArgs } = splitArgsByLocation(matrixEntry.route, args);
	const pathParams = {};
	const cases = [];
	const plannedCases = [];

	for (const [name, arg] of pathArgs) {
		const value = safeValueForArg(name, arg, { seed });
		if (value === undefined) {
			plannedCases.push(plannedCase('required-path-arg', name, 'No generic safe path value is available.', { type: argType(arg) }));
		} else {
			pathParams[name] = value;
		}
	}

	cases.push(buildRequestCase(matrixEntry, 'baseline', { pathParams }, { seed, plannedCases: [] }));

	const requiredValues = {};
	for (const [name, arg] of requestArgs) {
		if (arg.required !== true) {
			continue;
		}
		const value = safeValueForArg(name, arg, { seed });
		if (value === undefined) {
			plannedCases.push(plannedCase('required-arg', name, 'Required arg is not executable without a generic safe value.', { type: argType(arg) }));
		} else {
			requiredValues[name] = value;
		}
	}
	if (Object.keys(requiredValues).length > 0) {
		cases.push(buildRequestCase(matrixEntry, 'required-args', { pathParams, args: requiredValues }, { seed, args: Object.keys(requiredValues) }));
	}

	for (const [name, arg] of requestArgs) {
		if (PAGINATION_ARG_NAMES.includes(name)) {
			const value = name === 'per_page' ? 1 : 2;
			cases.push(buildRequestCase(matrixEntry, 'pagination', { pathParams, args: { ...requiredValues, [name]: value } }, { seed, arg: name, parts: [name] }));
		}

		if (hasOwn(arg, 'default') || hasOwn(arg.schema || {}, 'default')) {
			cases.push(buildRequestCase(matrixEntry, 'default', {
				pathParams,
				args: { ...requiredValues, [name]: hasOwn(arg, 'default') ? arg.default : arg.schema.default },
			}, { seed, arg: name, parts: [name] }));
		}

		for (const value of stableSeededSort(argEnumValues(arg), `${seed}:${name}`).slice(0, 2)) {
			cases.push(buildRequestCase(matrixEntry, 'enum', { pathParams, args: { ...requiredValues, [name]: value } }, { seed, arg: name, value, parts: [name, value] }));
		}

		if (arg.required === true) {
			plannedCases.push(plannedCase('missing-required-arg', name, 'Missing required arg is a negative case; execute only when the caller opts into unsafe requests.', { type: argType(arg) }));
		}
		if (argEnumValues(arg).length > 0) {
			plannedCases.push(plannedCase('invalid-enum', name, 'Invalid enum value is tracked as planned metadata by default.', { allowed: argEnumValues(arg) }));
		}
		if (argType(arg) === 'integer' || argType(arg) === 'number') {
			plannedCases.push(plannedCase('numeric-boundary', name, 'Numeric boundary case needs caller policy before execution.', {
				minimum: arg.minimum,
				maximum: arg.maximum,
			}));
		}
	}

	const uniqueCases = [];
	const seenKeys = new Set();
	for (const requestCase of cases) {
		if (seenKeys.has(requestCase.key)) {
			continue;
		}
		seenKeys.add(requestCase.key);
		uniqueCases.push(requestCase);
	}

	return uniqueCases.slice(0, maxCases).map((requestCase) => ({
		...requestCase,
		metadata: {
			...requestCase.metadata,
			plannedCases,
		},
	}));
}

function generateWordPressRestRequestCases(input, options = {}) {
	const routes = extractRestRoutes(input);
	const matrix = normalizeWordPressRestRouteMatrix(input, options);
	const maxCases = normalizeCaseLimit(options.maxCases);
	const cases = [];

	for (const entry of matrix) {
		const route = routes[entry.route] || routes[normalizeRoutePath(entry.route)] || {};
		cases.push(...generateWordPressRestRequestCasesForEntry(entry, route, { ...options, maxCases }));
		if (cases.length >= maxCases) {
			return cases.slice(0, maxCases);
		}
	}

	return cases;
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
	generateWordPressRestRequestCases,
	generateWordPressRestRequestCasesForEntry,
	restRouteMatrixKey,
	summarizeRouteArgs,
	summarizeSchema,
};
