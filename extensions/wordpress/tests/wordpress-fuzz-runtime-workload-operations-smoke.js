'use strict';

const assert = require('node:assert/strict');

const {
	WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA,
	WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA,
	attachWordPressFuzzRuntimeWorkloadOperationDescriptor,
	buildWordPressFuzzRuntimeWorkloadOperationDescriptor,
	mapWordPressRuntimeActionToCodeboxContract,
	requiredCapabilitiesForWordPressFuzzRuntimeOperation,
	summarizeWordPressFuzzRuntimeWorkloadOperations,
	validateWordPressFuzzRuntimeWorkloadOperationDescriptor,
} = require('../lib/wordpress-fuzz-runtime-workload-operations');
const {
	buildWordPressFuzzMutationLifecycleContract,
} = require('../lib/wordpress-fuzz-mutation-lifecycle');

function actionContract(action) {
	return {
		schema: 'wp-codebox/wordpress-runtime-action/v1',
		action,
		ability: `wp-codebox/runtime-action/${action}`,
		input_schema: `wp-codebox/wordpress-runtime-action/${action}/input/v1`,
		output_schema: `wp-codebox/wordpress-runtime-action/${action}/output/v1`,
	};
}

const runtimeActionContracts = {
	schema: 'wp-codebox/wordpress-runtime-action-contracts/v1',
	actions: Object.fromEntries([
		'rest_request',
		'crud_operation',
		'admin_page_load',
		'frontend_page_load',
		'block_render',
		'block_editor',
		'db_query',
		'wp_cli',
		'login_as',
		'nonce_for',
		'checkpoint',
		'restore',
		'reset_state',
		'replay_case',
		'minimize_case',
	].map((action) => [action, actionContract(action)])),
};

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
	codeboxRuntimeContracts: runtimeActionContracts,
});
assert.equal(crudDescriptor.schema, WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_SCHEMA);
assert.equal(crudDescriptor.family, 'crud');
assert.equal(crudDescriptor.action, 'crud_operation');
assert.equal(crudDescriptor.command, 'wordpress.crud-operation');
assert.equal(crudDescriptor.wp_codebox_command, undefined);
assert.equal(crudDescriptor.wp_codebox_ability, 'wp-codebox/runtime-action/crud_operation');
assert.equal(crudDescriptor.wp_codebox_input_schema, 'wp-codebox/wordpress-runtime-action/crud_operation/input/v1');
assert.equal(crudDescriptor.metadata.wp_codebox_ability, 'wp-codebox/runtime-action/crud_operation');
assert.equal(crudDescriptor.status, 'ready');
assert.equal(crudDescriptor.validation.schema, WORDPRESS_FUZZ_RUNTIME_WORKLOAD_OPERATION_VALIDATION_SCHEMA);
assert.equal(crudDescriptor.validation.ok, true);
assert.deepEqual(crudDescriptor.required_capabilities, ['crud']);

const missingContractDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(crudCase, {
	runtimeCapabilities: { capabilities: ['crud'] },
});
assert.equal(missingContractDescriptor.status, 'blocked');
assert.equal(missingContractDescriptor.skip_reason, 'wp-codebox-runtime-action-contracts-missing');
assert.equal(missingContractDescriptor.blockers[0].code, 'wp-codebox-runtime-action-contracts-missing');

const unsupportedContractDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(crudCase, {
	runtimeCapabilities: { capabilities: ['crud'] },
	codeboxRuntimeContracts: { schema: 'wp-codebox/wordpress-runtime-action-contracts/v1', actions: { rest_request: actionContract('rest_request') } },
});
assert.equal(unsupportedContractDescriptor.status, 'blocked');
assert.equal(unsupportedContractDescriptor.skip_reason, 'unsupported-wordpress-runtime-action');
assert.equal(unsupportedContractDescriptor.blockers[0].code, 'unsupported-wordpress-runtime-action');
assert.deepEqual(unsupportedContractDescriptor.blockers[0].supported_actions, ['rest_request']);

