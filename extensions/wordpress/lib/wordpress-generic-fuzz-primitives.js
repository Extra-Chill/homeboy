'use strict';

const WORDPRESS_CRUD_OPERATION_SCHEMA = 'homeboy/wordpress-crud-operation/v1';
const WORDPRESS_FIXTURE_PERSONA_SCHEMA = 'homeboy/wordpress-fixture-persona/v1';
const WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA = 'homeboy/wordpress-performance-observation/v1';

const CRUD_ACTIONS = new Set(['create', 'read', 'update', 'delete']);
const SAFETY_LEVELS = new Set(['safe', 'mutating', 'destructive']);
const RESET_STRATEGIES = new Set(['none', 'delete-created', 'restore-snapshot', 'transaction', 'manual']);
const OBSERVATION_STATUSES = new Set(['passed', 'failed', 'errored', 'skipped']);

function normalizeWordPressCrudOperation(operation) {
	assertPlainObject(operation, 'operation');
	assertSchema(operation.schema, WORDPRESS_CRUD_OPERATION_SCHEMA, 'WordPress CRUD operation');

	const action = operation.action || operation.verb;
	if (!CRUD_ACTIONS.has(action)) {
		throw new Error(`Unsupported WordPress CRUD action: ${action}`);
	}

	const resourceType = operation.resource_type || operation.resourceType || operation.resource || operation.object_type || operation.objectType;
	if (!resourceType || typeof resourceType !== 'string') {
		throw new Error('operation.resource_type must be a string.');
	}

	const id = normalizeId(operation.id, `${action}-${resourceType}`, 'operation.id');

	return stripUndefined({
		schema: WORDPRESS_CRUD_OPERATION_SCHEMA,
		id,
		action,
		resource_type: resourceType,
		label: operation.label || id,
		safety: normalizeSafety(operation.safety, action),
		capability_context: normalizeCapabilityContext(operation.capability_context || operation.capabilityContext),
		nonce_context: normalizeNonceContext(operation.nonce_context || operation.nonceContext),
		transport: normalizeTransport(operation.transport),
		input: objectOrUndefined(operation.input),
		expected: objectOrUndefined(operation.expected),
		rollback_policy: normalizeResetPolicy(operation.rollback_policy || operation.rollbackPolicy || operation.reset_policy || operation.resetPolicy, action),
		metadata: objectOrUndefined(operation.metadata) || {},
	});
}

function normalizeWordPressFixturePersona(persona) {
	assertPlainObject(persona, 'persona');
	assertSchema(persona.schema, WORDPRESS_FIXTURE_PERSONA_SCHEMA, 'WordPress fixture persona');

	const id = normalizeId(persona.id, null, 'persona.id');
	const fixtures = asArray(persona.fixtures, 'fixtures').map(normalizeFixture);

	return stripUndefined({
		schema: WORDPRESS_FIXTURE_PERSONA_SCHEMA,
		id,
		label: persona.label || id,
		description: stringOrUndefined(persona.description),
		auth_context: normalizeAuthContext(persona.auth_context || persona.authContext || persona.user),
		fixtures,
		reset_policy: normalizeResetPolicy(persona.reset_policy || persona.resetPolicy, 'update'),
		metadata: objectOrUndefined(persona.metadata) || {},
	});
}

function normalizeWordPressPerformanceObservation(observation) {
	assertPlainObject(observation, 'observation');
	assertSchema(observation.schema, WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA, 'WordPress performance observation');

	const id = normalizeId(observation.id, 'wordpress-performance-observation', 'observation.id');
	const samples = asArray(observation.samples, 'samples').map(normalizePerformanceSample);
	const durationMs = numberOrNull(observation.duration_ms ?? observation.durationMs) ?? sumSampleDurations(samples);
	const status = observation.status || 'passed';
	if (!OBSERVATION_STATUSES.has(status)) {
		throw new Error(`Unsupported WordPress performance observation status: ${status}`);
	}

	return stripUndefined({
		schema: WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA,
		id,
		status,
		operation_id: observation.operation_id || observation.operationId || null,
		fixture_id: observation.fixture_id || observation.fixtureId || null,
		persona_id: observation.persona_id || observation.personaId || null,
		started_at: observation.started_at || observation.startedAt || null,
		finished_at: observation.finished_at || observation.finishedAt || null,
		duration_ms: durationMs,
		summary: {
			sample_count: samples.length,
			duration_ms: durationMs,
			...(observation.summary || {}),
		},
		metrics: objectOrUndefined(observation.metrics) || {},
		budgets: objectOrUndefined(observation.budgets),
		regressions: asArray(observation.regressions, 'regressions'),
		samples,
		runtime: objectOrUndefined(observation.runtime),
		metadata: objectOrUndefined(observation.metadata) || {},
	});
}

