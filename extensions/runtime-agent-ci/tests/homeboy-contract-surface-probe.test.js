'use strict';

const assert = require('node:assert/strict');

const {
  MIRRORED_ARTIFACT_MANIFEST_CONSTANTS,
  probeHomeboyContractSurface,
} = require('../lib/homeboy-contract-surface-probe.cjs');

const missing = probeHomeboyContractSurface({
  homeboyCommand: 'homeboy',
  spawnSync: () => ({ status: 2, stdout: '', stderr: "error: unrecognized subcommand 'contract'" }),
});

assert.equal(missing.status, 'skipped');
assert.match(missing.message, /skipped/);

const failedFirstCommand = probeHomeboyContractSurface({
  homeboyCommand: 'homeboy',
  spawnSync: (_command, argv) => {
    if (argv[1] === 'constants') {
      return { status: 2, stdout: '', stderr: "error: unrecognized subcommand 'constants'" };
    }
    return {
      status: 0,
      stdout: JSON.stringify({ constants: MIRRORED_ARTIFACT_MANIFEST_CONSTANTS }),
      stderr: '',
    };
  },
});

assert.equal(failedFirstCommand.status, 'passed');
assert.deepEqual(failedFirstCommand.argv, ['contract', 'show', '--format=json']);

const nestedContractShape = probeHomeboyContractSurface({
  homeboyCommand: 'homeboy',
  spawnSync: () => ({
    status: 0,
    stdout: JSON.stringify({
      artifact_manifest: {
        schema: 'homeboy/artifact-manifest/v1',
        file: 'homeboy-artifact-manifest.json',
      },
      artifact_paths: {
        schema: 'homeboy/runtime-agent-artifact-paths/v1',
      },
      runner_artifact_manifest_ref: {
        schema: 'homeboy/runner-artifact-manifest-ref/v1',
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
        ...MIRRORED_ARTIFACT_MANIFEST_CONSTANTS,
        ARTIFACT_MANIFEST_SCHEMA: 'homeboy/artifact-manifest/v2',
      },
    }),
    stderr: '',
  }),
});

assert.equal(drift.status, 'failed');
assert.match(drift.message, /ARTIFACT_MANIFEST_SCHEMA expected homeboy\/artifact-manifest\/v1/);

const live = probeHomeboyContractSurface();
if (live.status === 'skipped') {
  process.stdout.write(`${live.message}\n`);
} else {
  assert.equal(live.status, 'passed', live.message);
  process.stdout.write(`${live.message}\n`);
}

process.stdout.write('Homeboy contract surface probe check passed\n');
