'use strict';

const assert = require('node:assert/strict');
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
  assert.equal(config.secret_env_plan.schema, SECRET_ENV_PLAN_SCHEMA);
  assert.deepEqual(config.secret_env_plan.inheritance, {
    require_declaration: true,
    allowed_env_names: ['HOMEBOY_AGENT_RUNTIME_SECRET_ENV'],
  });
  assert.deepEqual(config.secret_env_plan.secret_env_names, config.secret_env);
} finally {
  fs.rmSync(tmpRoot, { recursive: true, force: true });
}

assert.deepEqual(
  buildSecretEnvPlan({
    secretEnv: ['PRIVATE_TOKEN'],
    runtimeEnv: { PUBLIC_MODE: 'test', PRIVATE_MODE: false },
    providerSecretEnvMapping: { token: 'PROVIDER_SECRET_1' },
    secretEnvFallbacks: { PRIVATE_TOKEN: ['PROVIDER_SECRET_1'] },
  }),
  {
    schema: SECRET_ENV_PLAN_SCHEMA,
    public_env: { PUBLIC_MODE: 'test' },
    secret_env_names: ['PRIVATE_TOKEN'],
    requirements: [{ name: 'PRIVATE_TOKEN', required: true }],
    env_name_mapping: {
      provider_secret_env: ['PROVIDER_SECRET_1'],
      secret_env_fallbacks: ['PROVIDER_SECRET_1'],
    },
    inheritance: {
      require_declaration: true,
      allowed_env_names: ['HOMEBOY_AGENT_RUNTIME_SECRET_ENV'],
    },
  }
);

assert.deepEqual(
  normalizeProviderPlugin('{"providerSecretEnv":{"token":"PROVIDER_TOKEN"}}', 'fixture', true).provider_secret_env,
  { token: 'PROVIDER_TOKEN' }
);

process.stdout.write('Runtime agent full-run config loop policy checks passed\n');
