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
const DEFAULT_REPORT_LIMIT = 10;

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

function normalizeOptionalRoutePath(routeKey) {
	const raw = String(routeKey || '').trim();
	if (!raw) {
		return '';
	}
	return normalizeRoutePath(raw);
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

function numericValue(value, fallback = 0) {
	const number = Number(value);
	return Number.isFinite(number) ? number : fallback;
}

function maybeNumericValue(value) {
	const number = Number(value);
	return Number.isFinite(number) ? number : undefined;
}

function sortText(a, b) {
	return String(a || '').localeCompare(String(b || ''));
}

function statusKey(status) {
	return status === undefined || status === null || status === '' ? 'unknown' : String(status);
}

function stripRestBase(value) {
	let raw = String(value || '').trim();
	if (!raw) {
		return '';
	}

	try {
		const parsed = new URL(raw, 'https://example.test');
		const restRoute = parsed.searchParams.get('rest_route');
		if (restRoute) {
			return normalizeOptionalRoutePath(restRoute);
		}
		raw = parsed.pathname;
	} catch {
		raw = raw.replace(/^https?:\/\/[^/]+/i, '').split('?', 1)[0];
	}

	return normalizeOptionalRoutePath(raw.replace(/^\/wp-json(?=\/|$)/, '') || raw);
}

function resultRoutePath(result = {}) {
	return stripRestBase(result.route || result.path || result.normalizedUrl || result.url || result.uri || '');
}

function resultCaseKey(result = {}) {
	if (result.id || result.key || result.caseId || result.case_id) {
		return String(result.id || result.key || result.caseId || result.case_id);
	}
	const route = resultRoutePath(result);
	return route ? restRouteMatrixKey({ method: result.method, route }) : '';
}

function normalizeRestMatrixInventoryCase(entry = {}) {
	if (!isPlainObject(entry)) {
		throw new TypeError('REST matrix inventory cases must be objects');
	}
	const route = normalizeOptionalRoutePath(entry.route || entry.path || entry.normalizedUrl || entry.url || entry.uri || '');
	if (!route) {
		throw new TypeError('REST matrix inventory cases require route or path');
	}
	const method = normalizeRestRouteMethod(entry.method);
	const namespace = entry.namespace || entry.classification?.namespace || routeNamespace(route, entry);
	const id = entry.id || entry.key || restRouteMatrixKey({ method, route });
	return {
		id,
		key: id,
		label: entry.label || `${method} ${routeDisplayPath(route)}`,
		method,
		path: routeDisplayPath(route),
		route,
		namespace,
		classification: entry.classification || classifyRestRoute(route, { namespace }),
	};
}

function normalizeRestMatrixResult(entry = {}) {
	if (!isPlainObject(entry)) {
		throw new TypeError('REST matrix results must be objects');
	}
	const route = resultRoutePath(entry);
	const method = normalizeRestRouteMethod(entry.method);
	const id = resultCaseKey({ ...entry, method, route });
	const namespace = entry.namespace || entry.classification?.namespace || (route ? routeNamespace(route, entry) : '');
	const status = entry.status ?? entry.statusCode ?? entry.responseStatus;
	const durationMs = maybeNumericValue(entry.durationMs ?? entry.duration_ms ?? entry.totalMs ?? entry.total_ms);
	const queryCount = maybeNumericValue(entry.queryCount ?? entry.query_count ?? entry.dbQueryCount ?? entry.db_query_count);
	const responseBytes = maybeNumericValue(entry.responseBytes ?? entry.response_bytes ?? entry.bytes ?? entry.bodyBytes ?? entry.body_bytes);
	const covered = entry.covered === false ? false : Boolean(entry.covered === true || status !== undefined || entry.ok !== undefined || durationMs !== undefined || queryCount !== undefined);

	return {
		id,
		key: id,
		method,
		path: route ? routeDisplayPath(route) : '',
		route,
		namespace,
		status,
		durationMs,
		queryCount,
		responseBytes,
		covered,
		findings: Array.isArray(entry.findings) ? entry.findings : [],
	};
}

function incrementCoverage(group, name, covered) {
	const key = name || 'unknown';
	if (!group[key]) {
		group[key] = { total: 0, covered: 0, uncovered: 0 };
	}
	group[key].total += 1;
	if (covered) {
		group[key].covered += 1;
	} else {
		group[key].uncovered += 1;
	}
}

function sortedCoverageObject(group) {
	return Object.fromEntries(Object.entries(group).sort(([a], [b]) => sortText(a, b)));
}

function normalizeAllowedStatuses(value) {
	if (value === undefined || value === null) {
		return null;
	}
	return new Set(normalizeStringList(value).map(String));
}

function budgetFinding(row, type, metric, actual, budget) {
	return {
		type,
		severity: 'warning',
		id: row.id,
		method: row.method,
		path: row.path,
		namespace: row.namespace,
		metric,
		actual,
		budget,
		message: `${row.method} ${row.path} ${metric} ${actual} exceeds budget ${budget}`,
	};
}

function restMatrixBudgetFindings(row, budgets = {}) {
	const findings = [];
	if (row.durationMs !== undefined && budgets.maxDurationMs !== undefined && row.durationMs > numericValue(budgets.maxDurationMs, Infinity)) {
		findings.push(budgetFinding(row, 'duration_budget_exceeded', 'durationMs', row.durationMs, numericValue(budgets.maxDurationMs)));
	}
	if (row.queryCount !== undefined && budgets.maxQueryCount !== undefined && row.queryCount > numericValue(budgets.maxQueryCount, Infinity)) {
		findings.push(budgetFinding(row, 'query_count_budget_exceeded', 'queryCount', row.queryCount, numericValue(budgets.maxQueryCount)));
	}
	const allowedStatuses = normalizeAllowedStatuses(budgets.allowedStatuses ?? budgets.allowedStatusCodes);
	if (allowedStatuses && row.status !== undefined && !allowedStatuses.has(String(row.status))) {
		findings.push({
			type: 'status_not_allowed',
			severity: 'warning',
			id: row.id,
			method: row.method,
			path: row.path,
			namespace: row.namespace,
			metric: 'status',
			actual: row.status,
			budget: [...allowedStatuses].sort(sortText),
			message: `${row.method} ${row.path} returned status ${row.status}`,
		});
	}
	return findings;
}

function normalizeInputCases(input = {}, options = {}) {
	const inventory = input.inventory || input.cases || input.routes || input.restIndex || input.routeInventory;
	if (Array.isArray(inventory)) {
		return inventory.map(normalizeRestMatrixInventoryCase).sort((a, b) => sortText(a.id, b.id));
	}
	if (isPlainObject(inventory)) {
		return normalizeWordPressRestRouteMatrix(inventory, options).map(normalizeRestMatrixInventoryCase).sort((a, b) => sortText(a.id, b.id));
	}
	return [];
}

function normalizeInputResults(input = {}) {
	const results = input.results || input.caseResults || input.case_results || [];
	if (!Array.isArray(results)) {
		throw new TypeError('REST matrix case results must be an array');
	}
	return results.map(normalizeRestMatrixResult).sort((a, b) => sortText(a.id, b.id));
}

function buildRestRouteMatrixArtifact(input = {}, options = {}) {
	const cases = normalizeInputCases(input, options);
	const results = normalizeInputResults(input);
	const resultById = new Map(results.filter((result) => result.id).map((result) => [result.id, result]));
	const rowsById = new Map();

	for (const inventoryCase of cases) {
		const result = resultById.get(inventoryCase.id);
		rowsById.set(inventoryCase.id, {
			...inventoryCase,
			...(result || {}),
			id: inventoryCase.id,
			key: inventoryCase.id,
			method: result?.method || inventoryCase.method,
			path: result?.path || inventoryCase.path,
			route: result?.route || inventoryCase.route,
			namespace: result?.namespace || inventoryCase.namespace,
			classification: result?.classification || inventoryCase.classification,
			covered: result ? result.covered : false,
		});
	}

	for (const result of results) {
		if (result.id && !rowsById.has(result.id)) {
			rowsById.set(result.id, {
				...result,
				label: result.route ? `${result.method} ${result.path}` : result.id,
			});
		}
	}

	const rows = [...rowsById.values()].sort((a, b) => sortText(a.id, b.id));
	const byNamespace = {};
	const byMethod = {};
	const byStatus = {};
	const budgetFindings = [];
	const missingRoutes = [];
	const uncoveredRoutes = [];

	for (const row of rows) {
		incrementCoverage(byNamespace, row.namespace, row.covered);
		incrementCoverage(byMethod, row.method, row.covered);
		if (row.covered) {
			incrementCoverage(byStatus, statusKey(row.status), true);
		} else {
			missingRoutes.push(row);
			uncoveredRoutes.push(row);
		}
		for (const finding of row.findings || []) {
			budgetFindings.push({ id: row.id, method: row.method, path: row.path, namespace: row.namespace, ...finding });
		}
		budgetFindings.push(...restMatrixBudgetFindings(row, options.budgets || input.budgets || {}));
	}

	const slowestByDuration = rows
		.filter((row) => row.durationMs !== undefined)
		.sort((a, b) => b.durationMs - a.durationMs || sortText(a.id, b.id))
		.slice(0, Math.max(0, Math.floor(numericValue(options.slowestLimit ?? DEFAULT_REPORT_LIMIT))));
	const slowestByQueryCount = rows
		.filter((row) => row.queryCount !== undefined)
		.sort((a, b) => b.queryCount - a.queryCount || sortText(a.id, b.id))
		.slice(0, Math.max(0, Math.floor(numericValue(options.slowestLimit ?? DEFAULT_REPORT_LIMIT))));

	return {
		schema: 'homeboy/wordpress-rest-route-matrix-artifact/v1',
		type: 'wordpress-rest-route-matrix-artifact',
		totals: {
			routeCount: cases.length,
			resultCount: results.length,
			caseCount: rows.length,
			coveredCount: rows.filter((row) => row.covered).length,
			uncoveredCount: rows.filter((row) => !row.covered).length,
			budgetFindingCount: budgetFindings.length,
		},
		coverage: {
			byNamespace: sortedCoverageObject(byNamespace),
			byMethod: sortedCoverageObject(byMethod),
			byStatus: sortedCoverageObject(byStatus),
		},
		slowestByDuration,
		slowestByQueryCount,
		missingRoutes: missingRoutes.sort((a, b) => sortText(a.id, b.id)),
		uncoveredRoutes: uncoveredRoutes.sort((a, b) => sortText(a.id, b.id)),
		budgetFindings: budgetFindings.sort((a, b) => sortText(a.id, b.id) || sortText(a.type, b.type)),
		routes: rows,
	};
}

function escapeMarkdownCell(value) {
	return String(value ?? '').replace(/\|/g, '\\|').replace(/\n/g, ' ');
}

function formatNumber(value) {
	const number = maybeNumericValue(value);
	return number === undefined ? '-' : String(Math.round(number * 10) / 10);
}

function formatCoverageTable(group, label) {
	const lines = [`| ${label} | Total | Covered | Uncovered |`, '| --- | ---: | ---: | ---: |'];
	for (const [key, row] of Object.entries(group)) {
		lines.push(`| ${escapeMarkdownCell(key)} | ${row.total} | ${row.covered} | ${row.uncovered} |`);
	}
	return lines;
}

function formatRouteRows(rows, options = {}) {
	const limit = Math.max(0, Math.floor(numericValue(options.limit ?? DEFAULT_REPORT_LIMIT)));
	const visibleRows = limit > 0 ? rows.slice(0, limit) : rows;
	const lines = ['| Route | Status | Duration ms | Query count | Findings |', '| --- | ---: | ---: | ---: | ---: |'];
	for (const row of visibleRows) {
		lines.push(`| \`${escapeMarkdownCell(`${row.method} ${row.path || row.route || row.id}`)}\` | ${escapeMarkdownCell(statusKey(row.status))} | ${formatNumber(row.durationMs)} | ${formatNumber(row.queryCount)} | ${(row.findings || []).length} |`);
	}
	return lines;
}

function formatRestRouteMatrixMarkdownReport(input = {}, options = {}) {
	const artifact = input?.schema === 'homeboy/wordpress-rest-route-matrix-artifact/v1'
		? input
		: buildRestRouteMatrixArtifact(input, options);
	const limit = Math.max(0, Math.floor(numericValue(options.limit ?? DEFAULT_REPORT_LIMIT)));
	const lines = [
		`## ${options.title || 'WordPress REST route matrix'}`,
		'',
		`Routes: ${artifact.totals.routeCount}; results: ${artifact.totals.resultCount}; covered: ${artifact.totals.coveredCount}; uncovered: ${artifact.totals.uncoveredCount}; budget findings: ${artifact.totals.budgetFindingCount}`,
		'',
		'## Coverage by namespace',
		'',
		...formatCoverageTable(artifact.coverage.byNamespace, 'Namespace'),
		'',
		'## Coverage by method',
		'',
		...formatCoverageTable(artifact.coverage.byMethod, 'Method'),
		'',
		'## Coverage by status',
		'',
		...formatCoverageTable(artifact.coverage.byStatus, 'Status'),
	];

	if (artifact.slowestByDuration.length > 0) {
		lines.push('', '## Slowest routes by duration', '', ...formatRouteRows(artifact.slowestByDuration, { limit }));
	}
	if (artifact.slowestByQueryCount.length > 0) {
		lines.push('', '## Slowest routes by query count', '', ...formatRouteRows(artifact.slowestByQueryCount, { limit }));
	}
	if (artifact.uncoveredRoutes.length > 0) {
		lines.push('', '## Missing or uncovered routes', '', ...formatRouteRows(artifact.uncoveredRoutes, { limit }));
	}
	if (artifact.budgetFindings.length > 0) {
		lines.push('', '## Budget findings', '', '| Route | Type | Metric | Actual | Budget |', '| --- | --- | --- | ---: | --- |');
		for (const finding of artifact.budgetFindings.slice(0, limit > 0 ? limit : undefined)) {
			lines.push(`| \`${escapeMarkdownCell(`${finding.method} ${finding.path}`)}\` | ${escapeMarkdownCell(finding.type)} | ${escapeMarkdownCell(finding.metric || '')} | ${escapeMarkdownCell(finding.actual)} | ${escapeMarkdownCell(Array.isArray(finding.budget) ? finding.budget.join(', ') : finding.budget)} |`);
		}
	}

	return lines.join('\n');
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
	buildRestRouteMatrixArtifact,
	classifyRestRoute,
	formatRestRouteMatrixMarkdownReport,
	normalizeRestRouteMethod,
	normalizeWordPressRestRouteMatrix,
	generateWordPressRestRequestCases,
	generateWordPressRestRequestCasesForEntry,
	restRouteMatrixKey,
	summarizeRouteArgs,
	summarizeSchema,
};
