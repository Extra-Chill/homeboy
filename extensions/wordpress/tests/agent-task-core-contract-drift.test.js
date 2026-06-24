'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const {
  AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA,
  AGENT_TASK_ARTIFACT_SCHEMA,
  AGENT_TASK_EVIDENCE_REF_SCHEMA,
  AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA,
  AGENT_TASK_FAILURE_CLASSIFICATIONS,
  AGENT_TASK_OUTCOME_SCHEMA,
  AGENT_TASK_OUTCOME_STATUSES,
  AGENT_TASK_REDACTED_METADATA_KEYS,
  AGENT_TASK_REQUEST_SCHEMA,
  agentTaskProviderContractFields,
} = require('../../runtime-agent-ci/lib/agent-task-provider-contract');
const {
  FANOUT_RECONCILE_PLAN_SCHEMA,
  FANOUT_RECONCILE_RECORD_STATUSES,
  FANOUT_RECONCILE_RUN_SCHEMA,
  FANOUT_RECONCILE_RUN_STATUSES,
} = require('../../runtime-agent-ci/lib/fanout-reconcile-runner');
const {
  GENERIC_FANOUT_RECONCILE_CONFIG_SCHEMA,
  GENERIC_FANOUT_RECONCILE_RECONCILIATION_SCHEMA,
  GENERIC_FANOUT_RECONCILE_RESULT_SCHEMA,
  GENERIC_FANOUT_RECONCILE_SUCCESS_STATUSES,
  GENERIC_FINDING_PACKET_FANOUT_CONFIG_SCHEMA,
} = require('../../runtime-agent-ci/lib/generic-fanout-reconcile-workflow');

const contract = JSON.parse(fs.readFileSync(path.join(
  __dirname,
  '..',
  '..',
  'agent-runtimes',
  'fixtures',
  'homeboy-agent-task-core-contract.json'
), 'utf8'));
const wpCodeboxManifest = JSON.parse(fs.readFileSync(path.join(
  __dirname,
  '..',
  '..',
  'agent-runtimes',
  'wp-codebox',
  'wp-codebox.json'
), 'utf8'));
const wpCodeboxProvider = wpCodeboxManifest.agent_task_executors[0];
const providerFields = contract.provider_capability;

assert.equal(contract.schema, 'homeboy/agent-task-core-contract/v1');
assert.equal(AGENT_TASK_REQUEST_SCHEMA, contract.schemas.request);
assert.equal(AGENT_TASK_OUTCOME_SCHEMA, contract.schemas.outcome);
assert.equal(AGENT_TASK_ARTIFACT_SCHEMA, contract.schemas.artifact);
assert.equal(AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA, contract.schemas.artifact_declaration);
assert.equal(AGENT_TASK_EVIDENCE_REF_SCHEMA, contract.schemas.evidence_ref);
assert.equal(AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA, contract.schemas.provider);
assert.equal(GENERIC_FANOUT_RECONCILE_CONFIG_SCHEMA, contract.schemas.fanout_reconcile_config);
assert.equal(FANOUT_RECONCILE_PLAN_SCHEMA, contract.schemas.fanout_reconcile_plan);
assert.equal(FANOUT_RECONCILE_RUN_SCHEMA, contract.schemas.fanout_reconcile_run);
assert.equal(GENERIC_FANOUT_RECONCILE_RESULT_SCHEMA, contract.schemas.fanout_reconcile_result);
assert.equal(GENERIC_FANOUT_RECONCILE_RECONCILIATION_SCHEMA, contract.schemas.fanout_reconcile_reconciliation);
assert.equal(GENERIC_FINDING_PACKET_FANOUT_CONFIG_SCHEMA, contract.schemas.finding_packet_fanout_config);
assert.deepEqual(AGENT_TASK_OUTCOME_STATUSES, providerFields.outcome_statuses);
assert.deepEqual(AGENT_TASK_FAILURE_CLASSIFICATIONS, providerFields.failure_classifications);
assert.deepEqual(AGENT_TASK_REDACTED_METADATA_KEYS, providerFields.redacted_metadata_keys);
assert.deepEqual(FANOUT_RECONCILE_RECORD_STATUSES, contract.enums.fanout_record_status);
assert.deepEqual(FANOUT_RECONCILE_RUN_STATUSES, contract.enums.fanout_run_status);
assert.deepEqual(GENERIC_FANOUT_RECONCILE_SUCCESS_STATUSES, contract.orchestration.fanout_success_statuses);
assert.deepEqual(Object.keys(contract.orchestration.agent_task_outcome_to_fanout_record_status), AGENT_TASK_OUTCOME_STATUSES);
assert.deepEqual(
  Object.values(contract.orchestration.agent_task_outcome_to_fanout_record_status).filter((status) => !FANOUT_RECONCILE_RECORD_STATUSES.includes(status)),
  []
);
assert.deepEqual(agentTaskProviderContractFields(), {
  request_schema: providerFields.request_schema,
  outcome_schema: providerFields.outcome_schema,
  request_required_fields: providerFields.request_required_fields,
  outcome_statuses: providerFields.outcome_statuses,
  failure_classifications: providerFields.failure_classifications,
  redacted_metadata_keys: providerFields.redacted_metadata_keys,
});
assert.equal(wpCodeboxProvider.schema, providerFields.provider_schema);
assert.equal(wpCodeboxProvider.request_schema, providerFields.request_schema);
assert.equal(wpCodeboxProvider.outcome_schema, providerFields.outcome_schema);
assert.deepEqual(wpCodeboxProvider.request_required_fields, providerFields.request_required_fields);
assert.deepEqual(wpCodeboxProvider.outcome_statuses, providerFields.outcome_statuses);
assert.deepEqual(wpCodeboxProvider.failure_classifications, providerFields.failure_classifications);
assert.deepEqual(wpCodeboxProvider.redacted_metadata_keys, providerFields.redacted_metadata_keys);

process.stdout.write('Agent task core contract drift check passed\n');
