'use strict';

/**
 * Internal dependencies
 */
const {
	WP_CODEBOX_RUNTIME_ACTION_CONTRACT_FIELDS,
	wpCodeboxRuntimeActionTarget,
} = require('./wordpress-fuzz-runtime-action-contracts');
const {
	normalizeWordPressFuzzRuntimeCapabilities,
} = require('./wordpress-fuzz-runtime-capabilities');
const {
	normalizeWordPressFuzzMutationLifecycleContract,
} = require('./wordpress-fuzz-mutation-lifecycle');

const WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA = 'homeboy/wordpress-fuzz-runtime-workload-operation/v1';
const WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA = 'homeboy/wordpress-fuzz-runtime-workload-operation-validation/v1';

const ACTION_COMMANDS = Object.freeze({
	rest_request: 'wordpress.rest-request',
	crud_operation: 'wordpress.crud-operation',
	db_operation: 'wordpress.db-operation',
	admin_page_load: 'wordpress.admin-page-load',
	admin_action: undefined,
	ajax_action: undefined,
	admin_post: undefined,
	frontend_page_load: 'wordpress.frontend-page-load',
	block_render: 'wordpress.block-render',
	block_editor: 'wordpress.block-editor',
	db_query: 'wordpress.db-query',
	wp_cli: 'wordpress.wp-cli',
	action_auth: undefined,
	login_as: 'wordpress.login-as',
	nonce_for: 'wordpress.nonce-for',
	nonce: undefined,
	session: undefined,
	checkpoint: 'wordpress.checkpoint',
	restore: 'wordpress.restore',
	reset_state: 'wordpress.reset-state',
	replay_case: 'wordpress.replay-case',
	minimize_case: 'wordpress.minimize-case',
});

const FAMILY_REQUIRED_CAPABILITIES = Object.freeze({
	crud: Object.freeze(['crud']),
	rest: Object.freeze(['rest']),
	admin_page: Object.freeze(['admin']),
	frontend_page: Object.freeze(['browser']),
	block: Object.freeze(['block']),
	database: Object.freeze(['database']),
	sequence: Object.freeze(['sequence']),
});

const ACTION_REQUIRED_CAPABILITIES = Object.freeze({
	rest_request: Object.freeze(['rest']),
	crud_operation: Object.freeze(['crud']),
	admin_page_load: Object.freeze(['admin']),
	admin_action: Object.freeze(['admin']),
	ajax_action: Object.freeze(['admin']),
	admin_post: Object.freeze(['admin']),
	frontend_page_load: Object.freeze(['browser']),
	block_render: Object.freeze(['block']),
	block_editor: Object.freeze(['block', 'block-editor', 'browser']),
	db_query: Object.freeze(['database']),
	db_operation: Object.freeze(['database']),
	wp_cli: Object.freeze(['wp-cli']),
	action_auth: Object.freeze(['admin']),
	login_as: Object.freeze(['admin']),
	nonce_for: Object.freeze(['admin']),
	nonce: Object.freeze(['admin']),
	session: Object.freeze(['admin']),
	checkpoint: Object.freeze(['checkpoint']),
	restore: Object.freeze(['restore']),
	reset_state: Object.freeze(['reset']),
	replay_case: Object.freeze(['sequence']),
	minimize_case: Object.freeze(['sequence']),
});

function buildWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase = {}, options = {}) {
	if (testCase.input?.type === 'random_walk' || testCase.operation?.runtime_action === 'random_walk') {
		return undefined;
	}
	const family = runtimeOperationFamily(testCase);
	const action = runtimeOperationAction(testCase, { family });
	if (!family && !action) {
		return undefined;
	}
	const operationFamily = family || familyForRuntimeOperationAction(action);

	const requiredCapabilities = requiredCapabilitiesForWordPressFuzzRuntimeOperation(testCase, { family: operationFamily, action });
	const runtimeCapabilitiesProvided = runtimeCapabilitiesWereProvided(options.runtimeCapabilities ?? options.runtime_capabilities);
	const missingCapabilities = runtimeCapabilitiesProvided
		? missingRuntimeCapabilities(requiredCapabilities, options.runtimeCapabilities ?? options.runtime_capabilities)
		: [];
	const readinessBlocker = runtimeReadinessBlockerForOperation(operationFamily, testCase, options.runtimeReadiness ?? options.runtime_readiness);
	const mutationLifecycle = normalizeWordPressFuzzMutationLifecycleContract(testCase.metadata?.mutation_lifecycle || testCase.metadata?.mutationLifecycle || testCase.mutation_lifecycle || testCase.mutationLifecycle) || {};
	const input = runtimeOperationInput(testCase, { family: operationFamily, action });
	const validation = validateWordPressFuzzRuntimeWorkloadOperationPayload({ family: operationFamily, action, input });
	const contractMapping = mapWordPressRuntimeActionToCodeboxContract(action, options.codeboxRuntimeContracts || options.codebox_runtime_contracts || options.wpCodeboxRuntimeContracts || options.wp_codebox_runtime_contracts, { testCase, family: operationFamily, input });
	const blockers = [
		...validation.diagnostics.map((diagnostic) => ({ ...diagnostic, blocker: true })),
		...(contractMapping.blocker ? [contractMapping.blocker] : []),
		...missingCapabilities.map((capability) => ({
			code: 'missing-runtime-workload-capability',
			message: `Runtime workload operation requires capability: ${capability}`,
			capability,
			blocker: true,
		})),
		...(readinessBlocker ? [readinessBlocker] : []),
	];
	const status = runtimeOperationStatus({ validation, contractMapping, readinessBlocker, missingCapabilities });

	return stripUndefined({
		schema: WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA,
		id: testCase.operation_id || testCase.operationId || testCase.id,
		case_id: testCase.case_id || testCase.caseId || testCase.id,
		family: operationFamily,
		action,
		command: ACTION_COMMANDS[action],
		target: contractMapping.target,
		wp_codebox_action: contractMapping.action,
		wp_codebox_command: contractMapping.command,
		wp_codebox_ability: contractMapping.ability,
		wp_codebox_input_schema: contractMapping.input_schema,
		wp_codebox_output_schema: contractMapping.output_schema,
		wp_codebox_contract_schema: contractMapping.contract_schema,
		wp_codebox_mutation_contract_schema: contractMapping.mutation_contract_schema,
		status,
		required_capabilities: requiredCapabilities,
		missing_capabilities: missingCapabilities.length > 0 ? missingCapabilities : undefined,
		skip_reason: contractMapping.blocker?.skip_reason || readinessBlocker?.skip_reason || (missingCapabilities.length > 0 ? 'missing-runtime-workload-capability' : undefined),
		blockers: blockers.length > 0 ? blockers : undefined,
		validation,
		input,
		mutation_lifecycle: mutationLifecycle,
		metadata: stripUndefined({
			intent: testCase.intent,
			operation_id: testCase.operation_id || testCase.operationId,
			wp_codebox_action: contractMapping.action,
			wp_codebox_command: contractMapping.command,
			wp_codebox_ability: contractMapping.ability,
			wp_codebox_contract_schema: contractMapping.contract_schema,
			wp_codebox_mutation_contract_schema: contractMapping.mutation_contract_schema,
			sequence_plan: testCase.metadata?.sequence_plan || testCase.metadata?.sequencePlan || testCase.metadata?.sequence,
			exploration: testCase.metadata?.exploration,
			mutation_lifecycle: mutationLifecycle,
		}),
	});
}

function runtimeOperationStatus({ validation, contractMapping, readinessBlocker, missingCapabilities }) {
	if (!validation.ok || contractMapping.blocker) {
		return 'blocked';
	}
	if (readinessBlocker?.status) {
		return readinessBlocker.status;
	}
	if (readinessBlocker?.blocking) {
		return 'blocked';
	}
	if (missingCapabilities.length > 0 || readinessBlocker) {
		return 'planned';
	}
	return 'ready';
}

