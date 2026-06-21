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

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-inputs-'));
const runnerTemp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-runner-'));
fs.mkdirSync(path.join(workspace, '.ci/wp-codebox/packages/cli/dist'), { recursive: true });
fs.writeFileSync(path.join(workspace, '.ci/wp-codebox/packages/cli/dist/index.js'), '#!/usr/bin/env node\n');

const config = buildConfig({
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  WORKLOAD_ID: 'legacy-workload-id',
  WORKLOAD_LABEL: 'Legacy workload label',
  TARGET_REPO: 'Extra-Chill/example',
  RUNTIME: 'wp-codebox',
  PROFILE: 'codebox-profile',
  RUNTIME_PROFILES: JSON.stringify({
    'codebox-profile': {
      schema: 'wp-codebox/runtime-profile/v1',
      id: 'codebox-profile',
    },
  }),
  WORKLOAD: JSON.stringify({
    schema: 'wp-codebox/workload/v1',
    id: 'codebox-workload',
    label: 'Codebox workload',
  }),
  TOOL_PROFILE: JSON.stringify({
    schema: 'homeboy/runtime-tool-profile/v1',
    tools: { workspace_read: true },
  }),
  ARTIFACT_DECLARATIONS: JSON.stringify([
    { name: 'packet', kind: 'application/vnd.example.packet+json', required: true },
    'transcript',
  ]),
});

assert.equal(config.workload_id, 'codebox-workload');
assert.equal(config.workload_label, 'Codebox workload');
assert.deepEqual(config.workload, {
  schema: 'wp-codebox/workload/v1',
  id: 'codebox-workload',
  label: 'Codebox workload',
});
assert.deepEqual(config.sandbox_tool_policy, {
  schema: 'homeboy/runtime-tool-profile/v1',
  tools: { workspace_read: true },
});
assert.deepEqual(config.artifact_declarations, [
  {
    schema: 'wp-codebox/artifact-declaration/v1',
    name: 'packet',
    type: 'application/vnd.example.packet+json',
    required: true,
  },
  {
    schema: 'wp-codebox/artifact-declaration/v1',
    name: 'transcript',
    required: true,
  },
]);

const legacyToolPolicyConfig = buildConfig({
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  WORKLOAD_ID: 'legacy-tool-policy',
  TARGET_REPO: 'Extra-Chill/example',
  RUNTIME: 'wp-codebox',
  PROFILE: 'codebox-profile',
  RUNTIME_PROFILES: JSON.stringify({
    'codebox-profile': {
      schema: 'wp-codebox/runtime-profile/v1',
      id: 'codebox-profile',
    },
  }),
  TOOL_POLICY: JSON.stringify({ tools: { workspace_write: false } }),
});
assert.deepEqual(legacyToolPolicyConfig.sandbox_tool_policy, { tools: { workspace_write: false } });

console.log('runtime agent full-run Codebox inputs smoke passed');
