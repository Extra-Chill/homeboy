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
const WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA = 'homeboy/wordpress-fuzz-runtime-workload-operation-validation/v1';

const FAMILY_COMMANDS = Object.freeze({
	crud: 'wordpress.crud',
	rest: 'wordpress.request-rest-route',
	admin_page: 'wordpress.load-admin-page',
	frontend_page: 'wordpress.load-frontend-page',
	block: 'wordpress.exercise-block',
	database: 'wordpress.profile-database',
});

const WP_CODEBOX_WORKLOAD_COMMAND = 'run-wordpress-workload';
const WP_CODEBOX_WORKLOAD_ABILITY = 'wp-codebox/run-wordpress-workload';

const FAMILY_REQUIRED_CAPABILITIES = Object.freeze({
	crud: Object.freeze(['crud']),
	rest: Object.freeze(['rest']),
	admin_page: Object.freeze(['admin']),
	frontend_page: Object.freeze(['browser']),
	block: Object.freeze(['block']),
	database: Object.freeze(['database']),
});

function buildWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase = {}, options = {}) {
	if (testCase.input?.type === 'random_walk' || testCase.operation?.runtime_action === 'random_walk') {
		return undefined;
	}
	const family = runtimeOperationFamily(testCase);
	if (!family) {
		return undefined;
	}

	const requiredCapabilities = requiredCapabilitiesForWordPressFuzzRuntimeOperation(testCase, { family });
	const runtimeCapabilitiesProvided = runtimeCapabilitiesWereProvided(options.runtimeCapabilities ?? options.runtime_capabilities);
	const missingCapabilities = runtimeCapabilitiesProvided
		? missingRuntimeCapabilities(requiredCapabilities, options.runtimeCapabilities ?? options.runtime_capabilities)
		: [];
	const readinessBlocker = runtimeReadinessBlockerForOperation(family, testCase, options.runtimeReadiness ?? options.runtime_readiness);
	const mutationLifecycle = normalizeWordPressFuzzMutationLifecycleContract(testCase.metadata?.mutation_lifecycle || testCase.metadata?.mutationLifecycle || testCase.mutation_lifecycle || testCase.mutationLifecycle) || {};
	const input = runtimeOperationInput(testCase, { family });
	const validation = validateWordPressFuzzRuntimeWorkloadOperationPayload({ family, input });
	const blockers = [
		...validation.diagnostics.map((diagnostic) => ({ ...diagnostic, blocker: true })),
		...missingCapabilities.map((capability) => ({
			code: 'missing-runtime-workload-capability',
			message: `Runtime workload operation requires capability: ${capability}`,
			capability,
			blocker: true,
		})),
		...(readinessBlocker ? [readinessBlocker] : []),
	];
	const status = validation.ok ? (readinessBlocker?.blocking ? 'blocked' : (missingCapabilities.length > 0 || readinessBlocker ? 'skipped' : 'ready')) : 'blocked';

	return stripUndefined({
		schema: WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA,
		id: testCase.operation_id || testCase.operationId || testCase.id,
		case_id: testCase.case_id || testCase.caseId || testCase.id,
		family,
		command: FAMILY_COMMANDS[family],
		wp_codebox_command: WP_CODEBOX_WORKLOAD_COMMAND,
		wp_codebox_ability: WP_CODEBOX_WORKLOAD_ABILITY,
		status,
		required_capabilities: requiredCapabilities,
		missing_capabilities: missingCapabilities.length > 0 ? missingCapabilities : undefined,
		skip_reason: readinessBlocker?.skip_reason || (missingCapabilities.length > 0 ? 'missing-runtime-workload-capability' : undefined),
		blockers: blockers.length > 0 ? blockers : undefined,
		validation,
		input,
		mutation_lifecycle: mutationLifecycle,
		metadata: stripUndefined({
			intent: testCase.intent,
			operation_id: testCase.operation_id || testCase.operationId,
			wp_codebox_command: WP_CODEBOX_WORKLOAD_COMMAND,
			wp_codebox_ability: WP_CODEBOX_WORKLOAD_ABILITY,
			mutation_lifecycle: mutationLifecycle,
		}),
	});
}