function normalizeFixture(fixture, index) {
	assertPlainObject(fixture, `fixtures[${index}]`);
	const id = normalizeId(fixture.id, `fixture-${index + 1}`, `fixtures[${index}].id`);
	const type = fixture.type || fixture.resource_type || fixture.resourceType;
	if (!type || typeof type !== 'string') {
		throw new Error(`fixtures[${index}].type must be a string.`);
	}

	return stripUndefined({
		...fixture,
		id,
		type,
		operation: fixture.operation ? normalizeWordPressCrudOperation(fixture.operation) : undefined,
		depends_on: asArray(fixture.depends_on || fixture.dependsOn, `fixtures[${index}].depends_on`),
		reset_policy: normalizeResetPolicy(fixture.reset_policy || fixture.resetPolicy, 'create'),
		metadata: objectOrUndefined(fixture.metadata) || {},
	});
}

function normalizeSafety(safety, action) {
	const defaults = defaultSafetyForAction(action);
	if (safety === undefined || safety === null) {
		return defaults;
	}
	assertPlainObject(safety, 'safety');
	const level = safety.level || defaults.level;
	if (!SAFETY_LEVELS.has(level)) {
		throw new Error(`Unsupported WordPress operation safety level: ${level}`);
	}

	return {
		level,
		mutates: safety.mutates ?? defaults.mutates,
		rollback_required: safety.rollback_required ?? safety.rollbackRequired ?? defaults.rollback_required,
		reason_codes: normalizeReasonCodes(safety.reason_codes || safety.reasonCodes || defaults.reason_codes),
	};
}

function defaultSafetyForAction(action) {
	if (action === 'read') {
		return { level: 'safe', mutates: false, rollback_required: false, reason_codes: [] };
	}
	if (action === 'delete') {
		return { level: 'destructive', mutates: true, rollback_required: true, reason_codes: ['delete_operation'] };
	}
	return { level: 'mutating', mutates: true, rollback_required: true, reason_codes: [`${action}_operation`] };
}

function normalizeCapabilityContext(context) {
	if (context === undefined || context === null) {
		return undefined;
	}
	assertPlainObject(context, 'capability_context');
	return stripUndefined({
		required: normalizeStringArray(context.required || context.capabilities, 'capability_context.required'),
		forbidden: normalizeStringArray(context.forbidden, 'capability_context.forbidden'),
		role: stringOrUndefined(context.role),
		user_id: context.user_id ?? context.userId,
		metadata: objectOrUndefined(context.metadata),
	});
}

function normalizeNonceContext(context) {
	if (context === undefined || context === null) {
		return undefined;
	}
	assertPlainObject(context, 'nonce_context');
	return stripUndefined({
		action: stringOrUndefined(context.action),
		field: stringOrUndefined(context.field),
		source: stringOrUndefined(context.source),
		required: context.required,
		metadata: objectOrUndefined(context.metadata),
	});
}

function normalizeAuthContext(context) {
	if (context === undefined || context === null) {
		return undefined;
	}
	assertPlainObject(context, 'auth_context');
	return stripUndefined({
		user_id: context.user_id ?? context.userId,
		username: stringOrUndefined(context.username || context.user_login),
		roles: normalizeStringArray(context.roles, 'auth_context.roles'),
		capabilities: normalizeStringArray(context.capabilities, 'auth_context.capabilities'),
		nonce_context: normalizeNonceContext(context.nonce_context || context.nonceContext),
		metadata: objectOrUndefined(context.metadata),
	});
}

