'use strict';

const assert = require('node:assert/strict');
const {
	WORDPRESS_CRUD_OPERATION_SCHEMA,
	WORDPRESS_CRUD_OPERATION_RESULT_SCHEMA,
	WORDPRESS_FIXTURE_PERSONA_SCHEMA,
	WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA,
	normalizeWordPressCrudOperation,
	normalizeWordPressCrudOperationResult,
	normalizeWordPressFixturePersona,
	normalizeWordPressPerformanceObservation,
} = require('../lib/wordpress-generic-fuzz-primitives');

const readOperation = normalizeWordPressCrudOperation({
	schema: WORDPRESS_CRUD_OPERATION_SCHEMA,
	action: 'read',
	resource_type: 'post',
	transport: { type: 'rest', method: 'GET', route: '/wp/v2/posts' },
	capability_context: { required: ['read'] },
});

assert.equal(readOperation.id, 'read-post');
assert.equal(readOperation.safety.level, 'safe');
assert.equal(readOperation.safety.rollback_required, false);
assert.equal(readOperation.rollback_policy.strategy, 'none');

const deleteOperation = normalizeWordPressCrudOperation({
	action: 'delete',
	resource_type: 'comment',
	nonce_context: { action: 'delete-comment', required: true },
});

assert.equal(deleteOperation.safety.level, 'destructive');
assert.deepEqual(deleteOperation.safety.reason_codes, ['delete_operation']);
assert.equal(deleteOperation.rollback_policy.after_each_case, true);

const persona = normalizeWordPressFixturePersona({
	schema: WORDPRESS_FIXTURE_PERSONA_SCHEMA,
	id: 'editor-with-post',
	auth_context: {
		username: 'fixture-editor',
		roles: ['editor'],
		capabilities: ['edit_posts'],
		nonce_context: { source: 'wp_rest', required: true },
	},
	fixtures: [
		{
			id: 'draft-post',
			type: 'post',
			operation: {
				action: 'create',
				resource_type: 'post',
				input: { post_status: 'draft' },
			},
			reset_policy: { strategy: 'delete-created', after_each_case: true },
		},
	],
});

assert.equal(persona.schema, WORDPRESS_FIXTURE_PERSONA_SCHEMA);
assert.equal(persona.auth_context.roles[0], 'editor');
assert.equal(persona.fixtures[0].operation.safety.level, 'mutating');
assert.equal(persona.fixtures[0].reset_policy.strategy, 'delete-created');

const observation = normalizeWordPressPerformanceObservation({
	schema: WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA,
	id: 'crud-read-observation',
	operation_id: readOperation.id,
	persona_id: persona.id,
	status: 'passed',
	metrics: { queries: 3 },
	samples: [
		{ duration_ms: 12, metrics: { queries: 1 } },
		{ durationMs: 18, metrics: { queries: 2 } },
	],
});

assert.equal(observation.duration_ms, 30);
assert.equal(observation.summary.sample_count, 2);
assert.equal(observation.metrics.queries, 3);

const crudResult = normalizeWordPressCrudOperationResult({
	schema: WORDPRESS_CRUD_OPERATION_RESULT_SCHEMA,
	operation: {
		action: 'create',
		resource_type: 'post',
		capability_context: { required: ['edit_posts'] },
		nonce_context: { action: 'wp_rest', source: 'rest', required: true },
		rollback_policy: { strategy: 'delete-created' },
	},
	status: 'passed',
	resource_id: 123,
	auth_context: { username: 'fixture-editor', roles: ['editor'], capabilities: ['edit_posts'] },
	before_state_hash: 'sha256:before',
	after_state_hash: 'sha256:after',
	created_refs: [{ resource_type: 'post', resource_id: 123, rollback_action: 'delete' }],
	rollback_result: { status: 'passed', strategy: 'delete-created' },
});

assert.equal(crudResult.schema, WORDPRESS_CRUD_OPERATION_RESULT_SCHEMA);
assert.equal(crudResult.operation.id, 'create-post');
assert.equal(crudResult.resource_type, 'post');
assert.equal(crudResult.resource_id, 123);
assert.deepEqual(crudResult.capability_context.required, ['edit_posts']);
assert.equal(crudResult.nonce_context.required, true);
assert.equal(crudResult.before_state_hash, 'sha256:before');
assert.equal(crudResult.after_state_hash, 'sha256:after');
assert.equal(crudResult.rollback_policy.strategy, 'delete-created');
assert.equal(crudResult.rollback_result.status, 'passed');
assert.deepEqual(crudResult.created_refs, [{ resource_type: 'post', resource_id: 123, rollback_action: 'delete' }]);

const skippedCrudResult = normalizeWordPressCrudOperationResult({
	operation: { action: 'update', resource_type: 'option' },
	skip_reason: 'missing-runtime-fuzz-capabilities',
});
assert.equal(skippedCrudResult.status, 'skipped');
assert.deepEqual(skippedCrudResult.skip_reasons, ['missing-runtime-fuzz-capabilities']);

assert.throws(() => normalizeWordPressCrudOperation({ action: 'publish', resource_type: 'post' }), /Unsupported WordPress CRUD action/);
assert.throws(() => normalizeWordPressCrudOperationResult({ operation: { action: 'create', resource_type: 'post' }, status: 'unknown' }), /Unsupported WordPress CRUD operation result status/);
assert.throws(() => normalizeWordPressFixturePersona({ fixtures: [{ type: 'post' }] }), /persona.id must be a string/);
assert.throws(() => normalizeWordPressPerformanceObservation({ status: 'unknown' }), /Unsupported WordPress performance observation status/);

console.log('wordpress generic fuzz primitives smoke passed');