assert.deepEqual(mapWordPressRuntimeActionToCodeboxContract('rest_request', runtimeActionContracts), {
	action: 'rest_request',
	ability: 'wp-codebox/runtime-action/rest_request',
	input_schema: 'wp-codebox/wordpress-runtime-action/rest_request/input/v1',
	output_schema: 'wp-codebox/wordpress-runtime-action/rest_request/output/v1',
	contract_schema: 'wp-codebox/wordpress-runtime-action/v1',
});

const restDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest',
	intent: 'request-rest-route',
	operation: { method: 'GET', route: '/wp/v2/posts' },
}, { runtimeCapabilities: { capabilities: ['rest'] }, codeboxRuntimeContracts: runtimeActionContracts });
assert.equal(restDescriptor.family, 'rest');
assert.equal(restDescriptor.action, 'rest_request');
assert.equal(restDescriptor.command, 'wordpress.rest-request');
assert.deepEqual(restDescriptor.input, { method: 'GET', route: '/wp/v2/posts' });

const readinessSupportedRestDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-readiness',
	intent: 'request-rest-route',
	operation: { method: 'GET', route: '/wp/v2/posts' },
}, {
	runtimeCapabilities: { capabilities: ['rest'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['read'] },
	codeboxRuntimeContracts: runtimeActionContracts,
});
assert.equal(readinessSupportedRestDescriptor.status, 'ready');

const readinessUnsupportedCrudDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(crudCase, {
	runtimeCapabilities: { capabilities: ['crud'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'ready', operationKinds: ['read'] },
	codeboxRuntimeContracts: runtimeActionContracts,
});
assert.equal(readinessUnsupportedCrudDescriptor.status, 'planned');
assert.equal(readinessUnsupportedCrudDescriptor.skip_reason, 'unsupported-runtime-workload-operation-kind');
assert.deepEqual(readinessUnsupportedCrudDescriptor.blockers.map((blocker) => blocker.code), ['unsupported-runtime-workload-operation-kind']);

const readinessMissingCommandDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(crudCase, {
	runtimeCapabilities: { capabilities: ['crud'] },
	runtimeReadiness: { schema: 'wp-codebox/fuzz-runner-readiness/v1', status: 'blocked', command_available: false },
	codeboxRuntimeContracts: runtimeActionContracts,
});
assert.equal(readinessMissingCommandDescriptor.status, 'blocked');
assert.equal(readinessMissingCommandDescriptor.skip_reason, 'wp-codebox-fuzz-readiness-command-unavailable');

const adminDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({ intent: 'request-admin-page', operation: { path: '/wp-admin/edit.php' } }, { runtimeCapabilities: { capabilities: ['admin'] }, codeboxRuntimeContracts: runtimeActionContracts });
assert.equal(adminDescriptor.family, 'admin_page');
assert.equal(adminDescriptor.action, 'admin_page_load');
assert.equal(adminDescriptor.command, 'wordpress.admin-page-load');

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
}, { runtimeCapabilities: { capabilities: ['admin', 'snapshot', 'restore', 'reset'] }, codeboxRuntimeContracts: runtimeActionContracts });
assert.equal(adminMutationDescriptor.status, 'ready');
assert.equal(adminMutationDescriptor.skip_reason, undefined);
assert.deepEqual(adminMutationDescriptor.required_capabilities, ['admin', 'reset', 'restore', 'snapshot']);
assert.equal(adminMutationDescriptor.input.interaction_kind, 'form');
assert.equal(adminMutationDescriptor.input.selector, '#posts-filter');
assert.deepEqual(adminMutationDescriptor.input.fields, { action: 'edit' });
assert.deepEqual(adminMutationDescriptor.input.capability_context, { required: ['edit_posts'] });
assert.deepEqual(adminMutationDescriptor.input.nonce_context, { required: true, action: 'bulk-posts', field: '_wpnonce' });

const pageDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({ intent: 'request-frontend-page', metadata: { surface: { type: 'frontend-url' } }, operation: { path: '/' } }, { runtimeCapabilities: { capabilities: ['browser'] }, codeboxRuntimeContracts: runtimeActionContracts });
assert.equal(pageDescriptor.family, 'frontend_page');
assert.equal(pageDescriptor.action, 'frontend_page_load');
assert.equal(pageDescriptor.command, 'wordpress.frontend-page-load');

const blockDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({ intent: 'render-block', operation: { block_name: 'core/paragraph', lifecycle: 'render' } }, { runtimeCapabilities: { capabilities: ['block'] }, codeboxRuntimeContracts: runtimeActionContracts });
assert.equal(blockDescriptor.family, 'block');
assert.equal(blockDescriptor.action, 'block_render');
assert.equal(blockDescriptor.command, 'wordpress.block-render');
assert.deepEqual(requiredCapabilitiesForWordPressFuzzRuntimeOperation({ intent: 'insert-block-in-editor', operation: { block_name: 'core/paragraph' } }), ['block', 'block-editor', 'browser']);

const dbDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({ intent: 'profile-database-query', operation: { query: 'SELECT ID FROM wp_posts' } }, { runtimeCapabilities: { capabilities: ['database', 'query-observation'] }, codeboxRuntimeContracts: runtimeActionContracts });
assert.equal(dbDescriptor.family, 'database');
assert.equal(dbDescriptor.action, 'db_query');
assert.equal(dbDescriptor.command, 'wordpress.db-query');
assert.deepEqual(dbDescriptor.required_capabilities, ['database', 'query-observation']);

const dbMutationDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	intent: 'mutate-database-table',
	operation: { table: 'wp_posts', mutation: 'insert' },
	destructive_reasons: ['db-mutation'],
	metadata: { mutation_lifecycle: buildWordPressFuzzMutationLifecycleContract({ kind: 'database' }) },
}, { runtimeCapabilities: { capabilities: ['database', 'snapshot', 'transaction', 'reset'] }, codeboxRuntimeContracts: runtimeActionContracts });
assert.equal(dbMutationDescriptor.status, 'ready');
assert.equal(dbMutationDescriptor.skip_reason, undefined);
assert.deepEqual(dbMutationDescriptor.required_capabilities, ['database', 'reset', 'snapshot', 'transaction']);

const capabilityOnlyMutationDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-post-capability-only',
	intent: 'request-rest-route',
	operation: { method: 'POST', route: '/example/v1/items' },
	metadata: { mutation_lifecycle: buildWordPressFuzzMutationLifecycleContract({ kind: 'rest', method: 'POST' }) },
}, { runtimeCapabilities: { capabilities: ['rest', 'checkpoint', 'rest-rollback'] }, codeboxRuntimeContracts: runtimeActionContracts });
assert.equal(capabilityOnlyMutationDescriptor.status, 'ready');
assert.equal(capabilityOnlyMutationDescriptor.skip_reason, undefined);

const mutationLifecycle = buildWordPressFuzzMutationLifecycleContract({ kind: 'rest', method: 'DELETE' });
const mutatingDescriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:rest-delete',
	intent: 'request-rest-route',
	operation: { method: 'DELETE', route: '/wp/v2/posts/1' },
	metadata: { mutation_lifecycle: mutationLifecycle },
}, { runtimeCapabilities: { capabilities: ['rest'] }, codeboxRuntimeContracts: runtimeActionContracts });
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
	codeboxRuntimeContracts: runtimeActionContracts,
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
	codeboxRuntimeContracts: runtimeActionContracts,
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
	codeboxRuntimeContracts: runtimeActionContracts,
});
assert.equal(readyMutationDescriptor.status, 'ready');

