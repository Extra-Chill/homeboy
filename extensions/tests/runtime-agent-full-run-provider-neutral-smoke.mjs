#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { dependencyEntries, resolvePlan } = require(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/materialize-dependencies.cjs'));
const { buildConfig, providerBenchEnvFromManifest } = require(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/build-runner-config.cjs'));
const { normalizeProviderPlugin } = require(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/lib/common.cjs'));
const { resolveRuntimeProvider } = require(path.join(repoRoot, 'runtime-agent-ci/lib/runtime-provider-resolver.cjs'));

const baseEnv = {
  RUNTIME: 'local-shell',
  RUNTIME_DEPENDENCIES: '',
  VALIDATION_DEPENDENCIES: '',
  PROVIDER_PLUGIN: '{}',
  PROVIDER: 'openai',
};

assert.deepEqual(dependencyEntries(baseEnv), []);
assert.deepEqual(resolvePlan(dependencyEntries(baseEnv), true), []);
assert.deepEqual(normalizeProviderPlugin('{}', 'openai', true).provider_secret_env, {});

const explicitProviderPlugin = {
  repo: 'WordPress/ai-provider-for-openai',
  ref: 'trunk',
  path: '.',
  provider_secret_env: {
    connectors_ai_openai_api_key: 'PROVIDER_SECRET_1',
  },
};
assert.deepEqual(resolvePlan(dependencyEntries({
  ...baseEnv,
  PROVIDER_PLUGIN: JSON.stringify(explicitProviderPlugin),
}), true), [{
  repo: 'WordPress/ai-provider-for-openai',
  ref: 'trunk',
  target: '.ci/ai-provider-for-openai',
}]);
assert.deepEqual(normalizeProviderPlugin(JSON.stringify(explicitProviderPlugin), 'openai', true).provider_secret_env, {
  connectors_ai_openai_api_key: 'PROVIDER_SECRET_1',
});

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-provider-neutral-workspace-'));
const runnerTemp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-provider-neutral-runner-'));
const binDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-provider-neutral-bin-'));
const runtime = resolveRuntimeProvider('wp-codebox', { workspace });
assert.deepEqual(providerBenchEnvFromManifest(runtime, 'openai', {
  OPENAI_API_KEY: 'manifest-secret-value',
}), new Set(['OPENAI_API_KEY']));
assert.deepEqual(providerBenchEnvFromManifest(runtime, 'openai', {}), new Set(['OPENAI_API_KEY']));

const config = buildConfig({
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  WORKLOAD_ID: 'provider-neutral',
  TARGET_REPO: 'Extra-Chill/example',
  RUNTIME: 'local-shell',
  PATH: `${binDir}${path.delimiter}${process.env.PATH || ''}`,
  PROFILE: 'local-shell-profile',
  RUNTIME_PROFILES: JSON.stringify({
    'local-shell-profile': {
      id: 'local-shell-profile',
    },
  }),
  PROVIDER: 'openai',
  PROVIDER_PLUGIN: JSON.stringify(explicitProviderPlugin),
  PROVIDER_SECRET_1: 'secret-value',
  OPENAI_API_KEY: 'manifest-secret-value',
});

assert.deepEqual(config.provider_secret_env_mapping, {
  connectors_ai_openai_api_key: 'PROVIDER_SECRET_1',
});
assert.deepEqual(config.secret_env, [
  'GITHUB_TOKEN',
  'HOMEBOY_GITHUB_APP_TOKEN',
  'PROVIDER_SECRET_1',
]);
assert.equal('PROVIDER_SECRET_1' in config.bench_env, false);
assert.equal('OPENAI_API_KEY' in config.bench_env, false);
assert.equal('GITHUB_TOKEN' in config.bench_env, false);
assert.equal('HOMEBOY_GITHUB_APP_TOKEN' in config.bench_env, false);
assert.equal(JSON.stringify(config).includes('secret-value'), false);
assert.equal(JSON.stringify(config).includes('manifest-secret-value'), false);

const neutralConfig = buildConfig({
  GITHUB_WORKSPACE: workspace,
  RUNNER_TEMP: runnerTemp,
  WORKLOAD_ID: 'provider-neutral-empty',
  TARGET_REPO: 'Extra-Chill/example',
  RUNTIME: 'local-shell',
  PATH: `${binDir}${path.delimiter}${process.env.PATH || ''}`,
  PROFILE: 'local-shell-profile',
  RUNTIME_PROFILES: JSON.stringify({
    'local-shell-profile': {
      id: 'local-shell-profile',
    },
  }),
  PROVIDER: 'openai',
  PROVIDER_PLUGIN: '{}',
});

assert.deepEqual(neutralConfig.provider_secret_env_mapping, {});
assert.equal('OPENAI_API_KEY' in neutralConfig.bench_env, false);
assert.deepEqual(neutralConfig.secret_env, ['GITHUB_TOKEN', 'HOMEBOY_GITHUB_APP_TOKEN']);

console.log('runtime agent full-run provider-neutral smoke passed');