function normalizeTransport(transport) {
	if (transport === undefined || transport === null) {
		return undefined;
	}
	assertPlainObject(transport, 'transport');
	return stripUndefined({
		type: stringOrUndefined(transport.type),
		method: stringOrUndefined(transport.method),
		route: stringOrUndefined(transport.route),
		path: stringOrUndefined(transport.path),
		command: stringOrUndefined(transport.command),
		metadata: objectOrUndefined(transport.metadata),
	});
}

function normalizeResetPolicy(policy, action) {
	const defaults = defaultResetPolicyForAction(action);
	if (policy === undefined || policy === null) {
		return defaults;
	}
	assertPlainObject(policy, 'reset_policy');
	const strategy = policy.strategy || defaults.strategy;
	if (!RESET_STRATEGIES.has(strategy)) {
		throw new Error(`Unsupported WordPress reset policy strategy: ${strategy}`);
	}
	return stripUndefined({
		strategy,
		scope: stringOrUndefined(policy.scope || defaults.scope),
		after_each_case: policy.after_each_case ?? policy.afterEachCase ?? defaults.after_each_case,
		artifacts: normalizeStringArray(policy.artifacts, 'reset_policy.artifacts'),
		metadata: objectOrUndefined(policy.metadata),
	});
}

function defaultResetPolicyForAction(action) {
	if (action === 'read') {
		return { strategy: 'none', scope: 'operation', after_each_case: false };
	}
	return { strategy: 'delete-created', scope: 'operation', after_each_case: true };
}

function normalizePerformanceSample(sample, index) {
	assertPlainObject(sample, `samples[${index}]`);
	return stripUndefined({
		id: sample.id || `sample-${index + 1}`,
		operation_id: sample.operation_id || sample.operationId,
		started_at: sample.started_at || sample.startedAt,
		finished_at: sample.finished_at || sample.finishedAt,
		duration_ms: numberOrNull(sample.duration_ms ?? sample.durationMs),
		metrics: objectOrUndefined(sample.metrics) || {},
		metadata: objectOrUndefined(sample.metadata),
	});
}

function normalizeReasonCodes(value) {
	return [...new Set(asArray(Array.isArray(value) ? value : [value], 'reason_codes').map(String).filter(Boolean))].sort();
}

function normalizeStringArray(value, field) {
	return asArray(value, field).map(String).filter(Boolean);
}

function sumSampleDurations(samples) {
	const total = samples.reduce((sum, sample) => sum + (Number(sample.duration_ms) || 0), 0);
	return total > 0 ? total : null;
}

function assertPlainObject(value, field) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`${field} must be an object.`);
	}
}

function assertSchema(value, expected, field) {
	if (value && value !== expected) {
		throw new Error(`Unsupported ${field} schema: ${value}`);
	}
}

function asArray(value, field) {
	if (value === undefined || value === null) {
		return [];
	}
	if (!Array.isArray(value)) {
		throw new Error(`${field} must be an array.`);
	}
	return value;
}

function normalizeId(value, fallback, field) {
	const id = value || fallback;
	if (!id || typeof id !== 'string') {
		throw new Error(`${field} must be a string.`);
	}
	return id;
}

function objectOrUndefined(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : undefined;
}

function stringOrUndefined(value) {
	return typeof value === 'string' && value !== '' ? value : undefined;
}

function numberOrNull(value) {
	if (value === undefined || value === null || value === '') {
		return null;
	}
	const parsed = Number(value);
	return Number.isFinite(parsed) ? parsed : null;
}

function stripUndefined(value) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return value;
	}
	return Object.fromEntries(
		Object.entries(value).filter(([, entry]) => entry !== undefined)
	);
}

module.exports = {
	WORDPRESS_CRUD_OPERATION_SCHEMA,
	WORDPRESS_FIXTURE_PERSONA_SCHEMA,
	WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA,
	normalizeWordPressCrudOperation,
	normalizeWordPressFixturePersona,
	normalizeWordPressPerformanceObservation,
};
