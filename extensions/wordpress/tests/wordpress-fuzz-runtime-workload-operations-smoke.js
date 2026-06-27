'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA,
	attachWordPressFuzzRuntimeWorkloadOperationDescriptor,
	buildWordPressFuzzRuntimeWorkloadOperationDescriptor,
	requiredCapabilitiesForWordPressFuzzRuntimeOperation,
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
const crudDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(crudCase, { runtimeCapabilities: { capabilities: ['crud'] } });
assert.equal(crudDescriptor.schema, WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA);
assert.equal(crudDescriptor.family, 'crud');
assert.equal(crudDescriptor.command, 'wordpress.crud');
assert.equal(crudDescriptor.status, 'ready');
assert.deepEqual(crudDescriptor.required_capabilities, ['crud']);

const restDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest',
	intent: 'request-rest-route',
	operation: { method: 'GET', route: '/wp/v2/posts' },
}, { runtimeCapabilities: { capabilities: ['rest'] } });
assert.equal(restDescriptor.family, 'rest');
assert.equal(restDescriptor.command, 'wordpress.request-rest-route');
assert.deepEqual(restDescriptor.input, { method: 'GET', route: '/wp/v2/posts' });

const adminDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({ intent: 'request-admin-page', operation: { path: '/wp-admin/edit.php' } }, { runtimeCapabilities: { capabilities: ['admin'] } });
assert.equal(adminDescriptor.family, 'admin_page');
assert.equal(adminDescriptor.command, 'wordpress.load-admin-page');

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

const mutationLifecycle = buildWordPressFuzzMutationLifecycleContract({ kind: 'rest', method: 'DELETE' });
const mutatingDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-delete',
	intent: 'request-rest-route',
	operation: { method: 'DELETE', route: '/wp/v2/posts/1' },
	metadata: { mutation_lifecycle: mutationLifecycle },
}, { runtimeCapabilities: { capabilities: ['rest'] } });
assert.equal(mutatingDescriptor.mutation_lifecycle.delete_boundary_required, true);
assert.equal(mutatingDescriptor.metadata.mutation_lifecycle.kind, 'rest');

const skipped = attachWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:block',
	intent: 'render-block',
	operation: { block_name: 'core/paragraph' },
	skip_reasons: ['existing-reason'],
	metadata: {},
}, { runtimeCapabilities: { capabilities: [] } });
assert.equal(skipped.executable, false);
assert.equal(skipped.execution_tier, 'plan_only');
assert.equal(skipped.runtime_operation.status, 'skipped');
assert.deepEqual(skipped.runtime_operation.missing_capabilities, ['block']);
assert.deepEqual(skipped.skip_reasons, ['existing-reason', 'missing-runtime-workload-capability']);
assert.deepEqual(skipped.required_capabilities, ['block']);
assert.equal(skipped.metadata.runtime_workload_capability_gated, true);

assert(!JSON.stringify([crudDescriptor, restDescriptor, adminDescriptor, pageDescriptor, blockDescriptor, dbDescriptor]).includes('woocommerce'));

console.log('WordPress fuzz runtime workload operations smoke passed.');
