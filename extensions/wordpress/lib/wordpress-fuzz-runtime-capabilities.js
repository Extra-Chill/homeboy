'use strict';

const WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA = 'homeboy/wordpress-fuzz-runtime-capabilities/v1';
const WORDPRESS_FUZZ_MUTATION_POLICY_SCHEMA = 'homeboy/wordpress-fuzz-mutation-policy/v1';
const WORDPRESS_FUZZ_MUTATION_MODES = Object.freeze(['isolated', 'read_only', 'destructive-deny']);
const WORDPRESS_FUZZ_EXECUTION_TIERS = Object.freeze(['discovered', 'plan_only', 'read_only_executable', 'isolated_mutating_executable']);

const WORDPRESS_FUZZ_RUNTIME_CAPABILITIES = Object.freeze([
	'snapshot',
	'checkpoint',
	'restore',
	'rest-rollback',
	'transaction',
	'reset',
	'crud',
	'rest',
	'admin',
	'database',
	'browser',
]);

const WORDPRESS_FUZZ_RUNTIME_CAPABILITY_ALIASES = new Map([
	['snapshots', 'snapshot'],
	['snapshotting', 'snapshot'],
	['checkpointing', 'checkpoint'],
	['rollback', 'restore'],
	['rollback-snapshot', 'restore'],
	['rollback_snapshot', 'restore'],
	['rest-rollback-safe', 'rest-rollback'],
	['rest_rollback_safe', 'rest-rollback'],
	['rest-mutation-rollback', 'rest-rollback'],
	['rest_mutation_rollback', 'rest-rollback'],
	['rollback-safe-rest', 'rest-rollback'],
	['rollback_safe_rest', 'rest-rollback'],
	['db-transaction', 'transaction'],
	['db_transaction', 'transaction'],
	['transactions', 'transaction'],
	['reset-db', 'reset'],
	['reset_db', 'reset'],
	['database-reset', 'reset'],
	['database_reset', 'reset'],
	['crud-execution', 'crud'],
	['crud_execution', 'crud'],
	['rest-execution', 'rest'],
	['rest_execution', 'rest'],
	['admin-execution', 'admin'],
	['admin_execution', 'admin'],
	['db', 'database'],
	['db-execution', 'database'],
	['db_execution', 'database'],
	['database-execution', 'database'],
	['database_execution', 'database'],
	['browser-execution', 'browser'],
	['browser_execution', 'browser'],
]);

const WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SET = new Set(WORDPRESS_FUZZ_RUNTIME_CAPABILITIES);

const WORDPRESS_FUZZ_RUNTIME_CAPABILITY_REQUIREMENTS = Object.freeze({
	mutating_crud: Object.freeze(['crud', 'snapshot', 'restore', 'reset']),
	mutating_rest: Object.freeze(['rest', 'checkpoint', 'rest-rollback']),
	rest_crud_mutation: Object.freeze(['crud', 'rest', 'checkpoint', 'rest-rollback']),
	admin_mutation: Object.freeze(['admin', 'snapshot', 'restore', 'reset']),
	db_mutation: Object.freeze(['database', 'snapshot', 'transaction', 'reset']),
});

function normalizeWordPressFuzzRuntimeCapability(value) {
	const key = String(value || '').trim().toLowerCase().replace(/\s+/g, '-');
	return WORDPRESS_FUZZ_RUNTIME_CAPABILITY_ALIASES.get(key) || (WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SET.has(key) ? key : '');
}

function normalizeWordPressFuzzRuntimeCapabilities(value = {}) {
	const rawCapabilities = Array.isArray(value)
		? value
		: value.capabilities || value.runtime_capabilities || value.runtimeCapabilities || value.supports || [];
	const capabilities = [...new Set((Array.isArray(rawCapabilities) ? rawCapabilities : [rawCapabilities])
		.map(normalizeWordPressFuzzRuntimeCapability)
		.filter(Boolean))].sort();
	const capabilitySet = new Set(capabilities);

	return {
		schema: WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA,
		capabilities,
		isolation: normalizeIsolationContract(value, capabilitySet),
		execution: normalizeExecutionContract(value, capabilitySet),
		metadata: isObject(value.metadata) ? { ...value.metadata } : {},
	};
}

function normalizeIsolationContract(value = {}, capabilitySet = new Set()) {
	const isolation = isObject(value.isolation) ? value.isolation : {};
	return {
		snapshot: capabilityFlag(capabilitySet, isolation, 'snapshot'),
		checkpoint: capabilityFlag(capabilitySet, isolation, 'checkpoint'),
		restore: capabilityFlag(capabilitySet, isolation, 'restore'),
		transaction: capabilityFlag(capabilitySet, isolation, 'transaction'),
		reset: capabilityFlag(capabilitySet, isolation, 'reset'),
	};
}