function runtimeReadinessBlockerForOperation(family, testCase = {}, readiness) {
	const source = objectOrUndefined(readiness);
	if (!source) {
		return undefined;
	}
	if (source.command_available === false) {
		return {
			code: 'wp-codebox-fuzz-readiness-command-unavailable',
			message: 'WP Codebox fuzz readiness command is unavailable; runtime workload operation support cannot be claimed.',
			skip_reason: 'wp-codebox-fuzz-readiness-command-unavailable',
			blocking: true,
			blocker: true,
		};
	}
	const operationKinds = normalizeArray(source.operationKinds || source.operation_kinds);
	if (operationKinds.length === 0) {
		return undefined;
	}
	const operationKind = readinessOperationKindForFamily(family, testCase);
	if (!operationKinds.includes(operationKind)) {
		return {
			code: 'unsupported-runtime-workload-operation-kind',
			message: `WP Codebox fuzz readiness does not support ${operationKind} operations for ${family}.`,
			operation_kind: operationKind,
			supported_operation_kinds: operationKinds,
			skip_reason: 'unsupported-runtime-workload-operation-kind',
			blocker: true,
		};
	}
	if (operationKind === 'mutation' && !readinessSupportsMutationIsolation(source)) {
		return {
			code: 'wp-codebox-fuzz-mutation-isolation-unsupported',
			message: 'WP Codebox fuzz readiness does not declare mutation isolation support; mutating REST operations cannot execute.',
			operation_kind: operationKind,
			skip_reason: 'wp-codebox-fuzz-mutation-isolation-unsupported',
			blocking: true,
			blocker: true,
		};
	}
	if (operationKind === 'mutation' && restDeleteBoundaryRequired(testCase) && !readinessSupportsDeleteBoundary(source)) {
		return {
			code: 'wp-codebox-fuzz-delete-boundary-unsupported',
			message: 'WP Codebox fuzz readiness does not declare delete-boundary support; DELETE mutation cases cannot execute.',
			operation_kind: operationKind,
			skip_reason: 'wp-codebox-fuzz-delete-boundary-unsupported',
			blocking: true,
			blocker: true,
		};
	}
	if (source.status === 'unsupported') {
		return {
			code: 'wp-codebox-fuzz-readiness-unsupported',
			message: 'WP Codebox fuzz readiness reports this runner as unsupported for the requested fuzz contract.',
			skip_reason: 'wp-codebox-fuzz-readiness-unsupported',
			blocker: true,
		};
	}
	return undefined;
}

function readinessSupportsMutationIsolation(readiness = {}) {
	const capabilities = objectOrUndefined(readiness.capabilities) || {};
	const mutation = objectOrUndefined(readiness.mutation) || objectOrUndefined(capabilities.mutation) || {};
	const isolation = objectOrUndefined(readiness.isolation) || objectOrUndefined(capabilities.isolation) || {};
	return readiness.mutationIsolation === true
		|| readiness.mutation_isolation === true
		|| capabilities.mutationIsolation === true
		|| capabilities.mutation_isolation === true
		|| mutation.isolated === true
		|| mutation.isolation === true
		|| isolation.mutation === true
		|| isolation.checkpoint === true
		|| isolation.snapshot === true;
}

function restDeleteBoundaryRequired(testCase = {}) {
	const mutationLifecycle = normalizeWordPressFuzzMutationLifecycleContract(testCase.metadata?.mutation_lifecycle || testCase.metadata?.mutationLifecycle || testCase.mutation_lifecycle || testCase.mutationLifecycle) || {};
	return mutationLifecycle.delete_boundary_required === true || String(testCase.operation?.method || '').toUpperCase() === 'DELETE';
}

function readinessSupportsDeleteBoundary(readiness = {}) {
	const capabilities = objectOrUndefined(readiness.capabilities) || {};
	const mutation = objectOrUndefined(readiness.mutation) || objectOrUndefined(capabilities.mutation) || {};
	const deleteBoundary = objectOrUndefined(readiness.delete_boundary) || objectOrUndefined(readiness.deleteBoundary) || objectOrUndefined(capabilities.delete_boundary) || objectOrUndefined(capabilities.deleteBoundary) || {};
	return readiness.deleteBoundary === true
		|| readiness.delete_boundary === true
		|| capabilities.deleteBoundary === true
		|| capabilities.delete_boundary === true
		|| mutation.delete_boundary === true
		|| mutation.deleteBoundary === true
		|| deleteBoundary.supported === true;
}

