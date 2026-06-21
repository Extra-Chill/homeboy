'use strict';

const assert = require('node:assert/strict');
const {
	WORDPRESS_CRUD_OPERATION_SCHEMA,
	WORDPRESS_FIXTURE_PERSONA_SCHEMA,
	WORDPRESS_PERFORMANCE_OBSERVATION_SCHEMA,
	normalizeWordPressCrudOperation,
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

assert.throws(() => normalizeWordPressCrudOperation({ action: 'publish', resource_type: 'post' }), /Unsupported WordPress CRUD action/);
assert.throws(() => normalizeWordPressFixturePersona({ fixtures: [{ type: 'post' }] }), /persona.id must be a string/);
assert.throws(() => normalizeWordPressPerformanceObservation({ status: 'unknown' }), /Unsupported WordPress performance observation status/);

console.log('wordpress generic fuzz primitives smoke passed');
