'use strict';

const assert = require('node:assert/strict');

const {
  CORE_PUBLISHED_CONTRACT_CONSTANTS,
  LOCAL_ARTIFACT_MANIFEST_CONSTANTS,
  probeHomeboyContractSurface,
} = require('../lib/homeboy-contract-surface-probe.cjs');
const {
  RUN_OUTCOME_ENVELOPE_SCHEMA,
  RUNTIME_CONTRACT_CONSTANTS,
  runtimeContractConstantsFromHomeboyOutput,
} = require('../lib/runtime-contracts.cjs');
const {
  checkHomeboyContractExportFixtures,
  compareSchemaCatalogFixture,
} = require('../scripts/check-homeboy-contract-export-fixtures.cjs');
const missing = probeHomeboyContractSurface({
  homeboyCommand: 'homeboy',
  spawnSync: () => ({ status: 2, stdout: '', stderr: "error: unrecognized subcommand 'constants'" }),
});

assert.equal(missing.status, 'skipped');
assert.match(missing.message, /skipped/);

const releasedContractShape = probeHomeboyContractSurface({
  homeboyCommand: 'homeboy',
  spawnSync: () => ({
    status: 0,
    stdout: JSON.stringify({
      success: true,
      data: {
        constants: CORE_PUBLISHED_CONTRACT_CONSTANTS,
        contract_id: 'all',
        schema: 'homeboy/contract-constants/v1',
      },
    }),
    stderr: '',
  }),
});

assert.equal(releasedContractShape.status, 'passed');
assert.deepEqual(releasedContractShape.argv, ['contract', 'constants', 'all']);
assert.equal(RUN_OUTCOME_ENVELOPE_SCHEMA, 'homeboy/run-outcome-envelope/v1');
assert.equal(RUNTIME_CONTRACT_CONSTANTS.run_outcome_envelope.schema_id, RUN_OUTCOME_ENVELOPE_SCHEMA);

assert.deepEqual(runtimeContractConstantsFromHomeboyOutput({
  data: {
    constants: {
      runnerExecutionRecord: { schema_id: 'homeboy/runner-execution-record/v1' },
      pathMaterializationPlan: { schema_id: 'homeboy/path-materialization-plan/v1' },
      runOutcomeEnvelope: { schema_id: 'homeboy/run-outcome-envelope/v1' },
    },
  },
}), {
  runner_execution_record: { schema_id: 'homeboy/runner-execution-record/v1' },
  path_materialization_plan: { schema_id: 'homeboy/path-materialization-plan/v1' },
  run_outcome_envelope: { schema_id: 'homeboy/run-outcome-envelope/v1' },
});

const singleContractShape = probeHomeboyContractSurface({
  homeboyCommand: 'homeboy',
  spawnSync: (_command, argv) => ({
    status: argv[2] === 'artifact-manifest' ? 0 : 2,
    stdout: argv[2] === 'artifact-manifest' ? JSON.stringify({
      success: true,
      data: {
        constants: LOCAL_ARTIFACT_MANIFEST_CONSTANTS,
        contract_id: 'artifact-manifest',
        schema: 'homeboy/contract-constants/v1',
      },
    }) : '',
    stderr: '',
  }),
});

assert.equal(singleContractShape.status, 'passed');
assert.deepEqual(singleContractShape.argv, ['contract', 'constants', 'artifact-manifest']);

const drift = probeHomeboyContractSurface({
  homeboyCommand: 'homeboy',
  spawnSync: () => ({
    status: 0,
    stdout: JSON.stringify({
      constants: {
        secret_env_plan: CORE_PUBLISHED_CONTRACT_CONSTANTS.secret_env_plan,
        ...LOCAL_ARTIFACT_MANIFEST_CONSTANTS,
        schema_id: 'homeboy/artifact-manifest/v2',
      },
    }),
    stderr: '',
  }),
});

assert.equal(drift.status, 'failed');
assert.match(drift.message, /artifact_manifest\.schema_id expected homeboy\/artifact-manifest\/v1/);

const fixtureDrift = compareSchemaCatalogFixture({
  schema: 'homeboy/contract-schema-catalog/v1',
  contracts: [
    {
      id: 'homeboy/runner-workload/v1',
      example: { schema: 'homeboy/runner-workload/v1' },
    },
  ],
}, {
  schema: 'homeboy/contract-schema-catalog/v1',
  contract_ids: ['homeboy/runner-workload/v1'],
  examples: {
    'homeboy/runner-workload/v1': { schema: 'homeboy/runner-workload/v2' },
  },
});

assert.match(fixtureDrift.join('; '), /homeboy\/runner-workload\/v2/);

const live = probeHomeboyContractSurface();
if (live.status === 'skipped') {
  process.stdout.write(`${live.message}\n`);
} else {
  assert.equal(live.status, 'passed', live.message);
  process.stdout.write(`${live.message}\n`);
}

const exportFixtures = checkHomeboyContractExportFixtures();
if (exportFixtures.status === 'skipped') {
  process.stdout.write(`${exportFixtures.message}\n`);
} else {
  assert.equal(exportFixtures.status, 'passed', exportFixtures.message);
  process.stdout.write(`${exportFixtures.message}\n`);
}

process.stdout.write('Homeboy contract surface probe check passed\n');
