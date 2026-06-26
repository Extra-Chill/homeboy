'use strict';

const HOST_RECORD_SUCCESS_STATUSES = Object.freeze(['completed']);
const HOST_RECORD_FAILURE_STATUSES = Object.freeze(['failed', 'missing_record']);
const HOST_RUN_SUCCESS_STATUSES = Object.freeze(['completed']);
const HOST_RUN_FAILURE_STATUSES = Object.freeze(['failed']);
const HOST_RUN_PENDING_STATUSES = Object.freeze(['incomplete']);
const PROVIDER_SUCCESS_STATUSES = Object.freeze(['accepted', 'completed', 'passed', 'success', 'succeeded', 'no_op']);
const PROVIDER_FAILURE_STATUSES = Object.freeze(['cancelled', 'failed', 'provider_error', 'timeout', 'unable_to_remediate']);

function normalizeHostRecordStatus(value = {}, options = {}) {
  const record = plainObject(value) ? value : { status: value };
  const status = text(record.status || record.state);
  if (HOST_RECORD_SUCCESS_STATUSES.includes(status) || HOST_RECORD_FAILURE_STATUSES.includes(status)) {
    return status;
  }
  if (record.success === true || providerStatusSucceeded(status) || providerStatusSucceeded(record.outcome?.status || record.outcome_status || record.provider_status)) {
    return 'completed';
  }
  if (options.pending === true && ['pending', 'running', 'started', 'in_progress'].includes(status)) {
    return 'missing_record';
  }
  return 'failed';
}

function normalizeHostRunStatus(value = {}, options = {}) {
  const run = plainObject(value) ? value : { status: value };
  const status = text(run.status || run.state);
  if (HOST_RUN_SUCCESS_STATUSES.includes(status) || HOST_RUN_FAILURE_STATUSES.includes(status) || HOST_RUN_PENDING_STATUSES.includes(status)) {
    return status;
  }
  if (run.incomplete === true || options.incomplete === true || ['pending', 'running', 'started', 'in_progress'].includes(status)) {
    return 'incomplete';
  }
  if (run.success === true || run.accepted === true || providerStatusSucceeded(status)) {
    return 'completed';
  }
  return 'failed';
}

function normalizeAgentTaskOutcomeStatus(result = {}, options = {}) {
  const status = text(options.status || result.status || result.state);
  const exitStatus = options.exitStatus ?? options.exit_status ?? 0;
  if (['provider_error', 'timeout', 'unable_to_remediate', 'failed'].includes(status)) {
    return status;
  }
  if (result.provider_error) {
    return 'provider_error';
  }
  if (result.timeout) {
    return 'timeout';
  }
  if (result.unable_to_remediate) {
    return 'unable_to_remediate';
  }
  if (result.success === false || exitStatus !== 0) {
    return 'failed';
  }
  if (status === 'no_op' || result.outcome === 'no_op' || result.no_op) {
    return 'no_op';
  }
  if (providerStatusSucceeded(status) || result.success === true) {
    return 'succeeded';
  }
  return Object.keys(result || {}).length > 0 ? 'provider_error' : 'failed';
}

function providerStatusSucceeded(status) {
  return PROVIDER_SUCCESS_STATUSES.includes(text(status));
}

function providerStatusFailed(status) {
  return PROVIDER_FAILURE_STATUSES.includes(text(status));
}

function text(value) {
  return typeof value === 'string' ? value.trim() : '';
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

module.exports = {
  HOST_RECORD_FAILURE_STATUSES,
  HOST_RECORD_SUCCESS_STATUSES,
  HOST_RUN_FAILURE_STATUSES,
  HOST_RUN_PENDING_STATUSES,
  HOST_RUN_SUCCESS_STATUSES,
  PROVIDER_FAILURE_STATUSES,
  PROVIDER_SUCCESS_STATUSES,
  normalizeAgentTaskOutcomeStatus,
  normalizeHostRecordStatus,
  normalizeHostRunStatus,
  providerStatusFailed,
  providerStatusSucceeded,
};
