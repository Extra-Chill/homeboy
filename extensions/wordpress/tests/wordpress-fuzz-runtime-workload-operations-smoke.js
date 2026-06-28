'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA,
	WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA,
	attachWordPressFuzzRuntimeWorkloadOperationDescriptor,
	buildWordPressFuzzRuntimeWorkloadOperationDescriptor,
	requiredCapabilitiesForWordPressFuzzRuntimeOperation,
	summarizeWordPressFuzzRuntimeWorkloadOperations,
	validateWordPressFuzzRuntimeWorkloadOperationDescriptor,
} = require('../lib/wordpress-fuzz-runtime-workload-operations');
const {
	buildWordPressFuzzMutationLifecycleContract,
} = require('../lib/wordpress-fuzz-mutation-lifecycle');

const crudCase = {
	id: 'case:create-post',
	intent: 'create-post',
	operation_id: 'post:create',
	operation: {
		schema: 'homeboy/wordpress-crud-operation/v1',
		action: 'create',
		resource_type: 'post',
		input: { post_type: 'post' },
	},
	metadata: { crud: { action: 'create' } },
};
const crudDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(crudCase, {
	runtimeCapabilities: { capabilities: ['crud'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['mutation'], mutationIsolation: true },
});
assert.equal(crudDescriptor.schema, WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA);
assert.equal(crudDescriptor.family, 'crud');
assert.equal(crudDescriptor.command, 'wordpress.crud');
assert.equal(crudDescriptor.wp_codebox_command, 'run-wordpress-workload');
assert.equal(crudDescriptor.wp_codebox_ability, 'wp-codebox/run-wordpress-workload');
assert.equal(crudDescriptor.metadata.wp_codebox_command, 'run-wordpress-workload');
assert.equal(crudDescriptor.status, 'ready');
assert.equal(crudDescriptor.validation.schema, WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA);
assert.equal(crudDescriptor.validation.ok, true);
assert.deepEqual(crudDescriptor.required_capabilities, ['crud']);

const restDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest',
	intent: 'request-rest-route',
	operation: { method: 'GET', route: '/wp/v2/posts' },
}, { runtimeCapabilities: { capabilities: ['rest'] } });
assert.equal(restDescriptor.family, 'rest');
assert.equal(restDescriptor.command, 'wordpress.request-rest-route');
assert.deepEqual(restDescriptor.input, { method: 'GET', route: '/wp/v2/posts' });

const readinessSupportedRestDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-readiness',
	intent: 'request-rest-route',
	operation: { method: 'GET', route: '/wp/v2/posts' },
}, {
	runtimeCapabilities: { capabilities: ['rest'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['read'] },
});
assert.equal(readinessSupportedRestDescriptor.status, 'ready');

const readinessUnsupportedCrudDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(crudCase, {
	runtimeCapabilities: { capabilities: ['crud'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['read'] },
});
assert.equal(readinessUnsupportedCrudDescriptor.status, 'planned');
assert.equal(readinessUnsupportedCrudDescriptor.skip_reason, 'unsupported-runtime-workload-operation-kind');
assert.deepEqual(readinessUnsupportedCrudDescriptor.blockers.map((blocker) => blocker.code), ['unsupported-runtime-workload-operation-kind']);

const readinessMissingCommandDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(crudCase, {
	runtimeCapabilities: { capabilities: ['crud'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'blocked', command_available: false },
});
assert.equal(readinessMissingCommandDescriptor.status, 'blocked');
assert.equal(readinessMissingCommandDescriptor.skip_reason, 'wp-codebox-fuzz-readiness-command-unavailable');

const adminDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({ intent: 'request-admin-page', operation: { path: '/wp-admin/edit.php' } }, { runtimeCapabilities: { capabilities: ['admin'] } });
assert.equal(adminDescriptor.family, 'admin_page');
assert.equal(adminDescriptor.command, 'wordpress.load-admin-page');

const adminMutationDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:admin-post',
	intent: 'plan-admin-page-mutation',
	operation: { path: '/wp-admin/edit.php', method: 'POST', interaction_kind: 'form', interaction_id: 'bulk-action', selector: '#posts-filter', fields: { action: 'edit' } },
	destructive_reasons: ['form_mutation'],
	metadata: {
		capability_context: { required: ['edit_posts'] },
		nonce_context: { required: true, action: 'bulk-posts', field: '_wpnonce' },
		mutation_lifecycle: buildWordPressFuzzMutationLifecycleContract({ kind: 'admin', method: 'POST' }),
	},
}, { runtimeCapabilities: { capabilities: ['admin', 'snapshot', 'restore', 'reset'] } });
assert.equal(adminMutationDescriptor.status, 'planned');
assert.equal(adminMutationDescriptor.skip_reason, 'wp-codebox-fuzz-live-readiness-required');
assert.deepEqual(adminMutationDescriptor.required_capabilities, ['admin', 'reset', 'restore', 'snapshot']);
assert.equal(adminMutationDescriptor.input.interaction_kind, 'form');
assert.equal(adminMutationDescriptor.input.selector, '#posts-filter');
assert.deepEqual(adminMutationDescriptor.input.fields, { action: 'edit' });
assert.deepEqual(adminMutationDescriptor.input.capability_context, { required: ['edit_posts'] });
assert.deepEqual(adminMutationDescriptor.input.nonce_context, { required: true, action: 'bulk-posts', field: '_wpnonce' });

const pageDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({ intent: 'request-frontend-page', metadata: { surface: { type: 'frontend-url' } }, operation: { path: '/' } }, { runtimeCapabilities: { capabilities: ['browser'] } });
assert.equal(pageDescriptor.family, 'frontend_page');
assert.equal(pageDescriptor.command, 'wordpress.load-frontend-page');

const blockDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({ intent: 'render-block', operation: { block_name: 'core/paragraph', lifecycle: 'render' } }, { runtimeCapabilities: { capabilities: ['block'] } });
assert.equal(blockDescriptor.family, 'block');
assert.equal(blockDescriptor.command, 'wordpress.exercise-block');
assert.deepEqual(requiredCapabilitiesForWordPressFuzzRuntimeOperation({ intent: 'insert-block-in-editor', operation: { block_name: 'core/paragraph' } }), ['block', 'block-editor', 'browser']);

const dbDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({ intent: 'profile-database-query', operation: { query: 'SELECT ID FROM wp_posts' } }, { runtimeCapabilities: { capabilities: ['database', 'query-observation'] } });
assert.equal(dbDescriptor.family, 'database');
assert.equal(dbDescriptor.command, 'wordpress.profile-database');
assert.deepEqual(dbDescriptor.required_capabilities, ['database', 'query-observation']);

const dbMutationDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	intent: 'mutate-database-table',
	operation: { table: 'wp_posts', mutation: 'insert' },
	destructive_reasons: ['db-mutation'],
	metadata: { mutation_lifecycle: buildWordPressFuzzMutationLifecycleContract({ kind: 'database' }) },
}, { runtimeCapabilities: { capabilities: ['database', 'snapshot', 'transaction', 'reset'] } });
assert.equal(dbMutationDescriptor.status, 'planned');
assert.equal(dbMutationDescriptor.skip_reason, 'wp-codebox-fuzz-live-readiness-required');
assert.deepEqual(dbMutationDescriptor.required_capabilities, ['database', 'reset', 'snapshot', 'transaction']);

const capabilityOnlyMutationDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-post-capability-only',
	intent: 'request-rest-route',
	operation: { method: 'POST', route: '/example/v1/items' },
	metadata: { mutation_lifecycle: buildWordPressFuzzMutationLifecycleContract({ kind: 'rest', method: 'POST' }) },
}, { runtimeCapabilities: { capabilities: ['rest', 'checkpoint', 'rest-rollback'] } });
assert.equal(capabilityOnlyMutationDescriptor.status, 'planned');
assert.equal(capabilityOnlyMutationDescriptor.skip_reason, 'wp-codebox-fuzz-live-readiness-required');

const mutationLifecycle = buildWordPressFuzzMutationLifecycleContract({ kind: 'rest', method: 'DELETE' });
const mutatingDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-delete',
	intent: 'request-rest-route',
	operation: { method: 'DELETE', route: '/wp/v2/posts/1' },
	metadata: { mutation_lifecycle: mutationLifecycle },
}, { runtimeCapabilities: { capabilities: ['rest'] } });
assert.equal(mutatingDescriptor.mutation_lifecycle.delete_boundary_required, true);
assert.equal(mutatingDescriptor.metadata.mutation_lifecycle.kind, 'rest');

const mutationIsolationBlockedDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-post-no-isolation',
	intent: 'request-rest-route',
	operation: { method: 'POST', route: '/example/v1/items' },
	metadata: { mutation_lifecycle: buildWordPressFuzzMutationLifecycleContract({ kind: 'rest', method: 'POST' }) },
}, {
	runtimeCapabilities: { capabilities: ['rest', 'checkpoint', 'rest-rollback'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['mutation'] },
});
assert.equal(mutationIsolationBlockedDescriptor.status, 'blocked');
assert.equal(mutationIsolationBlockedDescriptor.skip_reason, 'wp-codebox-fuzz-mutation-isolation-unsupported');
assert.equal(mutationIsolationBlockedDescriptor.blockers[0].code, 'wp-codebox-fuzz-mutation-isolation-unsupported');

const deleteBoundaryBlockedDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-delete-no-boundary',
	intent: 'request-rest-route',
	operation: { method: 'DELETE', route: '/example/v1/items/42' },
	metadata: { mutation_lifecycle: buildWordPressFuzzMutationLifecycleContract({ kind: 'rest', method: 'DELETE' }) },
}, {
	runtimeCapabilities: { capabilities: ['rest', 'checkpoint', 'rest-rollback'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['mutation'], mutationIsolation: true },
});
assert.equal(deleteBoundaryBlockedDescriptor.status, 'blocked');
assert.equal(deleteBoundaryBlockedDescriptor.skip_reason, 'wp-codebox-fuzz-delete-boundary-unsupported');
assert.equal(deleteBoundaryBlockedDescriptor.blockers[0].code, 'wp-codebox-fuzz-delete-boundary-unsupported');

const readyMutationDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-delete-ready',
	intent: 'request-rest-route',
	operation: { method: 'DELETE', route: '/example/v1/items/42' },
	metadata: { mutation_lifecycle: buildWordPressFuzzMutationLifecycleContract({ kind: 'rest', method: 'DELETE' }) },
}, {
	runtimeCapabilities: { capabilities: ['rest', 'checkpoint', 'rest-rollback'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['mutation'], mutationIsolation: true, deleteBoundary: true },
});
assert.equal(readyMutationDescriptor.status, 'ready');

const skipped = attachWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:block',
	intent: 'render-block',
	operation: { block_name: 'core/paragraph' },
	skip_reasons: ['existing-reason'],
	metadata: {},
}, { runtimeCapabilities: { capabilities: [] } });
assert.equal(skipped.executable, false);
assert.equal(skipped.execution_tier, 'plan_only');
assert.equal(skipped.runtime_operation.status, 'planned');
assert.deepEqual(skipped.runtime_operation.missing_capabilities, ['block']);
assert.deepEqual(skipped.runtime_operation.blockers.map((blocker) => blocker.code), ['missing-runtime-workload-capability']);
assert.deepEqual(skipped.skip_reasons, ['existing-reason', 'missing-runtime-workload-capability']);
assert.deepEqual(skipped.required_capabilities, ['block']);
assert.equal(skipped.metadata.runtime_workload_capability_gated, true);

const invalid = attachWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:invalid-rest',
	intent: 'request-rest-route',
	operation: { method: 'GET' },
}, { runtimeCapabilities: { capabilities: ['rest'] } });
assert.equal(invalid.executable, false);
assert.equal(invalid.execution_tier, 'plan_only');
assert.equal(invalid.runtime_operation.status, 'blocked');
assert.equal(invalid.runtime_operation.validation.ok, false);
assert.deepEqual(invalid.runtime_operation.blockers.map((blocker) => blocker.code), ['missing-runtime-workload-operation-field']);
assert.deepEqual(invalid.skip_reasons, ['invalid-runtime-workload-operation']);
assert.equal(invalid.metadata.runtime_workload_operation_blocked, true);
assert.equal(validateWordPressFuzzRuntimeWorkloadOperationDescriptor(invalid.runtime_operation).ok, false);

const summary = summarizeWordPressFuzzRuntimeWorkloadOperations({
	targets: [{ cases: [crudDescriptor, skipped.runtime_operation, invalid.runtime_operation, readinessUnsupportedCrudDescriptor, readinessMissingCommandDescriptor].map((runtime_operation) => ({ runtime_operation })) }],
});
assert.equal(summary.schema, 'homeboy/wordpress-fuzz-runtime-workload-operation-summary/v1');
assert.equal(summary.total, 5);
assert.deepEqual(summary.by_status, { ready: 1, planned: 2, blocked: 2 });
assert.deepEqual(summary.by_family_status.crud, { ready: 1, planned: 1, blocked: 1 });
assert.equal(summary.blockers.length, 4);

assert(!JSON.stringify([crudDescriptor, restDescriptor, adminDescriptor, pageDescriptor, blockDescriptor, dbDescriptor]).includes('woocommerce'));

console.log('WordPress fuzz runtime workload operations smoke passed.');