function readinessOperationKindForFamily(family, testCase = {}) {
	if (family === 'crud') {
		return 'crud';
	}
	const mutationLifecycle = normalizeWordPressFuzzMutationLifecycleContract(testCase.metadata?.mutation_lifecycle || testCase.metadata?.mutationLifecycle || testCase.mutation_lifecycle || testCase.mutationLifecycle) || {};
	const destructiveReasons = reasonList(testCase.destructive_reasons || testCase.destructiveReasons || testCase.destructive_reason || testCase.destructiveReason);
	if (mutationLifecycle.delete_boundary_required || mutationLifecycle.required_capabilities?.length > 0 || destructiveReasons.length > 0 || String(testCase.intent || '').includes('mutate') || ['POST', 'PUT', 'PATCH', 'DELETE'].includes(String(testCase.operation?.method || '').toUpperCase())) {
		return 'mutation';
	}
	return 'read';
}

function attachWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase = {}, options = {}) {
	const runtimeOperation = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase, options);
	if (!runtimeOperation) {
		return testCase;
	}

	const metadata = objectOrUndefined(testCase.metadata) || {};
	if (runtimeOperation.status === 'ready') {
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
	const operationSkipReasons = runtimeOperation.status === 'blocked'
		? ['invalid-runtime-workload-operation']
		: [runtimeOperation.skip_reason];
	return stripUndefined({
		...testCase,
		executable: false,
		execution_tier: 'plan_only',
		required_capabilities: requiredCapabilities,
		skip_reasons: reasonList([...skipReasons, ...operationSkipReasons]),
		runtime_operation: runtimeOperation,
		metadata: stripUndefined({
			...metadata,
			executable: false,
			execution_tier: 'plan_only',
			planned: true,
			gated: true,
			runtime_workload_capability_gated: runtimeOperation.missing_capabilities?.length > 0,
			missing_runtime_workload_capabilities: runtimeOperation.missing_capabilities,
			runtime_workload_operation_blocked: runtimeOperation.status === 'blocked' || undefined,
			runtime_workload_operation_validation: runtimeOperation.validation,
			runtime_operation: runtimeOperation,
		}),
	});
}

function validateWordPressFuzzRuntimeWorkloadOperationPayload(operation = {}) {
	const family = operation.family;
	const input = objectOrUndefined(operation.input) || {};
	const diagnostics = [];
	if (!FAMILY_COMMANDS[family]) {
		diagnostics.push(validationDiagnostic('unsupported-runtime-workload-operation-family', `Unsupported runtime workload operation family: ${family || '(missing)'}`, 'family'));
	}
	for (const field of requiredInputFieldsForFamily(family, input)) {
		if (input[field] === undefined || input[field] === null || String(input[field]).trim() === '') {
			diagnostics.push(validationDiagnostic('missing-runtime-workload-operation-field', `Runtime workload operation ${family} requires input.${field}.`, `input.${field}`));
		}
	}
	return {
		schema: WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA,
		ok: diagnostics.length === 0,
		family,
		diagnostics,
	};
}

function validateWordPressFuzzRuntimeWorkloadOperationDescriptor(descriptor = {}) {
	if (!objectOrUndefined(descriptor)) {
		return {
			schema: WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA,
			ok: false,
			diagnostics: [validationDiagnostic('runtime-workload-operation-must-be-object', 'Runtime workload operation descriptor must be an object.', '')],
		};
	}
	return validateWordPressFuzzRuntimeWorkloadOperationPayload({ family: descriptor.family, input: descriptor.input });
}

function requiredInputFieldsForFamily(family, input = {}) {
	if (family === 'crud') {
		return ['action', 'resource_type'];
	}
	if (family === 'rest') {
		return ['route'];
	}
	if (family === 'admin_page' || family === 'frontend_page') {
		return ['path'];
	}
	if (family === 'block') {
		return ['block_name'];
	}
	if (family === 'database') {
		return input.table || input.query || input.statement ? [] : ['query'];
	}
	return [];
}

function validationDiagnostic(code, message, path) {
	return stripUndefined({ code, message, path });
}