function mapWordPressRuntimeActionToCodeboxContract(action, contracts, options = {}) {
	if (!action) {
		return { blocker: unsupportedRuntimeActionBlocker(action) };
	}
	const actionContracts = normalizeCodeboxRuntimeActionContracts(contracts);
	if (!actionContracts) {
		return {
			blocker: {
				code: 'wp-codebox-runtime-action-contracts-missing',
				message: 'WP Codebox public runtime action contract descriptor is missing; Homeboy will not guess Codebox commands or abilities.',
				action,
				skip_reason: 'wp-codebox-runtime-action-contracts-missing',
				blocking: true,
				blocker: true,
			},
		};
	}
	const descriptor = actionContracts.get(action);
	if (!descriptor) {
		return { blocker: unsupportedRuntimeActionBlocker(action, [...actionContracts.keys()], missingContractFieldsForAction(action)) };
	}
	const mutationContract = mutationContractForAction(action, contracts, options);
	if (mutationContract.blocker) {
		return { blocker: mutationContract.blocker };
	}
	return stripUndefined({
		action,
		command: descriptor.command,
		ability: descriptor.ability,
		input_schema: descriptor.input_schema || descriptor.inputSchema || descriptor.request_schema || descriptor.requestSchema,
		output_schema: descriptor.output_schema || descriptor.outputSchema || descriptor.result_schema || descriptor.resultSchema,
		contract_schema: descriptor.schema,
		mutation_contract_schema: mutationContract.schema,
		target: wpCodeboxRuntimeActionTarget(action),
	});
}

function normalizeCodeboxRuntimeActionContracts(contracts) {
	const source = objectOrUndefined(contracts);
	if (!source) {
		return null;
	}
	const explicitDescriptors = explicitRuntimeActionDescriptors(source) || {};
	const contractEntries = source.actions || source.runtime_actions || source.wordpress_runtime_actions || source.operations || source.workload_operations || source;
	const entries = Array.isArray(contractEntries) ? contractEntries : [];
	const pairs = entries.length > 0
		? entries.map((entry) => [normalizeRuntimeAction(entry.action || entry.type || entry.id || entry.name), entry])
		: Object.entries({ ...explicitDescriptors, ...contractEntries }).map(([key, entry]) => [normalizeRuntimeAction(entry?.action || entry?.type || entry?.id || key), entry]);
	const map = new Map();
	for (const [key, entry] of pairs) {
		if (key && objectOrUndefined(entry)) {
			map.set(key, entry);
		}
	}
	return map;
}

function explicitRuntimeActionDescriptors(source = {}) {
	const entries = Object.entries(WP_CODEBOX_RUNTIME_ACTION_CONTRACT_FIELDS)
		.map(([action, paths]) => [action, descriptorFromExplicitPaths(source, paths, action)])
		.filter(([, descriptor]) => descriptor);
	return entries.length > 0 ? Object.fromEntries(entries) : undefined;
}

function descriptorFromExplicitPaths(source, paths, action) {
	for (const path of paths) {
		const value = valueAtPath(source, path);
		if (objectOrUndefined(value)) {
			return { action, ...value };
		}
		if (typeof value === 'string') {
			return { action, schema: value };
		}
	}
	return undefined;
}

function missingContractFieldsForAction(action) {
	return WP_CODEBOX_RUNTIME_ACTION_CONTRACT_FIELDS[action] || [`actions.${action}`];
}

function unsupportedRuntimeActionBlocker(action, supportedActions = [], expectedFields = []) {
	return stripUndefined({
		code: 'unsupported-wordpress-runtime-action',
		message: `WP Codebox public runtime action contract does not declare ${action || '(missing)'} support.`,
		action,
		supported_actions: supportedActions.length > 0 ? supportedActions.sort() : undefined,
		missing_contract_fields: expectedFields.length > 0 ? expectedFields : undefined,
		skip_reason: 'unsupported-wordpress-runtime-action',
		blocking: true,
		blocker: true,
	});
}

function mutationContractForAction(action, contracts, options = {}) {
	if (!runtimeActionMutates(action, options.testCase, options.input)) {
		return {};
	}
	const paths = mutationContractFieldsForAction(action);
	for (const path of paths) {
		const value = valueAtPath(contracts, path);
		if (typeof value === 'string') {
			return { schema: value };
		}
		if (objectOrUndefined(value)) {
			return { schema: value.schema || value.id || value.$id };
		}
	}
	return {
		blocker: {
			code: 'wp-codebox-runtime-mutation-contract-missing',
			message: `WP Codebox public runtime contract is missing mutation contract for ${action}.`,
			action,
			missing_contract_fields: paths,
			skip_reason: 'wp-codebox-runtime-mutation-contract-missing',
			blocking: true,
			blocker: true,
		},
	};
}

