'use strict';

/**
 * Internal dependencies
 */
const {
	normalizeWordPressFuzzRuntimeCapabilities,
} = require('./wordpress-fuzz-runtime-capabilities');
const {
	normalizeWordPressFuzzMutationLifecycleContract,
} = require('./wordpress-fuzz-mutation-lifecycle');

const WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA = 'homeboy/wordpress-fuzz-runtime-workload-operation/v1';

const FAMILY_COMMANDS = Object.freeze({
	crud: 'wordpress.crud',
	rest: 'wordpress.request-rest-route',
	admin_page: 'wordpress.load-admin-page',
	frontend_page: 'wordpress.load-frontend-page',
	block: 'wordpress.exercise-block',
	database: 'wordpress.profile-database',
});

const FAMILY_REQUIRED_CAPABILITIES = Object.freeze({
	crud: Object.freeze(['crud']),
	rest: Object.freeze(['rest']),
	admin_page: Object.freeze(['admin']),
	frontend_page: Object.freeze(['browser']),
	block: Object.freeze(['block']),
	database: Object.freeze(['database']),
});

function buildWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase = {}, options = {}) {
	const family = runtimeOperationFamily(testCase);
	if (!family) {
		return undefined;
	}

	const requiredCapabilities = requiredCapabilitiesForWordPressFuzzRuntimeOperation(testCase, { family });
	const runtimeCapabilitiesProvided = runtimeCapabilitiesWereProvided(options.runtimeCapabilities ?? options.runtime_capabilities);
	const missingCapabilities = runtimeCapabilitiesProvided
		? missingRuntimeCapabilities(requiredCapabilities, options.runtimeCapabilities ?? options.runtime_capabilities)
		: [];
	const skipped = missingCapabilities.length > 0;
	const mutationLifecycle = normalizeWordPressFuzzMutationLifecycleContract(testCase.metadata?.mutation_lifecycle || testCase.metadata?.mutationLifecycle || testCase.mutation_lifecycle || testCase.mutationLifecycle);

	return stripUndefined({
		schema: WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA,
		id: testCase.operation_id || testCase.operationId || testCase.id,
		case_id: testCase.case_id || testCase.caseId || testCase.id,
		family,
		command: FAMILY_COMMANDS[family],
		status: skipped ? 'skipped' : 'ready',
		required_capabilities: requiredCapabilities,
		missing_capabilities: skipped ? missingCapabilities : undefined,
		skip_reason: skipped ? 'missing-runtime-workload-capability' : undefined,
		input: runtimeOperationInput(testCase, { family }),
		mutation_lifecycle: mutationLifecycle,
		metadata: stripUndefined({
			intent: testCase.intent,
			operation_id: testCase.operation_id || testCase.operationId,
			mutation_lifecycle: mutationLifecycle,
		}),
	});
}

function attachWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase = {}, options = {}) {
	const runtimeOperation = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase, options);
	if (!runtimeOperation) {
		return testCase;
	}

	const metadata = objectOrUndefined(testCase.metadata) || {};
	if (runtimeOperation.status !== 'skipped') {
		return stripUndefined({
			...testCase,
			runtime_operation: runtimeOperation,
			metadata: stripUndefined({
				...metadata,
				runtime_operation: runtimeOperation,
			}),
		});
	}

	const skipReasons = reasonList(testCase.skip_reasons || testCase.skipReasons || testCase.skip_reason || testCase.skipReason);
	const requiredCapabilities = mergeCapabilities(testCase.required_capabilities || testCase.requiredCapabilities, runtimeOperation.required_capabilities);
	return stripUndefined({
		...testCase,
		executable: false,
		execution_tier: 'plan_only',
		required_capabilities: requiredCapabilities,
		skip_reasons: reasonList([...skipReasons, runtimeOperation.skip_reason]),
		runtime_operation: runtimeOperation,
		metadata: stripUndefined({
			...metadata,
			executable: false,
			execution_tier: 'plan_only',
			planned: true,
			gated: true,
			runtime_workload_capability_gated: true,
			missing_runtime_workload_capabilities: runtimeOperation.missing_capabilities,
			runtime_operation: runtimeOperation,
		}),
	});
}

