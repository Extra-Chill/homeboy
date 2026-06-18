#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { buildConfig } = require(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/build-runner-config.cjs'));

const runtimeProfile = {
  schema: 'homeboy/runtime-profile/v1',
  id: 'datamachine-agent-ci',
  runtime_task_ability: 'datamachine/run-agent-bundle',
  ability_requirements: ['datamachine/run-agent-bundle'],
};

function baseEnv(workspace, runnerTemp) {
  return {
    GITHUB_WORKSPACE: workspace,
    RUNNER_TEMP: runnerTemp,
    WORKLOAD_ID: 'datamachine-agent-ci-smoke',
    TARGET_REPO: 'example/repo',
    RUNTIME_PROFILE: runtimeProfile.id,
    RUNTIME_PROFILES: JSON.stringify({ [runtimeProfile.id]: runtimeProfile }),
  };
}

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-datamachine-agent-ci-config-'));
const codeboxWorkspace = path.join(root, 'codebox-workspace');
const fakeWorkspace = path.join(root, 'fake-workspace');
const runnerTemp = path.join(root, 'runner-temp');
fs.mkdirSync(path.join(codeboxWorkspace, '.ci/wp-codebox/packages/cli/dist'), { recursive: true });
fs.mkdirSync(fakeWorkspace, { recursive: true });
fs.mkdirSync(runnerTemp, { recursive: true });
fs.writeFileSync(path.join(codeboxWorkspace, '.ci/wp-codebox/packages/cli/dist/index.js'), '#!/usr/bin/env node\n');

const codeboxConfig = buildConfig({
  ...baseEnv(codeboxWorkspace, runnerTemp),
  RUNTIME_PROVIDER: 'wp-codebox',
});

assert.equal(codeboxConfig.runtime_profiles[runtimeProfile.id].schema, 'wp-codebox/runtime-profile/v1');
assert.equal(codeboxConfig.runtime_profiles[runtimeProfile.id].homeboy_profile_schema, 'homeboy/runtime-profile/v1');
assert.equal(codeboxConfig.runtime_requirements.schema, 'wp-codebox/runtime-profile/v1');
assert.equal(codeboxConfig.runtime_bin, path.join(codeboxWorkspace, '.ci/wp-codebox/packages/cli/dist/index.js'));

const fakeConfig = buildConfig({
  ...baseEnv(fakeWorkspace, runnerTemp),
  RUNTIME_PROVIDER: 'fake-runtime',
});

assert.equal(fakeConfig.runtime_profiles[runtimeProfile.id].schema, 'homeboy/runtime-profile/v1');
assert.deepEqual(fakeConfig.runtime_requirements, runtimeProfile);
assert.equal(Object.hasOwn(fakeConfig, 'runtime_bin'), false);
assert.equal(JSON.stringify(fakeConfig).includes('wp-codebox/'), false);

console.log('datamachine agent CI build runner config smoke passed');