const runtimeActionCases = [
	[{ target: { kind: 'runtime-action' }, operation: { runtime_action: 'wp_cli', args: ['plugin', 'list'] } }, 'wp_cli', { args: ['plugin', 'list'] }],
	[{ target: { kind: 'runtime-action' }, operation: { runtime_action: 'login_as', user: 1 } }, 'login_as', { user: 1 }],
	[{ target: { kind: 'runtime-action' }, operation: { runtime_action: 'nonce_for', action: 'bulk-posts' } }, 'nonce_for', { action: 'bulk-posts' }],
	[{ target: { kind: 'runtime-action' }, operation: { runtime_action: 'checkpoint', label: 'before' } }, 'checkpoint', { label: 'before' }],
	[{ target: { kind: 'runtime-action' }, operation: { runtime_action: 'restore', checkpoint_id: 'cp-1' } }, 'restore', { checkpoint_id: 'cp-1' }],
	[{ target: { kind: 'runtime-action' }, operation: { runtime_action: 'reset_state', scope: 'database' } }, 'reset_state', { scope: 'database' }],
	[{ target: { kind: 'runtime-action' }, operation: { runtime_action: 'replay_case', case: { id: 'case-1' } } }, 'replay_case', { case: { id: 'case-1' } }],
	[{ target: { kind: 'runtime-action' }, operation: { runtime_action: 'minimize_case', case: { id: 'case-2' } } }, 'minimize_case', { case: { id: 'case-2' } }],
];
for (const [testCase, expectedAction, expectedInput] of runtimeActionCases) {
	const descriptor = buildWordPressFuzzRuntimeWorkloadOperationDescriptor(testCase, { codeboxRuntimeContracts: runtimeActionContracts });
	assert.equal(descriptor.action, expectedAction);
	assert.equal(descriptor.status, 'ready');
	assert.equal(descriptor.wp_codebox_ability, `wp-codebox/runtime-action/${expectedAction}`);
	assert.deepEqual(descriptor.input, expectedInput);
}

const skipped = attachWordPressFuzzRuntimeWorkloadOperationDescriptor({
	id: 'case:block',
	intent: 'render-block',
	operation: { block_name: 'core/paragraph' },
	skip_reasons: ['existing-reason'],
	metadata: {},
}, { runtimeCapabilities: { capabilities: [] }, codeboxRuntimeContracts: runtimeActionContracts });
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
}, { runtimeCapabilities: { capabilities: ['rest'] }, codeboxRuntimeContracts: runtimeActionContracts });
assert.equal(invalid.executable, false);
assert.equal(invalid.execution_tier, 'plan_only');
assert.equal(invalid.runtime_operation.status, 'blocked');
assert.equal(invalid.runtime_operation.validation.ok, false);
assert.deepEqual(invalid.runtime_operation.blockers.map((blocker) => blocker.code), ['missing-runtime-workload-operation-field']);
assert.deepEqual(invalid.skip_reasons, ['invalid-runtime-workload-operation']);
assert.equal(invalid.metadata.runtime_workload_operation_blocked, true);
assert.equal(validateWordPressFuzzRuntimeWorkloadOperationDescriptor(invalid.runtime_operation).ok, false);

const summary = summarizeWordPressFuzzRuntimeWorkloadOperations({
	targets: [{ cases: [crudDescriptor, skipped.runtime_operation, invalid.runtime_operation, readinessUnsupportedCrudDescriptor, readinessMissingCommandDescriptor, missingContractDescriptor, unsupportedContractDescriptor].map((runtime_operation) => ({ runtime_operation })) }],
});
assert.equal(summary.schema, 'homeboy/wordpress-fuzz-runtime-workload-operation-summary/v1');
assert.equal(summary.total, 7);
assert.deepEqual(summary.by_status, { ready: 1, planned: 2, blocked: 4 });
assert.deepEqual(summary.by_family_status.crud, { ready: 1, planned: 1, blocked: 3 });
assert.equal(summary.blockers.length, 6);

assert(!JSON.stringify([crudDescriptor, restDescriptor, adminDescriptor, pageDescriptor, blockDescriptor, dbDescriptor]).includes('woocommerce'));

console.log('WordPress fuzz runtime workload operations smoke passed.');
