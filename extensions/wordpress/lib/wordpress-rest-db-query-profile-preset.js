'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');

const REST_DB_QUERY_PROFILE_PRESET_SCHEMA = 'homeboy/wordpress-rest-db-query-profile-preset/v1';
const REST_DB_QUERY_PROFILE_WORKLOAD_ID = 'rest-db-query-profile';
const REST_DB_QUERY_PROFILE_ARTIFACT_PATH = 'rest-db-query-profile/rest-db-query-profile.json';
const WORKLOAD_SOURCE_PATH = path.join(__dirname, '..', 'scripts', 'bench', 'workloads', 'rest-db-query-profile.php');

function assertObject(value, field) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`${field} must be an object.`);
	}
}

function normalizeArray(value, field) {
	if (value === undefined || value === null) {
		return [];
	}
	if (!Array.isArray(value)) {
		throw new Error(`${field} must be an array.`);
	}
	return value;
}

function normalizeRouteScope(scope, index) {
	assertObject(scope, `route_scopes[${index}]`);
	const id = scope.id || scope.surface || `route-scope-${index + 1}`;
	if (typeof id !== 'string' || id.trim() === '') {
		throw new Error(`route_scopes[${index}].id must be a string.`);
	}
	const prefixes = normalizeArray(scope.prefixes, `route_scopes[${index}].prefixes`);
	const patterns = normalizeArray(scope.patterns, `route_scopes[${index}].patterns`);
	if (prefixes.length === 0 && patterns.length === 0) {
		throw new Error(`route_scopes[${index}] requires prefixes or patterns.`);
	}

	return {
		...scope,
		id,
		prefixes,
		patterns,
	};
}

function normalizeRestRequestCase(testCase, index) {
	assertObject(testCase, `rest_request_cases[${index}]`);
	if (typeof testCase.path !== 'string' || testCase.path.trim() === '') {
		throw new Error(`rest_request_cases[${index}].path must be a string.`);
	}
	return {
		method: 'GET',
		params: {},
		capture_response: true,
		...testCase,
	};
}

function phpArrayAssignment(name, value) {
	return `$${name} = json_decode('${JSON.stringify(value).replace(/\\/g, '\\\\').replace(/'/g, "\\'")}', true);`;
}

function phpCallableSourceWrapper(source, config) {
	const body = String(source || '')
		.replace(/^\uFEFF/, '')
		.replace(/^\s*<\?php\s*/, '')
		.replace(/\?>\s*$/, '')
		.trim();
	return `${phpArrayAssignment('wp_codebox_rest_db_query_profile_config', config)}\n$wp_codebox_embedded_callable = (function () {\n${body}\n})(); return is_callable($wp_codebox_embedded_callable) ? $wp_codebox_embedded_callable() : $wp_codebox_embedded_callable;`;
}

function normalizePresetConfig(options = {}) {
	const routeScopes = normalizeArray(options.route_scopes || options.routeScopes, 'route_scopes').map(normalizeRouteScope);
	const restRequestCases = normalizeArray(options.rest_request_cases || options.restRequestCases, 'rest_request_cases').map(normalizeRestRequestCase);
	if (routeScopes.length === 0 && restRequestCases.length === 0) {
		throw new Error('A REST DB query profile preset requires route_scopes or rest_request_cases.');
	}

	return {
		route_scopes: routeScopes,
		rest_request_cases: restRequestCases,
		case_limit: options.case_limit || options.caseLimit || 80,
		query_length_limit: options.query_length_limit || options.queryLengthLimit || 500,
	};
}

function buildWordPressRestDbQueryProfileWorkload(options = {}) {
	const config = normalizePresetConfig(options);
	const source = fs.readFileSync(WORKLOAD_SOURCE_PATH, 'utf8');
	return {
		id: options.id || REST_DB_QUERY_PROFILE_WORKLOAD_ID,
		source: 'preset',
		run: [
			{
				type: 'php',
				code: phpCallableSourceWrapper(source, config),
				metadata: {
					preset: REST_DB_QUERY_PROFILE_PRESET_SCHEMA,
					embedded_source_file: true,
					source_file: WORKLOAD_SOURCE_PATH,
				},
			},
		],
		metadata: {
			runner: 'wp-codebox',
			workload: REST_DB_QUERY_PROFILE_WORKLOAD_ID,
			preset: REST_DB_QUERY_PROFILE_PRESET_SCHEMA,
			coverage_shape: 'configured WordPress REST route DB query profile',
		},
	};
}

