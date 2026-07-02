#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
  buildSecretEnvFallbacks,
  buildSecretEnvPlan,
  normalizeSecretEnvInput,
  normalizeSecretEnvMap,
  secretEnvMapSourceNames,
} = require(path.join(repoRoot, 'runtime-agent-ci/lib/secret-env-plan.cjs'));

assert.deepEqual(normalizeSecretEnvInput('OPENAI_API_KEY, PROVIDER_SECRET_1'), ['OPENAI_API_KEY', 'PROVIDER_SECRET_1']);
assert.deepEqual(normalizeSecretEnvMap({ OPENAI_API_KEY: ['PROVIDER_SECRET_1', 'PROVIDER_SECRET_2'] }), {
  OPENAI_API_KEY: ['PROVIDER_SECRET_1', 'PROVIDER_SECRET_2'],
});
assert.deepEqual(secretEnvMapSourceNames({ OPENAI_API_KEY: ['PROVIDER_SECRET_2', 'PROVIDER_SECRET_1'] }), ['PROVIDER_SECRET_1', 'PROVIDER_SECRET_2']);
assert.throws(() => normalizeSecretEnvMap({ OPENAI_API_KEY: ['not-valid'] }), /valid environment variable names/);

const secretEnvFallbacks = buildSecretEnvFallbacks({
  githubTokenEnv: 'HOMEBOY_GITHUB_APP_TOKEN',
  githubRepositoryTokenEnv: 'GITHUB_TOKEN',
  providerCanonicalSecretEnvNames: ['OPENAI_API_KEY'],
  providerCredentialSourceEnvNames: ['PROVIDER_SECRET_1'],
  secretEnvMap: { ANTHROPIC_API_KEY: ['PROVIDER_SECRET_2'] },
});
assert.deepEqual(secretEnvFallbacks, {
  ANTHROPIC_API_KEY: ['PROVIDER_SECRET_2'],
  HOMEBOY_GITHUB_APP_TOKEN: ['GITHUB_TOKEN'],
  OPENAI_API_KEY: ['PROVIDER_SECRET_1'],
});

const secretValue = 'sk-test-value-must-not-leak';
const plan = buildSecretEnvPlan({
  secretEnv: ['OPENAI_API_KEY', 'PROVIDER_SECRET_1'],
  runtimeEnv: {
    PUBLIC_FLAG: 'enabled',
    NON_STRING: 1,
  },
  providerSecretEnvMapping: { connectors_ai_openai_api_key: 'PROVIDER_SECRET_1' },
  secretEnvFallbacks,
});

assert.equal(plan.schema, 'homeboy/secret-env-plan/v1');
assert.deepEqual(plan.public_env, {
  PUBLIC_FLAG: 'enabled',
});
assert.deepEqual(plan.secret_env_names, ['OPENAI_API_KEY', 'PROVIDER_SECRET_1']);
assert.deepEqual(plan.env_name_mapping.provider_secret_env, ['PROVIDER_SECRET_1']);
assert.equal(JSON.stringify(plan).includes(secretValue), false);

console.log('secret env plan smoke passed');
