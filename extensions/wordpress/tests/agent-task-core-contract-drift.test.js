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
  providerSecretEnvRequirement,
} = require('../../agent-task-contracts');
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

const {
  FIXTURE_PATH,
  buildCoreContractFixture,
  canonicalJson,
  fetchCoreContractData,
} = require('../../agent-runtimes/fixtures/generate-homeboy-agent-task-core-contract.cjs');
const {
  CORE_CONTRACT_SCHEMA,
  CORE_CONTRACT_PATH,
  GENERATED_SCHEMA,
  OUTPUT_PATH,
  buildProviderContract,
  canonicalJson: canonicalProviderContractJson,
  readPinnedCoreContract,
} = require('../../agent-task-contracts/generate-agent-task-provider-contract.cjs');

const committedFixtureText = fs.readFileSync(FIXTURE_PATH, 'utf8');
const contract = JSON.parse(committedFixtureText);
const committedProviderContractText = fs.readFileSync(OUTPUT_PATH, 'utf8');
const generatedProviderContract = JSON.parse(committedProviderContractText);
const providerFields = contract.provider_capability;

assert.equal(contract.schema, 'homeboy/agent-task-core-contract/v1');
assert.equal(CORE_CONTRACT_PATH, FIXTURE_PATH);
assert.deepEqual(readPinnedCoreContract(), contract);
assert.equal(generatedProviderContract.schema, GENERATED_SCHEMA);
assert.equal(generatedProviderContract.core_contract_schema, CORE_CONTRACT_SCHEMA);
assert.deepEqual(generatedProviderContract.provider_capability, {
  failure_classifications: AGENT_TASK_FAILURE_CLASSIFICATIONS,
  outcome_statuses: AGENT_TASK_OUTCOME_STATUSES,
  redacted_metadata_keys: AGENT_TASK_REDACTED_METADATA_KEYS,
  request_required_fields: providerFields.request_required_fields,
});
assert.equal(AGENT_TASK_REQUEST_SCHEMA, contract.schemas.request);
assert.equal(AGENT_TASK_OUTCOME_SCHEMA, contract.schemas.outcome);
assert.equal(AGENT_TASK_ARTIFACT_SCHEMA, contract.schemas.artifact);
assert.equal(AGENT_TASK_ARTIFACT_DECLARATION_SCHEMA, contract.schemas.artifact_declaration);
assert.equal(AGENT_TASK_EVIDENCE_REF_SCHEMA, contract.schemas.evidence_ref);
assert.equal(AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA, contract.schemas.provider);
assert.equal(
  providerSecretEnvRequirement('example', 'EXAMPLE_TOKEN').schema,
  contract.schemas.secret_env_requirement
);
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
// The fixture is canonically serialized with sorted object keys, so compare the
// mapping's key set to the outcome statuses order-independently.
assert.deepEqual(
  Object.keys(contract.orchestration.agent_task_outcome_to_fanout_record_status).slice().sort(),
  AGENT_TASK_OUTCOME_STATUSES.slice().sort()
);
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

// Strong anti-drift guarantee: the fixture is a generated artifact derived from
// Homeboy core's published contract (`homeboy agent-task contract --format
// json`) merged with the extensions-owned fanout/orchestration overlay. When
// the homeboy binary is available, assert the committed fixture is byte-for-byte
// what the generator produces from core. This catches any core contract change
// (added, removed, renamed, or reordered keys) that the static checks above
// cannot see. Regenerate with:
//   node agent-runtimes/fixtures/generate-homeboy-agent-task-core-contract.cjs
const coreData = fetchCoreContractData();
if (coreData === null) {
  process.stdout.write(
    'Agent task core contract drift check passed (homeboy binary unavailable; '
      + 'core-region byte-equality check skipped)\n'
  );
} else {
  const expectedFixture = buildCoreContractFixture(coreData);
  assert.deepEqual(
    contract,
    expectedFixture,
    'Fixture drifted from core: regenerate with '
      + 'node agent-runtimes/fixtures/generate-homeboy-agent-task-core-contract.cjs'
  );
  assert.equal(
    committedFixtureText,
    canonicalJson(expectedFixture),
    'Fixture serialization drifted from the generator: regenerate with '
      + 'node agent-runtimes/fixtures/generate-homeboy-agent-task-core-contract.cjs'
  );
  const expectedProviderContract = buildProviderContract(coreData);
  assert.deepEqual(
    generatedProviderContract,
    expectedProviderContract,
    'Provider contract drifted from core: regenerate with '
      + 'node agent-task-contracts/generate-agent-task-provider-contract.cjs'
  );
  assert.equal(
    committedProviderContractText,
    canonicalProviderContractJson(expectedProviderContract),
    'Provider contract serialization drifted from the generator: regenerate with '
      + 'node agent-task-contracts/generate-agent-task-provider-contract.cjs'
  );
  process.stdout.write('Agent task core contract drift check passed (verified against core)\n');
}
