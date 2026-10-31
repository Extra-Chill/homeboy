#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(repoRoot, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const { buildConfig, projectRuntimeConfig } = require(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/build-runner-config.cjs'));

const genericProjection = projectRuntimeConfig({
  env: {
    ARTIFACT_DECLARATIONS: JSON.stringify([{ name: 'packet', kind: 'application/vnd.example.packet+json' }]),
  },
  runtime: {
    id: 'generic-runtime',
    manifest: {
      schema: 'homeboy/agent-runtime-manifest/v1',
      id: 'generic-runtime',
      runner_config_projection: {
        transcript_guest_dir_template: '/runtime/{workload_id}/transcript',
        runtime_fields: {
          runtime_example_version: { env: 'RUNTIME_EXAMPLE_VERSION', default: '1.0' },
        },
      },
    },
  },
  workspace: '/tmp/workspace',
  componentId: 'example-component',
  componentPath: '/tmp/workspace',
  workloadId: 'projection-smoke',
  runnerWorkspaceGuestCheckout: '/workspace/example-component',
});

assert.deepEqual(genericProjection.artifact_declarations, [
  { name: 'packet', kind: 'application/vnd.example.packet+json' },
]);
assert.equal(genericProjection.transcript_dir, '/runtime/projection-smoke/transcript');
assert.equal(genericProjection.transcript_dir.includes('/wordpress/wp-content/plugins'), false);
assert.deepEqual(genericProjection.wp_config_defines, {});
assert.deepEqual(genericProjection.runtime_fields, { runtime_example_version: '1.0' });

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-projection-'));
const runnerTemp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-projection-runner-'));
const binDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-bin-'));
const wpCodeboxBin = path.join(binDir, 'wp-codebox');
fs.writeFileSync(wpCodeboxBin, '#!/usr/bin/env sh\nexit 0\n');
fs.chmodSync(wpCodeboxBin, 0o755);

const codeboxConfig = buildConfig({
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  WORKLOAD_ID: 'codebox-projection',
  COMPONENT_ID: 'example',
  TARGET_REPO: 'Extra-Chill/example',
  RUNTIME: 'wp-codebox',
  PATH: `${binDir}${path.delimiter}${process.env.PATH || ''}`,
  PROFILE: 'codebox-profile',
  ARTIFACT_DECLARATIONS: JSON.stringify([
    { name: 'packet', kind: 'application/vnd.example.packet+json', required: true },
    'transcript',
  ]),
  EXTRA_WP_CONFIG_DEFINES: JSON.stringify({ CODEBOX_DEFINE: true }),
});

assert.deepEqual(codeboxConfig.artifact_declarations, [
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
assert.equal(codeboxConfig.transcript_dir, '/wordpress/wp-content/plugins/example/runtime-agent-artifacts/codebox-projection');
assert.deepEqual(codeboxConfig.wp_config_defines, { CODEBOX_DEFINE: true });
assert.equal(codeboxConfig.runtime_wordpress_version, '7.0');

const codeboxOverrideConfig = buildConfig({
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  WORKLOAD_ID: 'codebox-projection-override',
  COMPONENT_ID: 'example',
  TARGET_REPO: 'Extra-Chill/example',
  RUNTIME: 'wp-codebox',
  PATH: `${binDir}${path.delimiter}${process.env.PATH || ''}`,
  PROFILE: 'codebox-profile',
  RUNTIME_WORDPRESS_VERSION: 'nightly',
});

assert.equal(codeboxOverrideConfig.runtime_wordpress_version, 'nightly');

console.log('runtime agent full-run projection boundary smoke passed');