function summarizeWordPressFuzzRuntimeWorkloadOperations(plan = {}) {
	const operations = normalizeArray(plan.targets).flatMap((target) => normalizeArray(target.cases).map((testCase) => testCase.runtime_operation).filter(Boolean));
	const byStatus = {};
	const byFamily = {};
	const byFamilyStatus = {};
	const blockers = [];
	for (const operation of operations) {
		byStatus[operation.status] = (byStatus[operation.status] || 0) + 1;
		byFamily[operation.family] = (byFamily[operation.family] || 0) + 1;
		byFamilyStatus[operation.family] = byFamilyStatus[operation.family] || {};
		byFamilyStatus[operation.family][operation.status] = (byFamilyStatus[operation.family][operation.status] || 0) + 1;
		for (const blocker of normalizeArray(operation.blockers)) {
			blockers.push(stripUndefined({ operation_id: operation.id, case_id: operation.case_id, family: operation.family, ...blocker }));
		}
	}
	return {
		schema: 'homeboy/wordpress-fuzz-runtime-workload-operation-summary/v1',
		total: operations.length,
		by_status: byStatus,
		by_family: byFamily,
		by_family_status: byFamilyStatus,
		blockers,
	};
}

function requiredCapabilitiesForWordPressFuzzRuntimeOperation(testCase = {}, options = {}) {
	const family = options.family || runtimeOperationFamily(testCase);
	const base = FAMILY_REQUIRED_CAPABILITIES[family] || [];
	const mutationLifecycle = normalizeWordPressFuzzMutationLifecycleContract(testCase.metadata?.mutation_lifecycle || testCase.metadata?.mutationLifecycle || testCase.mutation_lifecycle || testCase.mutationLifecycle) || {};
	return mergeCapabilities(base, extraCapabilitiesForCase(testCase, family), mutationLifecycle.required_capabilities, testCase.required_capabilities || testCase.requiredCapabilities);
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
	const surface = objectOrUndefined(testCase.metadata?.surface || testCase.target_metadata?.surface || testCase.target_metadata) || {};
	if (family === 'crud') {
		return operation;
	}
	if (family === 'rest') {
		return stripUndefined({ method: operation.method || surface.method || 'GET', route: operation.route || operation.path || surface.route || surface.path || surface.metadata?.value, route_params: operation.route_params, query_params: operation.query_params, request_body: operation.request_body });
	}
	if (family === 'admin_page') {
		const metadata = objectOrUndefined(testCase.metadata) || {};
		const interaction = objectOrUndefined(metadata.interaction) || {};
		return stripUndefined({
			path: operation.path || operation.url || surface.path || surface.url || surface.metadata?.value,
			method: operation.method || interaction.method || surface.method || 'GET',
			interaction_kind: operation.interaction_kind || interaction.kind,
			interaction_id: operation.interaction_id || interaction.id || interaction.name || interaction.selector || interaction.action,
			selector: operation.selector || interaction.selector,
			action: operation.action || interaction.action,
			fields: operation.fields || interaction.fields,
			capability_context: objectOrUndefined(metadata.capability_context || metadata.capabilityContext),
			nonce_context: objectOrUndefined(metadata.nonce_context || metadata.nonceContext),
			safety: objectOrUndefined(metadata.safety),
		});
	}
	if (family === 'frontend_page') {
		return stripUndefined({ path: operation.path || operation.url || surface.path || surface.url || surface.metadata?.value, method: operation.method || surface.method || 'GET', interaction: operation.interaction });
	}
	if (family === 'block') {
		return stripUndefined({ block_name: operation.block_name || operation.blockName || operation.name || surface.block_name || surface.blockName || surface.name, lifecycle: operation.lifecycle, attributes: operation.attributes_sample || operation.attributes || surface.attributes_sample || surface.attributes });
	}
	if (family === 'database') {
		return stripUndefined({ table: operation.table || surface.table, query: operation.query || surface.query, statement: operation.statement || surface.statement, mutation: operation.mutation, where: operation.where, values: operation.values, columns: operation.columns, limit: operation.limit, options: operation.options, observation: operation.observation || operation.profile });
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
	WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA,
	attachWordPressFuzzRuntimeWorkloadOperationDescriptor,
	buildWordPressFuzzRuntimeWorkloadOperationDescriptor,
	requiredCapabilitiesForWordPressFuzzRuntimeOperation,
	summarizeWordPressFuzzRuntimeWorkloadOperations,
	validateWordPressFuzzRuntimeWorkloadOperationDescriptor,
	validateWordPressFuzzRuntimeWorkloadOperationPayload,
};