function buildWordPressRestDbQueryProfileFuzzWorkload(options = {}) {
	const plugin = options.plugin || {};
	const activation = plugin.activation || options.activation;
	const component = plugin.component || options.component || plugin.slug || options.slug;
	const slug = plugin.slug || options.slug || component;
	const workload = buildWordPressRestDbQueryProfileWorkload(options);
	return {
		schema: 'homeboy/fuzz-workload/v1',
		id: options.id || REST_DB_QUERY_PROFILE_WORKLOAD_ID,
		label: options.label || 'REST DB query profile coverage',
		safety_class: 'read_only',
		surface_ids: options.surface_ids || options.surfaceIds || ['wordpress-rest-routes', 'wordpress-database-queries'],
		operations: ['rest-query-profile', 'query-shape-attribution'],
		case_budget: options.case_budget || options.caseBudget || 80,
		duration_budget_seconds: options.duration_budget_seconds || options.durationBudgetSeconds || 900,
		metadata: {
			kind: 'wordpress-plugin-fuzz',
			wordpress_runner: 'wp-codebox',
			preset: REST_DB_QUERY_PROFILE_PRESET_SCHEMA,
			expected_artifact_role: 'fuzz_report',
			expected_artifact_semantic_key: 'fuzz.report',
			fixture: {
				runtime: 'wp-codebox',
				scope: 'disposable-wordpress',
				component,
				activation,
			},
		},
		target: {
			type: 'wordpress-plugin',
			slug,
			component,
		},
		workload: {
			runner: 'wp-codebox',
			type: 'inline',
			entry: workload.id,
			definition: workload,
		},
		cases: [
			{
				case_id: `${workload.id}:default`,
				surface_ids: options.surface_ids || options.surfaceIds || ['wordpress-rest-routes', 'wordpress-database-queries'],
				operations: ['rest-query-profile', 'query-shape-attribution'],
				artifacts: [
					{
						name: 'rest_db_query_profile',
						path: REST_DB_QUERY_PROFILE_ARTIFACT_PATH,
						required: true,
						metadata: { semantic_key: 'fuzz.report' },
					},
				],
				metadata: { safety_class: 'read_only' },
				intent: {
					schema: 'homeboy/fuzz-workload-intent/v1',
					type: 'wordpress-plugin-workload',
					plugin: activation ? { activation } : undefined,
					execute: {
						workload_ref: 'default',
						type: 'inline',
						entry: workload.id,
						definition: workload,
					},
					collect: [{ artifact: 'rest_db_query_profile' }],
				},
			},
		],
		limits: {
			max_cases: options.case_budget || options.caseBudget || 80,
			max_duration_seconds: options.duration_budget_seconds || options.durationBudgetSeconds || 900,
		},
		coverage: {
			surface_ids: options.surface_ids || options.surfaceIds || ['wordpress-rest-routes', 'wordpress-database-queries'],
			operations: ['rest-query-profile', 'query-shape-attribution'],
		},
		artifacts: {
			expected: [
				{
					name: 'rest_db_query_profile',
					role: 'fuzz_report',
					semantic_key: 'fuzz.report',
					required: true,
				},
			],
		},
	};
}

module.exports = {
	REST_DB_QUERY_PROFILE_PRESET_SCHEMA,
	REST_DB_QUERY_PROFILE_WORKLOAD_ID,
	REST_DB_QUERY_PROFILE_ARTIFACT_PATH,
	WORKLOAD_SOURCE_PATH,
	buildWordPressRestDbQueryProfileFuzzWorkload,
	buildWordPressRestDbQueryProfileWorkload,
	normalizePresetConfig,
};
