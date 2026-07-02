'use strict';

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  buildConfig,
  buildSecretEnvPlan,
  loopPolicyFromEnv,
  SECRET_ENV_PLAN_SCHEMA,
} = require('..');
const { normalizeProviderPlugin } = require('../lib/full-run-inputs.cjs');

assert.equal(typeof buildConfig, 'function');

function validateSecretEnvPlan(plan) {
  const fixturePath = path.join(fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-secret-env-plan-')), 'plan.json');
  fs.writeFileSync(fixturePath, `${JSON.stringify(plan, null, 2)}\n`);
  const result = spawnSync(process.env.HOMEBOY_COMMAND || 'homeboy', [
    'contract',
    'validate',
    SECRET_ENV_PLAN_SCHEMA,
    '--file',
    fixturePath,
  ], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const output = JSON.parse(result.stdout);
  assert.equal(output.success, true);
  assert.equal(output.data.valid, true);
  return output;
}

assert.deepEqual(loopPolicyFromEnv({}), {});
assert.deepEqual(loopPolicyFromEnv({
  LOOP_POLICY: '{"mode":"duration"}',
  MAX_REVOLUTIONS: '4',
  DURATION_MS: '5000',
  DEADLINE_AT: '2030-01-01T00:00:00.000Z',
}), {
  mode: 'duration',
  max_revolutions: 4,
  duration_ms: 5000,
  deadline_at: '2030-01-01T00:00:00.000Z',
});

const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-build-runner-config-'));
try {
  const config = buildConfig({
    ...process.env,
    GITHUB_WORKSPACE: tmpRoot,
    RUNNER_TEMP: tmpRoot,
    WORKLOAD_ID: 'loop-policy-fixture',
    TARGET_REPO: 'Extra-Chill/example',
    PROFILE: 'runtime-agent-ci',
    RUNTIME_PROFILES: '{}',
    RUNTIME: 'local-shell',
    LOOP_POLICY: '{"mode":"duration"}',
    MAX_REVOLUTIONS: '3',
    DURATION_MS: '60000',
    DEADLINE_AT: '2030-01-01T00:00:00.000Z',
  });

  assert.deepEqual(config.loop_policy, {
    mode: 'duration',
    max_revolutions: 3,
    duration_ms: 60000,
    deadline_at: '2030-01-01T00:00:00.000Z',
  });

  assert.equal(config.execution_kind, 'runtime_execution');
  assert.deepEqual(config.secret_env.slice(0, 2), ['GITHUB_TOKEN', 'HOMEBOY_GITHUB_APP_TOKEN']);
  validateSecretEnvPlan(config.secret_env_plan);
  assert.deepEqual(config.secret_env_plan.secret_env_names, config.secret_env);
} finally {
  fs.rmSync(tmpRoot, { recursive: true, force: true });
}

const mappedSecretEnvPlan = buildSecretEnvPlan({
  secretEnv: ['PRIVATE_TOKEN'],
  runtimeEnv: { PUBLIC_MODE: 'test', PRIVATE_MODE: false },
  providerSecretEnvMapping: { token: 'PROVIDER_SECRET_1' },
  secretEnvFallbacks: { PRIVATE_TOKEN: ['PROVIDER_SECRET_1'] },
});
validateSecretEnvPlan(mappedSecretEnvPlan);
assert.deepEqual(mappedSecretEnvPlan.public_env, { PUBLIC_MODE: 'test' });
assert.deepEqual(mappedSecretEnvPlan.secret_env_names, ['PRIVATE_TOKEN']);
assert.deepEqual(mappedSecretEnvPlan.requirements, [{ name: 'PRIVATE_TOKEN', required: true }]);
assert.deepEqual(mappedSecretEnvPlan.env_name_mapping, {
  provider_secret_env: ['PROVIDER_SECRET_1'],
  secret_env_fallbacks: ['PROVIDER_SECRET_1'],
});

const plannedSecretEnv = buildSecretEnvPlan({
  secretEnv: ['OPENAI_API_KEY'],
  basePlan: {
    schema: SECRET_ENV_PLAN_SCHEMA,
    public_env: { EXISTING_PUBLIC_MODE: 'on' },
    secret_env_names: ['ANTHROPIC_API_KEY'],
    requirements: [{ name: 'ANTHROPIC_API_KEY', required: false, source: 'runner' }],
  },
});
validateSecretEnvPlan(plannedSecretEnv);
assert.deepEqual(plannedSecretEnv.public_env, { EXISTING_PUBLIC_MODE: 'on' });
assert.deepEqual(plannedSecretEnv.secret_env_names, ['ANTHROPIC_API_KEY', 'OPENAI_API_KEY']);
assert.deepEqual(plannedSecretEnv.requirements, [
  { name: 'ANTHROPIC_API_KEY', required: false, source: 'runner' },
  { name: 'OPENAI_API_KEY', required: true },
]);

const canonicalSecretTmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-build-runner-config-secret-env-'));
try {
  const canonicalSecretConfig = buildConfig({
    ...process.env,
    GITHUB_WORKSPACE: canonicalSecretTmpRoot,
    RUNNER_TEMP: canonicalSecretTmpRoot,
    WORKLOAD_ID: 'canonical-secret-fixture',
    TARGET_REPO: 'Extra-Chill/example',
    PROFILE: 'runtime-agent-ci',
    RUNTIME_PROFILES: '{}',
    RUNTIME: 'local-shell',
    SECRET_ENV: 'OPENAI_API_KEY',
    SECRET_ENV_MAP: '{"OPENAI_API_KEY":"PROVIDER_SECRET_1"}',
    SECRET_ENV_PLAN: JSON.stringify({
      schema: SECRET_ENV_PLAN_SCHEMA,
      secret_env_names: ['ANTHROPIC_API_KEY'],
      requirements: [{ name: 'ANTHROPIC_API_KEY', required: false, source: 'runner' }],
    }),
  });
  validateSecretEnvPlan(canonicalSecretConfig.secret_env_plan);
  assert.deepEqual(canonicalSecretConfig.secret_env, [
    'ANTHROPIC_API_KEY',
    'GITHUB_TOKEN',
    'HOMEBOY_GITHUB_APP_TOKEN',
    'OPENAI_API_KEY',
  ]);
  assert.deepEqual(canonicalSecretConfig.secret_env_fallbacks.OPENAI_API_KEY, ['PROVIDER_SECRET_1']);
  assert.deepEqual(canonicalSecretConfig.secret_env_map, { OPENAI_API_KEY: ['PROVIDER_SECRET_1'] });
  assert.deepEqual(canonicalSecretConfig.secret_env_plan.requirements, [
    { name: 'ANTHROPIC_API_KEY', required: false, source: 'runner' },
    { name: 'GITHUB_TOKEN', required: true },
    { name: 'HOMEBOY_GITHUB_APP_TOKEN', required: true },
    { name: 'OPENAI_API_KEY', required: true },
  ]);
} finally {
  fs.rmSync(canonicalSecretTmpRoot, { recursive: true, force: true });
}

assert.deepEqual(
  normalizeProviderPlugin('{"providerSecretEnv":{"token":"PROVIDER_TOKEN"}}', 'fixture', true).provider_secret_env,
  { token: 'PROVIDER_TOKEN' }
);

process.stdout.write('Runtime agent full-run config loop policy checks passed\n');
