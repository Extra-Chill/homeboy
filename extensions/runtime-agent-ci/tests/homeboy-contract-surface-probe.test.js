'use strict';

const assert = require('node:assert/strict');

const {
  LOCAL_ARTIFACT_MANIFEST_CONSTANTS,
  probeHomeboyContractSurface,
} = require('../lib/homeboy-contract-surface-probe.cjs');
const {
  checkHomeboyContractExportFixtures,
  compareSchemaCatalogFixture,
} = require('../scripts/check-homeboy-contract-export-fixtures.cjs');
const {
  ARTIFACT_MANIFEST_FILE,
  ARTIFACT_MANIFEST_SCHEMA,
  RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
} = require('../lib/runtime-contracts.cjs');

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
        constants: LOCAL_ARTIFACT_MANIFEST_CONSTANTS,
        contract_id: 'artifact-manifest',
        schema: 'homeboy/contract-constants/v1',
      },
    }),
    stderr: '',
  }),
});

assert.equal(releasedContractShape.status, 'passed');
assert.deepEqual(releasedContractShape.argv, ['contract', 'constants', 'artifact-manifest']);

const nestedContractShape = probeHomeboyContractSurface({
  homeboyCommand: 'homeboy',
  spawnSync: () => ({
    status: 0,
    stdout: JSON.stringify({
      artifact_manifest: {
        schema: ARTIFACT_MANIFEST_SCHEMA,
        file: ARTIFACT_MANIFEST_FILE,
      },
      artifact_paths: {
        schema: 'homeboy/runtime-agent-artifact-paths/v1',
      },
      runner_artifact_manifest_ref: {
        schema: RUNNER_ARTIFACT_MANIFEST_REF_SCHEMA,
      },
    }),
    stderr: '',
  }),
});

assert.equal(nestedContractShape.status, 'passed');

const drift = probeHomeboyContractSurface({
  homeboyCommand: 'homeboy',
  spawnSync: () => ({
    status: 0,
    stdout: JSON.stringify({
      constants: {
        ...LOCAL_ARTIFACT_MANIFEST_CONSTANTS,
        schema_id: 'homeboy/artifact-manifest/v2',
      },
    }),
    stderr: '',
  }),
});

assert.equal(drift.status, 'failed');
assert.match(drift.message, /schema_id expected homeboy\/artifact-manifest\/v1/);

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
