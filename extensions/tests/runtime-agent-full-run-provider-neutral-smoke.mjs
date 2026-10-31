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
const { buildConfig, buildSecretEnvFallbacks, providerBenchEnvFromManifest } = require(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/build-runner-config.cjs'));
const { normalizeProviderPlugin } = require(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/lib/common.cjs'));
const { resolveRuntimeProvider } = require(path.join(repoRoot, 'runtime-agent-ci/lib/runtime-provider-resolver.cjs'));

const baseEnv = {
  RUNTIME: 'local-shell',
  RUNTIME_DEPENDENCIES: '',
  VALIDATION_DEPENDENCIES: '',
  PROVIDER_PLUGIN: '{}',
  PROVIDER: 'openai',
};
const fixtureProviderId = 'openai';
const fixtureProviderPluginRepo = 'WordPress/ai-provider-for-openai';
const fixtureProviderPluginTarget = '.ci/ai-provider-for-openai';
const fixtureProviderCanonicalSecretEnv = 'OPENAI_API_KEY';
const fixtureProviderSecretKey = 'connectors_ai_openai_api_key';
const fixtureProviderSecretEnv = 'PROVIDER_SECRET_1';

assert.deepEqual(dependencyEntries(baseEnv), []);
assert.deepEqual(resolvePlan(dependencyEntries(baseEnv), true), []);
assert.deepEqual(normalizeProviderPlugin('{}', fixtureProviderId, true).provider_secret_env, {});
const explicitProviderPlugin = {
  repo: fixtureProviderPluginRepo,
  ref: 'trunk',
  path: '.',
  provider_secret_env: {
    [fixtureProviderSecretKey]: fixtureProviderSecretEnv,
  },
};
const providerPluginPlan = resolvePlan(dependencyEntries({
  ...baseEnv,
  PROVIDER_PLUGIN: JSON.stringify(explicitProviderPlugin),
}), true, { workspace: repoRoot });
assert.deepEqual(providerPluginPlan, [{
  repo: fixtureProviderPluginRepo,
  ref: 'trunk',
  target: fixtureProviderPluginTarget,
  targetPath: path.join(repoRoot, fixtureProviderPluginTarget),
}]);
assert.deepEqual(normalizeProviderPlugin(JSON.stringify(explicitProviderPlugin), fixtureProviderId, true).provider_secret_env, {
  [fixtureProviderSecretKey]: fixtureProviderSecretEnv,
});

for (const unsafeTarget of ['.', '/tmp/foo', '../repo']) {
  assert.throws(
    () => resolvePlan([{ repo: 'Extra-Chill/example', ref: 'main', target: unsafeTarget }], true, { workspace: repoRoot }),
    /Dependency target must/,
    `rejects unsafe dependency target ${unsafeTarget}`,
  );
}
assert.deepEqual(resolvePlan([
  { repo: 'Extra-Chill/example-dependency', ref: 'main', target: '.ci/dependencies/example-dependency' },
], true, { workspace: repoRoot }), [{
  repo: 'Extra-Chill/example-dependency',
  ref: 'main',
  target: '.ci/dependencies/example-dependency',
  targetPath: path.join(repoRoot, '.ci', 'dependencies', 'example-dependency'),
}]);

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-provider-neutral-workspace-'));
const runnerTemp = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-provider-neutral-runner-'));
const binDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-provider-neutral-bin-'));
const runtime = resolveRuntimeProvider('wp-codebox', { workspace });
assert.deepEqual(providerBenchEnvFromManifest(runtime, fixtureProviderId, {
  [fixtureProviderCanonicalSecretEnv]: 'manifest-secret-value',
}), new Set([fixtureProviderCanonicalSecretEnv]));
assert.deepEqual(providerBenchEnvFromManifest(runtime, fixtureProviderId, {}), new Set([fixtureProviderCanonicalSecretEnv]));

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
  PROVIDER: fixtureProviderId,
  PROVIDER_PLUGIN: JSON.stringify(explicitProviderPlugin),
  [fixtureProviderSecretEnv]: 'secret-value',
  [fixtureProviderCanonicalSecretEnv]: 'manifest-secret-value',
});

assert.deepEqual(config.provider_secret_env_mapping, {
  [fixtureProviderSecretKey]: fixtureProviderSecretEnv,
});
assert.deepEqual(config.secret_env, [
  'GITHUB_TOKEN',
  'HOMEBOY_GITHUB_APP_TOKEN',
  fixtureProviderSecretEnv,
]);
assert.equal(fixtureProviderSecretEnv in config.bench_env, false);
assert.equal(fixtureProviderCanonicalSecretEnv in config.bench_env, false);
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
  PROVIDER: fixtureProviderId,
  PROVIDER_PLUGIN: '{}',
});

assert.deepEqual(neutralConfig.provider_secret_env_mapping, {});
assert.equal(fixtureProviderCanonicalSecretEnv in neutralConfig.bench_env, false);
assert.deepEqual(neutralConfig.secret_env, ['GITHUB_TOKEN', 'HOMEBOY_GITHUB_APP_TOKEN']);

// Secret env fallbacks: the provider's canonical key is sourced from the mapped
// generic credential secret, and the Homeboy app token falls back to GITHUB_TOKEN.
assert.deepEqual(
  buildSecretEnvFallbacks({
    githubTokenEnv: 'HOMEBOY_GITHUB_APP_TOKEN',
    githubRepositoryTokenEnv: 'GITHUB_TOKEN',
    providerCanonicalSecretEnvNames: [fixtureProviderCanonicalSecretEnv],
    providerCredentialSourceEnvNames: [fixtureProviderSecretEnv],
  }),
  {
    HOMEBOY_GITHUB_APP_TOKEN: ['GITHUB_TOKEN'],
    [fixtureProviderCanonicalSecretEnv]: [fixtureProviderSecretEnv],
  }
);

// Without a caller credential mapping, only the GitHub token fallback applies.
assert.deepEqual(
  buildSecretEnvFallbacks({
    githubTokenEnv: 'HOMEBOY_GITHUB_APP_TOKEN',
    githubRepositoryTokenEnv: 'GITHUB_TOKEN',
    providerCanonicalSecretEnvNames: [fixtureProviderCanonicalSecretEnv],
    providerCredentialSourceEnvNames: [],
  }),
  { HOMEBOY_GITHUB_APP_TOKEN: ['GITHUB_TOKEN'] }
);

// A canonical name that equals its own mapped source is not aliased to itself.
assert.deepEqual(
  buildSecretEnvFallbacks({
    githubTokenEnv: 'HOMEBOY_GITHUB_APP_TOKEN',
    githubRepositoryTokenEnv: 'GITHUB_TOKEN',
    providerCanonicalSecretEnvNames: [fixtureProviderCanonicalSecretEnv],
    providerCredentialSourceEnvNames: [fixtureProviderCanonicalSecretEnv],
  }),
  { HOMEBOY_GITHUB_APP_TOKEN: ['GITHUB_TOKEN'] }
);

console.log('runtime agent full-run provider-neutral smoke passed');
