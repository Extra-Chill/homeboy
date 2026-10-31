'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');
const { normalizeWordPressRestRouteMatrix } = require('./rest-route-matrix');

const WORDPRESS_REST_FUZZ_SURFACE_DISCOVERY_SCHEMA = 'homeboy/wordpress-rest-fuzz-surface-discovery/v1';
const WORDPRESS_FUZZ_SURFACES_SCHEMA = 'homeboy/wordpress-fuzz-surfaces/v1';
const DEFAULT_SURFACE_SCHEMA_PATHS = Object.freeze([
	'.homeboy/wordpress-fuzz-surfaces.json',
	'.homeboy/wordpress-rest-fuzz-surfaces.json',
	'homeboy-wordpress-fuzz-surfaces.json',
	'wordpress-fuzz-surfaces.json',
	'wordpress-rest-fuzz-surfaces.json',
]);

function normalizeSurfaceSchemaPaths(value) {
	if (value === undefined || value === null || value === '') {
		return [...DEFAULT_SURFACE_SCHEMA_PATHS];
	}
	if (Array.isArray(value)) {
		return value.flatMap(normalizeSurfaceSchemaPaths);
	}
	return [String(value)].filter(Boolean);
}

function resolveSurfaceSchemaFile(options = {}) {
	if (options.surfaceSchema || options.surface_schema) {
		return null;
	}

	const repoRoot = path.resolve(options.repoRoot || options.repo_root || process.cwd());
	for (const relativePath of normalizeSurfaceSchemaPaths(options.surfaceSchemaPaths || options.surface_schema_paths)) {
		const candidate = path.resolve(repoRoot, relativePath);
		if (!candidate.startsWith(`${repoRoot}${path.sep}`) && candidate !== repoRoot) {
			continue;
		}
		if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
			return candidate;
		}
	}

	return null;
}

function readJsonFile(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function loadRepositorySurfaceSchema(options = {}) {
	const inlineSchema = options.surfaceSchema || options.surface_schema;
	if (isPlainObject(inlineSchema)) {
		return {
			source: 'inline',
			path: '',
			artifact: inlineSchema,
		};
	}

	const filePath = resolveSurfaceSchemaFile(options);
	if (!filePath) {
		return null;
	}

	return {
		source: 'repository',
		path: filePath,
		artifact: readJsonFile(filePath),
	};
}

function routeInputFromOptions(options = {}) {
	return options.restIndex || options.rest_index || options.routes || options.routeIndex || options.route_index || null;
}

function buildRestIndexFuzzSurfaceArtifact(input = {}, options = {}) {
	const routeInput = routeInputFromOptions(options) || routeInputFromOptions(input) || input;
	const hasRoutes = isPlainObject(routeInput?.routes) || isPlainObject(routeInput);
	const cases = hasRoutes ? normalizeWordPressRestRouteMatrix(routeInput, options) : [];
	const namespaces = [...new Set(cases.map((entry) => entry.namespace).filter(Boolean))].sort();
	const methods = [...new Set(cases.map((entry) => entry.method).filter(Boolean))].sort();
	const generatedAt = options.generatedAt || options.generated_at || new Date(0).toISOString();

	return {
		schema: WORDPRESS_FUZZ_SURFACES_SCHEMA,
		type: 'wordpress-fuzz-surfaces',
		generated_at: generatedAt,
		source: 'rest-index',
		surfaces: [{
			id: 'wordpress-rest-api',
			kind: 'rest',
			schema: 'homeboy/wordpress-rest-fuzz-surface/v1',
			totals: {
				routes: cases.length,
				namespaces: namespaces.length,
				methods: methods.length,
			},
			namespaces,
			methods,
			routes: cases.map((entry) => ({
				id: entry.id,
				method: entry.method,
				path: entry.path,
				route: entry.route,
				namespace: entry.namespace,
				args: entry.argsSummary,
				response_schema: entry.schemaSummary,
			})),
		}],
	};
}

function discoverWordPressRestFuzzSurfaces(input = {}, options = {}) {
	const repositorySchema = loadRepositorySurfaceSchema({ ...input, ...options });
	const artifact = repositorySchema ? repositorySchema.artifact : buildRestIndexFuzzSurfaceArtifact(input, options);

	return {
		schema: WORDPRESS_REST_FUZZ_SURFACE_DISCOVERY_SCHEMA,
		type: 'wordpress-rest-fuzz-surface-discovery',
		adapter_id: 'homeboy/wordpress-rest-fuzz-surface-discovery/v1',
		source: repositorySchema ? repositorySchema.source : 'rest-index',
		source_path: repositorySchema?.path || '',
		artifact_schema: artifact?.schema || WORDPRESS_FUZZ_SURFACES_SCHEMA,
		artifact,
	};
}

module.exports = {
	DEFAULT_SURFACE_SCHEMA_PATHS,
	WORDPRESS_FUZZ_SURFACES_SCHEMA,
	WORDPRESS_REST_FUZZ_SURFACE_DISCOVERY_SCHEMA,
	buildRestIndexFuzzSurfaceArtifact,
	discoverWordPressRestFuzzSurfaces,
	loadRepositorySurfaceSchema,
};
