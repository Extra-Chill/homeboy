'use strict';

const assert = require('node:assert/strict');

const wordpress = require('../index');
const {
  AUDIT_FANOUT_APPLY_REQUEST_SCHEMA,
  AUDIT_FANOUT_DISPATCH_REQUEST_SCHEMA,
  AUDIT_FANOUT_RUNTIME_PROVIDER_SCHEMA,
  auditFanoutRuntimeProviderInterface,
  createAuditFanoutRuntimeProvider,
  validateAuditFanoutApplyRequest,
  validateAuditFanoutDispatchRequest,
  validateAuditFanoutRuntimeProvider,
} = require('../lib/audit-fanout-runtime-provider');

const providerInterface = auditFanoutRuntimeProviderInterface({
  id: 'wordpress.audit-fanout-codebox-runtime',
  label: 'WP Codebox audit fanout runtime',
  backend: 'wp-codebox',
});

assert.equal(providerInterface.schema, AUDIT_FANOUT_RUNTIME_PROVIDER_SCHEMA);
assert.equal(providerInterface.id, 'wordpress.audit-fanout-codebox-runtime');
assert.equal(providerInterface.backend, 'wp-codebox');
assert.equal(providerInterface.operations.dispatch.request_schema, AUDIT_FANOUT_DISPATCH_REQUEST_SCHEMA);
assert.equal(providerInterface.operations.apply.request_schema, AUDIT_FANOUT_APPLY_REQUEST_SCHEMA);
assert.equal(providerInterface.capabilities.includes('audit_fanout.dispatch'), true);
assert.equal(providerInterface.capabilities.includes('audit_fanout.apply'), true);

const provider = createAuditFanoutRuntimeProvider({
  ...providerInterface,
  async dispatch(request) {
    this.validateDispatchRequest(request);
    return { schema: this.operations.dispatch.result_schema, status: 'accepted' };
  },
  async apply(request) {
    this.validateApplyRequest(request);
    return { schema: this.operations.apply.result_schema, status: 'applied' };
  },
});

assert.equal(validateAuditFanoutRuntimeProvider(provider), provider);
assert.equal(typeof provider.dispatch, 'function');
assert.equal(typeof provider.apply, 'function');

const dispatchRequest = {
  schema: AUDIT_FANOUT_DISPATCH_REQUEST_SCHEMA,
  plan: {
    task_requests: [
      { id: 'audit-task-1', group_key: 'docs' },
    ],
  },
};
assert.equal(validateAuditFanoutDispatchRequest(dispatchRequest), dispatchRequest);

const applyRequest = {
  schema: AUDIT_FANOUT_APPLY_REQUEST_SCHEMA,
  artifact: { id: 'artifact-1' },
};
assert.equal(validateAuditFanoutApplyRequest(applyRequest), applyRequest);

assert.throws(
  () => createAuditFanoutRuntimeProvider({ ...providerInterface, dispatch() {} }),
  /apply function is required/
);
assert.throws(
  () => validateAuditFanoutDispatchRequest({ schema: AUDIT_FANOUT_DISPATCH_REQUEST_SCHEMA, plan: {} }),
  /plan\.task_requests must be an array/
);
assert.throws(
  () => validateAuditFanoutApplyRequest({ schema: AUDIT_FANOUT_APPLY_REQUEST_SCHEMA }),
  /artifact, change_artifact, or apply_request is required/
);

assert.equal(typeof wordpress.auditFanoutRuntimeProviderInterface, 'function');
assert.equal(wordpress.AUDIT_FANOUT_RUNTIME_PROVIDER_SCHEMA, AUDIT_FANOUT_RUNTIME_PROVIDER_SCHEMA);

process.stdout.write('audit fanout runtime provider smoke passed\n');
