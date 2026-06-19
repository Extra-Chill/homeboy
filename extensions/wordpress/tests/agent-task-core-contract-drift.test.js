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
const { providerContract } = require('../../agent-runtimes/wp-codebox');

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
assert.deepEqual(AGENT_TASK_OUTCOME_STATUSES, providerFields.outcome_statuses);
assert.deepEqual(AGENT_TASK_FAILURE_CLASSIFICATIONS, providerFields.failure_classifications);
assert.deepEqual(AGENT_TASK_REDACTED_METADATA_KEYS, providerFields.redacted_metadata_keys);
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
assert.deepEqual(providerContract(), wpCodeboxProvider);

process.stdout.write('Agent task core contract drift check passed\n');