function mutationContractFieldsForAction(action) {
	return {
		rest_request: ['schemas.wordpressRuntime.disposableMutation', 'schemas.wordpressRuntime.restMutation', 'mutationContracts.rest_request'],
		crud_operation: ['schemas.wordpressRuntime.disposableMutation', 'schemas.wordpressRuntime.crudMutation', 'mutationContracts.crud_operation'],
		db_operation: ['schemas.wordpressDb.mutation', 'schemas.wordpressRuntime.disposableMutation', 'mutationContracts.db_operation'],
		admin_action: ['schemas.wordpressRuntime.disposableMutation', 'schemas.wordpressRuntime.adminActionMutation', 'mutationContracts.admin_action'],
		ajax_action: ['schemas.wordpressRuntime.disposableMutation', 'schemas.wordpressRuntime.ajaxActionMutation', 'mutationContracts.ajax_action'],
		admin_post: ['schemas.wordpressRuntime.disposableMutation', 'schemas.wordpressRuntime.adminPostMutation', 'mutationContracts.admin_post'],
	}[action] || ['schemas.wordpressRuntime.disposableMutation', `mutationContracts.${action}`];
}

function runtimeActionMutates(action, testCase = {}, input = {}) {
	if (action === 'crud_operation') {
		return ['create', 'update', 'delete'].includes(String(input.action || testCase.operation?.action || '').toLowerCase());
	}
	if (action === 'db_operation') {
		return String(input.operation || testCase.operation?.operation || '').toLowerCase() === 'write' || Boolean(testCase.operation?.mutation || testCase.operation?.statement);
	}
	if (['admin_action', 'ajax_action', 'admin_post'].includes(action)) {
		return true;
	}
	if (action === 'rest_request') {
		return ['POST', 'PUT', 'PATCH', 'DELETE'].includes(String(input.method || testCase.operation?.method || '').toUpperCase());
	}
	const mutationLifecycle = normalizeWordPressFuzzMutationLifecycleContract(testCase.metadata?.mutation_lifecycle || testCase.metadata?.mutationLifecycle || testCase.mutation_lifecycle || testCase.mutationLifecycle) || {};
	return mutationLifecycle.required_capabilities?.length > 0 || reasonList(testCase.destructive_reasons || testCase.destructiveReasons).length > 0;
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
	if (operationKind === 'mutation' && !hasLiveWordPressFuzzReadinessEvidence(source)) {
		return {
			code: 'wp-codebox-fuzz-live-readiness-required',
			message: 'Mutating runtime workload operations require live WP Codebox readiness evidence before executable status can be claimed.',
			operation_kind: operationKind,
			skip_reason: 'wp-codebox-fuzz-live-readiness-required',
			status: 'planned',
			blocker: true,
		};
	}
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

function hasLiveWordPressFuzzReadinessEvidence(readiness = {}) {
	return readiness.schema === 'wp-codebox/fuzz-runner-readiness/v1'
		&& ['ready', 'blocked', 'unsupported'].includes(String(readiness.status || ''));
}

function readinessSupportsMutationIsolation(readiness = {}) {
	const capabilities = objectOrUndefined(readiness.capabilities) || {};
	const mutation = objectOrUndefined(readiness.mutation) || objectOrUndefined(capabilities.mutation) || {};
	const isolation = objectOrUndefined(readiness.isolation) || objectOrUndefined(capabilities.isolation) || {};
	return readiness.mutationIsolation === true
		|| readiness.mutation_isolation === true
		|| readiness.disposable === true
		|| capabilities.mutationIsolation === true
		|| capabilities.mutation_isolation === true
		|| capabilities.disposable === true
		|| mutation.isolated === true
		|| mutation.isolation === true
		|| mutation.disposable === true
		|| isolation.mutation === true
		|| isolation.disposable === true
		|| isolation.runtime_backed === true
		|| isolation.runtimeBacked === true
		|| isolation.checkpoint === true
		|| isolation.snapshot === true;
}

function readinessOperationKindForFamily(family, testCase = {}) {
	if (family === 'crud') {
		const action = String(testCase.operation?.action || testCase.metadata?.crud?.action || '').toLowerCase();
		if (['create', 'update', 'delete'].includes(action)) {
			return 'mutation';
		}
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
	const action = operation.action;
	const input = objectOrUndefined(operation.input) || {};
	const diagnostics = [];
	if (!Object.prototype.hasOwnProperty.call(ACTION_COMMANDS, action)) {
		diagnostics.push(validationDiagnostic('unsupported-runtime-workload-operation-action', `Unsupported runtime workload operation action: ${action || '(missing)'}`, 'action'));
	}
	if (family && !FAMILY_REQUIRED_CAPABILITIES[family]) {
		diagnostics.push(validationDiagnostic('unsupported-runtime-workload-operation-family', `Unsupported runtime workload operation family: ${family || '(missing)'}`, 'family'));
	}
	for (const field of requiredInputFieldsForAction(action, input)) {
		if (input[field] === undefined || input[field] === null || String(input[field]).trim() === '') {
			diagnostics.push(validationDiagnostic('missing-runtime-workload-operation-field', `Runtime workload operation ${action || family} requires input.${field}.`, `input.${field}`));
		}
	}
	return {
		schema: WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA,
		ok: diagnostics.length === 0,
		family,
		action,
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
	return validateWordPressFuzzRuntimeWorkloadOperationPayload({ family: descriptor.family, action: descriptor.action, input: descriptor.input });
}

function requiredInputFieldsForAction(action, input = {}) {
	if (action === 'rest_request') {
		return ['route'];
	}
	if (action === 'crud_operation') {
		return ['action', 'resource_type'];
	}
	if (action === 'db_operation') {
		return ['operation'];
	}
	if (action === 'admin_action' || action === 'ajax_action' || action === 'admin_post') {
		return input.action || input.fields?.action ? [] : ['action'];
	}
	if (action === 'action_auth') {
		return ['action'];
	}
	if (action === 'admin_page_load' || action === 'frontend_page_load') {
		return ['path'];
	}
	if (action === 'block_render' || action === 'block_editor') {
		return ['block_name'];
	}
	if (action === 'db_query') {
		return input.table || input.query || input.statement ? [] : ['query'];
	}
	if (action === 'wp_cli') {
		return ['args'];
	}
	if (action === 'login_as') {
		return ['user'];
	}
	if (action === 'nonce_for' || action === 'nonce') {
		return ['action'];
	}
	if (action === 'session') {
		return ['scope'];
	}
	if (action === 'restore') {
		return ['checkpoint_id'];
	}
	if (action === 'replay_case' || action === 'minimize_case') {
		return input.case || input.type || input.steps ? [] : ['case'];
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
	const action = options.action || runtimeOperationAction(testCase, { family });
	const base = ACTION_REQUIRED_CAPABILITIES[action] || FAMILY_REQUIRED_CAPABILITIES[family] || [];
	return mergeCapabilities(base, extraCapabilitiesForCase(testCase, family), testCase.required_capabilities || testCase.requiredCapabilities);
}

function runtimeOperationAction(testCase = {}, { family } = {}) {
	const operation = objectOrUndefined(testCase.operation) || {};
	const direct = normalizeRuntimeAction(operation.action_type || operation.actionType || operation.runtime_action || operation.runtimeAction || testCase.action || testCase.action_type || testCase.actionType || testCase.input?.action || testCase.input?.type);
	if (direct && Object.prototype.hasOwnProperty.call(ACTION_COMMANDS, direct)) {
		return direct;
	}
	const operationFamily = family || runtimeOperationFamily(testCase);
	if (operationFamily === 'rest') {
		return 'rest_request';
	}
	if (operationFamily === 'crud') {
		return 'crud_operation';
	}
	if (operationFamily === 'admin_page') {
		return adminRuntimeOperationAction(testCase) || 'admin_page_load';
	}
	if (operationFamily === 'frontend_page') {
		return 'frontend_page_load';
	}
	if (operationFamily === 'block') {
		return testCase.intent === 'insert-block-in-editor' || operation.lifecycle === 'editor' ? 'block_editor' : 'block_render';
	}
	if (operationFamily === 'database') {
		return databaseRuntimeOperationAction(testCase);
	}
	if (operationFamily === 'sequence') {
		return 'replay_case';
	}
	return undefined;
}

function adminRuntimeOperationAction(testCase = {}) {
	const operation = objectOrUndefined(testCase.operation) || {};
	const surface = objectOrUndefined(testCase.metadata?.surface || testCase.target_metadata?.surface || testCase.target_metadata) || {};
	const path = String(operation.action_path || operation.actionPath || operation.path || operation.url || surface.path || surface.url || '');
	if (surface.type === 'ajax-action' || path.includes('admin-ajax.php')) {
		return 'ajax_action';
	}
	if (path.includes('admin-post.php')) {
		return 'admin_post';
	}
	if (operation.interaction_kind || operation.interaction_id || operation.action || testCase.metadata?.interaction) {
		return 'admin_action';
	}
	return undefined;
}

function databaseRuntimeOperationAction(testCase = {}) {
	const operation = objectOrUndefined(testCase.operation) || {};
	if (operation.mutation || operation.statement || ['write', 'insert', 'update', 'delete'].includes(String(operation.operation || '').toLowerCase())) {
		return 'db_operation';
	}
	return 'db_query';
}

function normalizeRuntimeAction(value) {
	return String(value || '').trim().toLowerCase().replace(/[\s.-]+/g, '_');
}

function familyForRuntimeOperationAction(action) {
	if (action === 'rest_request') {
		return 'rest';
	}
	if (action === 'crud_operation') {
		return 'crud';
	}
	if (['admin_page_load', 'admin_action', 'ajax_action', 'admin_post', 'action_auth', 'login_as', 'nonce_for', 'nonce', 'session'].includes(action)) {
		return 'admin_page';
	}
	if (action === 'frontend_page_load') {
		return 'frontend_page';
	}
	if (action === 'block_render' || action === 'block_editor') {
		return 'block';
	}
	if (action === 'db_query' || action === 'db_operation' || action === 'wp_cli') {
		return 'database';
	}
	if (['checkpoint', 'restore', 'reset_state', 'replay_case', 'minimize_case'].includes(action)) {
		return 'sequence';
	}
	return undefined;
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
	if (intent.includes('admin-page') || surfaceType === 'admin-page' || surfaceType === 'ajax-action') {
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
	if (intent === 'stateful-sequence' || operation.runtime_action === 'stateful_sequence' || testCase.input?.type === 'stateful_sequence') {
		return 'sequence';
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

function runtimeOperationInput(testCase = {}, { family, action } = {}) {
	const operation = objectOrUndefined(testCase.operation) || {};
	const surface = objectOrUndefined(testCase.metadata?.surface || testCase.target_metadata?.surface || testCase.target_metadata) || {};
	if (action === 'wp_cli') {
		return stripUndefined({ args: operation.args || testCase.input?.args, env: operation.env || testCase.input?.env });
	}
	if (action === 'login_as') {
		return stripUndefined({ user: operation.user || operation.user_id || operation.userId || testCase.input?.user });
	}
	if (action === 'action_auth') {
		return stripUndefined({ action: operation.action || testCase.input?.action, session: operation.session || testCase.input?.session, nonce: operation.nonce || testCase.input?.nonce });
	}
	if (action === 'nonce_for' || action === 'nonce') {
		return stripUndefined({ action: operation.action || testCase.input?.action, user: operation.user || testCase.input?.user });
	}
	if (action === 'session') {
		return stripUndefined({ scope: operation.scope || testCase.input?.scope || 'admin', user: operation.user || testCase.input?.user });
	}
	if (action === 'checkpoint') {
		return stripUndefined({ label: operation.label || testCase.input?.label });
	}
	if (action === 'restore') {
		return stripUndefined({ checkpoint_id: operation.checkpoint_id || operation.checkpointId || testCase.input?.checkpoint_id || testCase.input?.checkpointId });
	}
	if (action === 'reset_state') {
		return stripUndefined({ scope: operation.scope || testCase.input?.scope });
	}
	if (action === 'replay_case' || action === 'minimize_case') {
		if (testCase.input?.type || testCase.input?.steps) {
			return testCase.input;
		}
		return stripUndefined({ case: operation.case || testCase.input?.case || testCase.case || testCase.id, options: operation.options || testCase.input?.options });
	}
	if (family === 'crud') {
		return operation;
	}
	if (family === 'rest') {
		return stripUndefined({ method: operation.method || surface.method || 'GET', route: operation.route || operation.path || surface.route || surface.path || surface.metadata?.value, route_params: operation.route_params, query_params: operation.query_params, request_body: operation.request_body });
	}
	if (['admin_action', 'ajax_action', 'admin_post'].includes(action)) {
		const metadata = objectOrUndefined(testCase.metadata) || {};
		const interaction = objectOrUndefined(metadata.interaction) || {};
		return stripUndefined({
			path: operation.action_path || operation.actionPath || operation.path || operation.url || surface.path || surface.url || surface.metadata?.value,
			method: operation.method || interaction.method || surface.method || 'GET',
			interaction_kind: operation.interaction_kind || interaction.kind,
			interaction_id: operation.interaction_id || interaction.id || interaction.name || interaction.selector || interaction.action,
			selector: operation.selector || interaction.selector,
			action: operation.action || interaction.action || interaction.id || interaction.name || surface.action || surface.hook || surface.name || surface.id,
			hook: surface.hook,
			fields: operation.fields || interaction.fields,
			input_descriptors: operation.input_descriptors || operation.inputDescriptors || interaction.input_descriptors || interaction.inputDescriptors,
			submit_controls: operation.submit_controls || operation.submitControls || interaction.submit_controls || interaction.submitControls,
			bulk_action: operation.bulk_action || operation.bulkAction || interaction.bulk_action || interaction.bulkAction,
			capability_context: objectOrUndefined(metadata.capability_context || metadata.capabilityContext || operation.capability_context || operation.capabilityContext || surface.capability_context || surface.capabilityContext),
			nonce_context: objectOrUndefined(metadata.nonce_context || metadata.nonceContext || operation.nonce_context || operation.nonceContext || surface.nonce_context || surface.nonceContext),
			session_context: objectOrUndefined(metadata.session_context || metadata.sessionContext),
			action_auth_context: objectOrUndefined(metadata.action_auth_context || metadata.actionAuthContext),
			safety: objectOrUndefined(metadata.safety),
		});
	}
	if (family === 'admin_page') {
		const metadata = objectOrUndefined(testCase.metadata) || {};
		return stripUndefined({
			path: operation.path || operation.url || surface.path || surface.url || surface.metadata?.value,
			method: operation.method || surface.method || 'GET',
			capability_context: objectOrUndefined(metadata.capability_context || metadata.capabilityContext),
		});
	}
	if (family === 'frontend_page') {
		return stripUndefined({ path: operation.path || operation.url || surface.path || surface.url || surface.metadata?.value, method: operation.method || surface.method || 'GET', interaction: operation.interaction });
	}
	if (family === 'block') {
		return stripUndefined({ block_name: operation.block_name || operation.blockName || operation.name || surface.block_name || surface.blockName || surface.name, lifecycle: operation.lifecycle, attributes: operation.attributes_sample || operation.attributes || surface.attributes_sample || surface.attributes });
	}
	if (family === 'database') {
		return stripUndefined({ operation: operation.operation || (operation.mutation || operation.statement ? 'write' : 'query-summary'), table: operation.table || surface.table, resource: operation.table || surface.table ? { table: operation.table || surface.table } : undefined, query: operation.query || operation.statement || surface.query ? { sql: operation.statement || operation.query || surface.query, table: operation.table || surface.table, columns: operation.columns, where: operation.where, limit: operation.limit } : undefined, mutation: operation.mutation, where: operation.where, values: operation.values, options: operation.options, observation: operation.observation || operation.profile });
	}
	if (family === 'sequence') {
		return testCase.input || operation;
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

function valueAtPath(value, pathName) {
	return String(pathName || '').split('.').reduce((current, part) => current?.[part], value);
}

function stripUndefined(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA,
	WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA,
	attachWordPressFuzzRuntimeWorkloadOperationDescriptor,
	buildWordPressFuzzRuntimeWorkloadOperationDescriptor,
	mapWordPressRuntimeActionToCodeboxContract,
	requiredCapabilitiesForWordPressFuzzRuntimeOperation,
	summarizeWordPressFuzzRuntimeWorkloadOperations,
	validateWordPressFuzzRuntimeWorkloadOperationDescriptor,
	validateWordPressFuzzRuntimeWorkloadOperationPayload,
};
