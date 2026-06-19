'use strict';

/* eslint-disable camelcase */

const AUDIT_FANOUT_RUNTIME_PROVIDER_SCHEMA = 'homeboy/audit-fanout-runtime-provider/v1';
const AUDIT_FANOUT_DISPATCH_REQUEST_SCHEMA = 'homeboy/audit-fanout-dispatch-request/v1';
const AUDIT_FANOUT_DISPATCH_RESULT_SCHEMA = 'homeboy/audit-fanout-dispatch-result/v1';
const AUDIT_FANOUT_APPLY_REQUEST_SCHEMA = 'homeboy/audit-fanout-apply-request/v1';
const AUDIT_FANOUT_APPLY_RESULT_SCHEMA = 'homeboy/audit-fanout-apply-result/v1';

const DEFAULT_AUDIT_FANOUT_RUNTIME_PROVIDER_ID = 'homeboy.audit-fanout-runtime-provider';
const DEFAULT_CAPABILITIES = Object.freeze([
  'audit_fanout.dispatch',
  'audit_fanout.apply',
]);

function auditFanoutRuntimeProviderInterface(options = {}) {
  const operations = options.operations && typeof options.operations === 'object' ? options.operations : {};

  return stripUndefined({
    schema: AUDIT_FANOUT_RUNTIME_PROVIDER_SCHEMA,
    id: text(options.id) || DEFAULT_AUDIT_FANOUT_RUNTIME_PROVIDER_ID,
    label: text(options.label) || 'Audit fanout runtime provider',
    backend: text(options.backend) || undefined,
    runtime: text(options.runtime) || undefined,
    capabilities: normalizeArray(options.capabilities, DEFAULT_CAPABILITIES),
    operations: {
      dispatch: {
        request_schema: AUDIT_FANOUT_DISPATCH_REQUEST_SCHEMA,
        result_schema: AUDIT_FANOUT_DISPATCH_RESULT_SCHEMA,
        ...(operations.dispatch || {}),
      },
      apply: {
        request_schema: AUDIT_FANOUT_APPLY_REQUEST_SCHEMA,
        result_schema: AUDIT_FANOUT_APPLY_RESULT_SCHEMA,
        ...(operations.apply || {}),
      },
    },
    metadata: objectOrUndefined(options.metadata),
  });
}

function createAuditFanoutRuntimeProvider(implementation = {}, options = {}) {
  const provider = {
    ...auditFanoutRuntimeProviderInterface({ ...implementation, ...options }),
    dispatch: implementation.dispatch,
    apply: implementation.apply,
    validateDispatchRequest: implementation.validateDispatchRequest || validateAuditFanoutDispatchRequest,
    validateApplyRequest: implementation.validateApplyRequest || validateAuditFanoutApplyRequest,
  };

  validateAuditFanoutRuntimeProvider(provider);
  return provider;
}

function validateAuditFanoutRuntimeProvider(provider) {
  if (!provider || typeof provider !== 'object' || Array.isArray(provider)) {
    throw new Error('audit fanout runtime provider must be an object');
  }
  if (provider.schema !== AUDIT_FANOUT_RUNTIME_PROVIDER_SCHEMA) {
    throw new Error(`audit fanout runtime provider schema must be ${AUDIT_FANOUT_RUNTIME_PROVIDER_SCHEMA}`);
  }
  if (!text(provider.id)) {
    throw new Error('audit fanout runtime provider id is required');
  }
  if (!provider.operations || typeof provider.operations !== 'object') {
    throw new Error('audit fanout runtime provider operations are required');
  }
  validateOperation(provider.operations.dispatch, 'dispatch', AUDIT_FANOUT_DISPATCH_REQUEST_SCHEMA, AUDIT_FANOUT_DISPATCH_RESULT_SCHEMA);
  validateOperation(provider.operations.apply, 'apply', AUDIT_FANOUT_APPLY_REQUEST_SCHEMA, AUDIT_FANOUT_APPLY_RESULT_SCHEMA);
  if (typeof provider.dispatch !== 'function') {
    throw new Error('audit fanout runtime provider dispatch function is required');
  }
  if (typeof provider.apply !== 'function') {
    throw new Error('audit fanout runtime provider apply function is required');
  }
  if (typeof provider.validateDispatchRequest !== 'function') {
    throw new Error('audit fanout runtime provider validateDispatchRequest function is required');
  }
  if (typeof provider.validateApplyRequest !== 'function') {
    throw new Error('audit fanout runtime provider validateApplyRequest function is required');
  }
  return provider;
}

function validateAuditFanoutDispatchRequest(request) {
  validateRequestObject(request, 'audit fanout dispatch request', AUDIT_FANOUT_DISPATCH_REQUEST_SCHEMA);
  if (!request.plan || typeof request.plan !== 'object' || Array.isArray(request.plan)) {
    throw new Error('audit fanout dispatch request plan is required');
  }
  if (!Array.isArray(request.plan.task_requests)) {
    throw new Error('audit fanout dispatch request plan.task_requests must be an array');
  }
  return request;
}

function validateAuditFanoutApplyRequest(request) {
  validateRequestObject(request, 'audit fanout apply request', AUDIT_FANOUT_APPLY_REQUEST_SCHEMA);
  if (!request.artifact && !request.change_artifact && !request.apply_request) {
    throw new Error('audit fanout apply request artifact, change_artifact, or apply_request is required');
  }
  return request;
}

function validateOperation(operation, name, requestSchema, resultSchema) {
  if (!operation || typeof operation !== 'object' || Array.isArray(operation)) {
    throw new Error(`audit fanout runtime provider ${name} operation is required`);
  }
  if (operation.request_schema !== requestSchema) {
    throw new Error(`audit fanout runtime provider ${name} request_schema must be ${requestSchema}`);
  }
  if (operation.result_schema !== resultSchema) {
    throw new Error(`audit fanout runtime provider ${name} result_schema must be ${resultSchema}`);
  }
}

function validateRequestObject(request, label, schema) {
  if (!request || typeof request !== 'object' || Array.isArray(request)) {
    throw new Error(`${label} must be an object`);
  }
  if (request.schema !== schema) {
    throw new Error(`${label} schema must be ${schema}`);
  }
}

function objectOrUndefined(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return undefined;
  }
  return value;
}

function normalizeArray(value, fallback = []) {
  if (!Array.isArray(value)) {
    return [...fallback];
  }
  return value.filter((entry) => text(entry));
}

function stripUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function text(value) {
  if (typeof value === 'string') {
    return value.trim();
  }
  return '';
}

module.exports = {
  AUDIT_FANOUT_APPLY_REQUEST_SCHEMA,
  AUDIT_FANOUT_APPLY_RESULT_SCHEMA,
  AUDIT_FANOUT_DISPATCH_REQUEST_SCHEMA,
  AUDIT_FANOUT_DISPATCH_RESULT_SCHEMA,
  AUDIT_FANOUT_RUNTIME_PROVIDER_SCHEMA,
  DEFAULT_AUDIT_FANOUT_RUNTIME_PROVIDER_ID,
  auditFanoutRuntimeProviderInterface,
  createAuditFanoutRuntimeProvider,
  validateAuditFanoutApplyRequest,
  validateAuditFanoutDispatchRequest,
  validateAuditFanoutRuntimeProvider,
};