function normalizeExecutionContract(value = {}, capabilitySet = new Set()) {
	const execution = isObject(value.execution) ? value.execution : {};
	return {
		crud: capabilityFlag(capabilitySet, execution, 'crud'),
		rest: capabilityFlag(capabilitySet, execution, 'rest'),
		admin: capabilityFlag(capabilitySet, execution, 'admin'),
		database: capabilityFlag(capabilitySet, execution, 'database'),
		browser: capabilityFlag(capabilitySet, execution, 'browser'),
	};
}

function capabilityFlag(capabilitySet, section, capability) {
	return section[capability] === true || capabilitySet.has(capability);
}

function gateWordPressFuzzCaseForRuntimeCapabilities(testCase, runtimeCapabilities, options = {}) {
	const policyGated = gateWordPressFuzzCaseForMutationPolicy(testCase, options);
	if (policyGated) {
		return policyGated;
	}

	const required = normalizeRequiredCapabilities(options.required_capabilities || options.requiredCapabilities || testCase.required_capabilities || testCase.requiredCapabilities);
	if (required.length === 0) {
		return testCase;
	}
	const declared = new Set(normalizeWordPressFuzzRuntimeCapabilities(runtimeCapabilities).capabilities);
	const requiredAny = normalizeRequiredCapabilityGroups(options.required_any_capabilities || options.requiredAnyCapabilities || testCase.required_any_capabilities || testCase.requiredAnyCapabilities);
	const missing = required.filter((capability) => !declared.has(capability));
	const missingAny = requiredAny.filter((group) => !group.some((capability) => declared.has(capability)));
	const skipReasons = reasonList(testCase.skip_reasons || testCase.skipReasons || testCase.skip_reason || testCase.skipReason);
	const metadata = isObject(testCase.metadata) ? { ...testCase.metadata } : {};

	if (missing.length === 0 && missingAny.length === 0) {
		const mutates = fuzzCaseMutates(testCase, options);
		const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
		const mutationTierGated = mutates && mutationMode !== 'isolated';
		const gatedSkipReasons = mutationTierGated ? reasonList([...skipReasons, 'requires-isolated-mutation-runtime']) : skipReasons;
		const executable = testCase.executable !== false && gatedSkipReasons.length === 0 && !mutationTierGated;
		const executionTier = normalizeWordPressFuzzExecutionTier(
			testCase.execution_tier || testCase.executionTier || testCase.metadata?.execution_tier || testCase.metadata?.executionTier,
			{ executable, mutates }
		);
		return {
			...testCase,
			executable,
			execution_tier: executionTier,
			required_capabilities: required,
			skip_reasons: gatedSkipReasons,
			metadata: {
				...metadata,
				executable,
				execution_tier: executionTier,
				gated: (metadata.gated === true || mutationTierGated) && !executable,
				runtime_capability_gated: false,
				execution_tier_gated: mutationTierGated,
				runtime_capability_contract: WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA,
				required_capabilities: required,
				required_any_capabilities: requiredAny.length > 0 ? requiredAny : undefined,
			},
		};
	}

	return {
		...testCase,
		executable: false,
		execution_tier: 'plan_only',
		required_capabilities: required,
		skip_reasons: reasonList([...skipReasons, 'missing-runtime-fuzz-capabilities']),
		metadata: {
			...metadata,
			executable: false,
			execution_tier: 'plan_only',
			planned: true,
			gated: true,
			runtime_capability_gated: true,
			runtime_capability_contract: WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA,
			required_capabilities: required,
			required_any_capabilities: requiredAny.length > 0 ? requiredAny : undefined,
			missing_capabilities: missing,
			missing_any_capabilities: missingAny.length > 0 ? missingAny : undefined,
		},
	};
}

function gateWordPressFuzzCaseForMutationPolicy(testCase = {}, options = {}) {
	const mutationMode = normalizeWordPressFuzzMutationMode(options.mutation_mode || options.mutationMode);
	if (!mutationMode) {
		return null;
	}

	const destructiveReasons = reasonList(testCase.destructive_reasons || testCase.destructiveReasons || testCase.destructive_reason || testCase.destructiveReason);
	const mutates = fuzzCaseMutates(testCase, { mutates: options.mutates, destructive_reasons: destructiveReasons });
	const policy = {
		schema: WORDPRESS_FUZZ_MUTATION_POLICY_SCHEMA,
		mode: mutationMode,
		mutates,
		destructive_reasons: destructiveReasons,
	};

	let denyReason = '';
	if (mutationMode === 'read_only' && mutates) {
		denyReason = 'mutation-policy-read-only';
	} else if (mutationMode === 'destructive-deny' && (mutates || destructiveReasons.length > 0)) {
		denyReason = 'mutation-policy-destructive-deny';
	}

	if (!denyReason) {
		return null;
	}

	const skipReasons = reasonList(testCase.skip_reasons || testCase.skipReasons || testCase.skip_reason || testCase.skipReason);
	const metadata = isObject(testCase.metadata) ? { ...testCase.metadata } : {};
	return {
		...testCase,
		executable: false,
		execution_tier: 'plan_only',
		skip_reasons: reasonList([...skipReasons, denyReason]),
		destructive_reasons: destructiveReasons,
		metadata: {
			...metadata,
			executable: false,
			execution_tier: 'plan_only',
			planned: true,
			gated: true,
			mutation_policy_gated: true,
			mutation_policy: policy,
		},
	};
}