function requiredCapabilitiesForWordPressFuzzRuntimeOperation(testCase = {}, options = {}) {
	const family = options.family || runtimeOperationFamily(testCase);
	const base = FAMILY_REQUIRED_CAPABILITIES[family] || [];
	return mergeCapabilities(base, extraCapabilitiesForCase(testCase, family));
}

function runtimeOperationFamily(testCase = {}) {
	const operation = objectOrUndefined(testCase.operation) || {};

	if (operation.schema === 'homeboy/wordpress-crud-operation/v1' || testCase.metadata?.crud) {
		return 'crud';
	}

	const intent = String(testCase.intent || '').trim();
	const surfaceType = testCase.metadata?.surface?.type || testCase.target_metadata?.type || testCase.type;
	if (intent === 'request-rest-route' || surfaceType === 'rest-route' || operation.route) {
		return 'rest';
	}
	if (intent.includes('admin-page') || surfaceType === 'admin-page') {
		return 'admin_page';
	}
	if (intent.includes('frontend') || surfaceType === 'frontend-url') {
		return 'frontend_page';
	}
	if (intent.includes('block') || surfaceType === 'block' || operation.block_name || operation.blockName) {
		return 'block';
	}
	if (intent.includes('database') || intent.includes('db-query') || ['database-table', 'db-query'].includes(surfaceType) || operation.table || operation.query || operation.statement) {
		return 'database';
	}
	return undefined;
}

function extraCapabilitiesForCase(testCase = {}, family) {
	if (family === 'block' && testCase.intent === 'insert-block-in-editor') {
		return ['browser', 'block-editor'];
	}
	if (family === 'database' && /profile|query|observe/.test(String(testCase.intent || ''))) {
		return ['query-observation'];
	}
	return [];
}

function runtimeOperationInput(testCase = {}, { family } = {}) {
	const operation = objectOrUndefined(testCase.operation) || {};
	if (family === 'crud') {
		return operation;
	}
	if (family === 'rest') {
		return stripUndefined({ method: operation.method || 'GET', route: operation.route || operation.path, route_params: operation.route_params, request_body: operation.request_body });
	}
	if (family === 'admin_page' || family === 'frontend_page') {
		return stripUndefined({ path: operation.path || operation.url, method: operation.method || 'GET', interaction: operation.interaction });
	}
	if (family === 'block') {
		return stripUndefined({ block_name: operation.block_name || operation.blockName || operation.name, lifecycle: operation.lifecycle, attributes: operation.attributes_sample || operation.attributes });
	}
	if (family === 'database') {
		return stripUndefined({ table: operation.table, query: operation.query, statement: operation.statement, mutation: operation.mutation, observation: operation.observation || operation.profile });
	}
	return operation;
}

function runtimeCapabilitiesWereProvided(value) {
	return value !== undefined && value !== null;
}

function missingRuntimeCapabilities(requiredCapabilities, runtimeCapabilities) {
	const declared = new Set(normalizeWordPressFuzzRuntimeCapabilities(runtimeCapabilities).capabilities);
	return requiredCapabilities.filter((capability) => !declared.has(capability));
}

function mergeCapabilities(...values) {
	return [...new Set(values.flatMap((value) => normalizeArray(value)).filter(Boolean))].sort();
}

function normalizeArray(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return Array.isArray(value) ? value : [value];
}

function reasonList(value) {
	return [...new Set(normalizeArray(value).map((item) => String(item || '').trim()).filter(Boolean))].sort();
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA,
	attachWordPressFuzzRuntimeWorkloadOperationDescriptor,
	buildWordPressFuzzRuntimeWorkloadOperationDescriptor,
	requiredCapabilitiesForWordPressFuzzRuntimeOperation,
};
