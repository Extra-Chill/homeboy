'use strict';

const WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA = 'homeboy/wordpress-fuzz-runtime-capabilities/v1';

const WORDPRESS_FUZZ_RUNTIME_CAPABILITIES = Object.freeze([
	'snapshot',
	'checkpoint',
	'restore',
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
	mutating_rest: Object.freeze(['rest', 'snapshot', 'restore', 'reset']),
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
	const required = normalizeRequiredCapabilities(options.required_capabilities || options.requiredCapabilities || testCase.required_capabilities || testCase.requiredCapabilities);
	if (required.length === 0) {
		return testCase;
	}
	const declared = new Set(normalizeWordPressFuzzRuntimeCapabilities(runtimeCapabilities).capabilities);
	const missing = required.filter((capability) => !declared.has(capability));
	const skipReasons = reasonList(testCase.skip_reasons || testCase.skipReasons || testCase.skip_reason || testCase.skipReason);
	const metadata = isObject(testCase.metadata) ? { ...testCase.metadata } : {};

	if (missing.length === 0) {
		const executable = testCase.executable !== false && skipReasons.length === 0;
		return {
			...testCase,
			executable,
			required_capabilities: required,
			skip_reasons: skipReasons,
			metadata: {
				...metadata,
				executable,
				gated: metadata.gated === true && !executable,
				runtime_capability_gated: false,
				runtime_capability_contract: WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA,
				required_capabilities: required,
			},
		};
	}

	return {
		...testCase,
		executable: false,
		required_capabilities: required,
		skip_reasons: reasonList([...skipReasons, 'missing-runtime-fuzz-capabilities']),
		metadata: {
			...metadata,
			executable: false,
			planned: true,
			gated: true,
			runtime_capability_gated: true,
			runtime_capability_contract: WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA,
			required_capabilities: required,
			missing_capabilities: missing,
		},
	};
}

function requiredCapabilitiesForWordPressFuzzCase(kind) {
	return [...(WORDPRESS_FUZZ_RUNTIME_CAPABILITY_REQUIREMENTS[kind] || [])];
}

function normalizeRequiredCapabilities(value) {
	return [...new Set((Array.isArray(value) ? value : [value])
		.map(normalizeWordPressFuzzRuntimeCapability)
		.filter(Boolean))].sort();
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
	WORDPRESS_FUZZ_RUNTIME_CAPABILITIES,
	WORDPRESS_FUZZ_RUNTIME_CAPABILITY_REQUIREMENTS,
	WORDPRESS_FUZZ_RUNTIME_CAPABILITY_SCHEMA,
	gateWordPressFuzzCaseForRuntimeCapabilities,
	normalizeWordPressFuzzRuntimeCapabilities,
	normalizeWordPressFuzzRuntimeCapability,
	requiredCapabilitiesForWordPressFuzzCase,
};