function normalizeWordPressFuzzExecutionTier(value, context = {}) {
	const normalized = String(value || '').trim().toLowerCase().replace(/-/g, '_');
	if (WORDPRESS_FUZZ_EXECUTION_TIERS.includes(normalized)) {
		return normalized;
	}
	if (context.discovered === true) {
		return 'discovered';
	}
	if (context.executable === false || context.planned === true || context.gated === true) {
		return 'plan_only';
	}
	return context.mutates === true ? 'isolated_mutating_executable' : 'read_only_executable';
}

function annotateWordPressFuzzCaseExecutionTier(testCase = {}, context = {}) {
	const skipReasons = reasonList(testCase.skip_reasons || testCase.skipReasons || testCase.skip_reason || testCase.skipReason);
	const mutates = fuzzCaseMutates(testCase, context);
	const executable = context.executable ?? testCase.executable ?? (skipReasons.length === 0 && !mutates);
	const executionTier = normalizeWordPressFuzzExecutionTier(
		context.execution_tier || context.executionTier || testCase.execution_tier || testCase.executionTier || testCase.metadata?.execution_tier || testCase.metadata?.executionTier,
		{ ...context, executable, mutates }
	);
	return {
		...testCase,
		executable,
		execution_tier: executionTier,
		metadata: {
			...(isObject(testCase.metadata) ? testCase.metadata : {}),
			executable,
			execution_tier: executionTier,
		},
	};
}

function fuzzCaseMutates(testCase = {}, context = {}) {
	const destructiveReasons = reasonList(context.destructive_reasons || context.destructiveReasons || testCase.destructive_reasons || testCase.destructiveReasons || testCase.destructive_reason || testCase.destructiveReason);
	return context.mutates === true
		|| destructiveReasons.length > 0
		|| testCase.metadata?.safety?.mutates === true
		|| testCase.metadata?.safety?.mutation === 'requires_isolated_editor_draft'
		|| testCase.operation?.safety?.mutates === true;
}

function requiredCapabilitiesForWordPressFuzzCase(kind) {
	return [...(WORDPRESS_FUZZ_RUNTIME_CAPABILITY_REQUIREMENTS[kind] || [])];
}

function normalizeRequiredCapabilities(value) {
	return [...new Set((Array.isArray(value) ? value : [value])
		.map(normalizeWordPressFuzzRuntimeCapability)
		.filter(Boolean))].sort();
}

function normalizeRequiredCapabilityGroups(value) {
	return (Array.isArray(value) ? value : [])
		.map((group) => normalizeRequiredCapabilities(Array.isArray(group) ? group : [group]))
		.filter((group) => group.length > 0);
}

function normalizeWordPressFuzzMutationMode(value) {
	const mode = String(value || '').trim().toLowerCase().replace(/-/g, '_') === 'read_only'
		? 'read_only'
		: String(value || '').trim().toLowerCase().replace(/_/g, '-');
	return WORDPRESS_FUZZ_MUTATION_MODES.includes(mode) ? mode : '';
}

function reasonList(value) {
	if (value === undefined || value === null) {
		return [];
	}
	return [...new Set((Array.isArray(value) ? value : [value]).map(String).filter(Boolean))].sort();
}

function isObject(value) {
	return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
	WORDPRESS_FUZZ_MUTATION_MODES,
	WORDPRESS_FUZZ_MUTATION_POLICY_SCHEMA,
	WORDPRESS_FUZZ_EXECUTION_TIERS,
	WORDPRESS_FUZZ_RUNTIME_CAPABILITIES,
	WORDPRESS_FUZZ_RUNTIME_CAPABILITY_REQUIREMENTS,
	WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA,
	annotateWordPressFuzzCaseExecutionTier,
	gateWordPressFuzzCaseForMutationPolicy,
	gateWordPressFuzzCaseForRuntimeCapabilities,
	normalizeWordPressFuzzExecutionTier,
	normalizeWordPressFuzzMutationMode,
	normalizeWordPressFuzzRuntimeCapabilities,
	normalizeWordPressFuzzRuntimeCapability,
	requiredCapabilitiesForWordPressFuzzCase,
};
