'use strict';

require('./helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  buildConfig,
  buildSecretEnvPlan,
  loopPolicyFromEnv,
  normalizePathMaterializationPlan,
  parseRuntimeWorkloadProfile,
  PATH_MATERIALIZATION_PLAN_SCHEMA,
  SECRET_ENV_PLAN_SCHEMA,
} = require('../provider-adapters');
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

assert.deepEqual(parseRuntimeWorkloadProfile(JSON.stringify({
  runtime: 'local-shell',
  profile: 'profile-default',
  workload: { id: 'profile-workload', label: 'Profile workload' },
  tool_profile: { workspace_tools: { inspect: { command: 'git status --short' } } },
  loop_policy: { mode: 'duration', max_revolutions: 2 },
})), {
  runtime: 'local-shell',
  profile: 'profile-default',
  workload_id: '',
  workload: { id: 'profile-workload', label: 'Profile workload' },
  tool_profile: { workspace_tools: { inspect: { command: 'git status --short' } } },
  loop_policy: { mode: 'duration', max_revolutions: 2 },
});

assert.throws(
  () => parseRuntimeWorkloadProfile('{bad-json'),
  /Invalid runtime_workload_profile_json:/
);

const pathPlanWorkspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-path-plan-'));
try {
  const pathPlan = normalizePathMaterializationPlan({
    schema: PATH_MATERIALIZATION_PLAN_SCHEMA,
    paths: {
      runner_workspace_guest_checkout: '/runtime/workspace/example',
      transcript_host_dir: 'artifacts/transcript',
      transcript_dir: '/runtime/workspace/example/artifacts/transcript',
    },
    runtime_mounts: [{ source: '.', target: '/runtime/workspace/example', metadata: { kind: 'runner-workspace' } }],
  }, { workspace: pathPlanWorkspace });
  assert.equal(pathPlan.runner_workspace_guest_checkout, '/runtime/workspace/example');
  assert.equal(pathPlan.transcript_host_dir, path.join(pathPlanWorkspace, 'artifacts/transcript'));
  assert.equal(pathPlan.transcript_dir, '/runtime/workspace/example/artifacts/transcript');
  assert.deepEqual(pathPlan.runtime_mounts, [{
    type: 'directory',
    source: pathPlanWorkspace,
    target: '/runtime/workspace/example',
    mode: 'readwrite',
    metadata: { kind: 'runner-workspace' },
  }]);

  assert.throws(
    () => normalizePathMaterializationPlan({
      schema: PATH_MATERIALIZATION_PLAN_SCHEMA,
      paths: { transcript_host_dir: '../outside' },
    }, { workspace: pathPlanWorkspace }),
    /transcript_host_dir.*parent-directory|transcript_host_dir.*under GITHUB_WORKSPACE/
  );
  assert.throws(
    () => normalizePathMaterializationPlan({
      schema: PATH_MATERIALIZATION_PLAN_SCHEMA,
      paths: { transcript_dir: 'relative/transcript' },
    }, { workspace: pathPlanWorkspace }),
    /transcript_dir must be an absolute POSIX path/
  );
  assert.throws(
    () => normalizePathMaterializationPlan({
      schema: PATH_MATERIALIZATION_PLAN_SCHEMA,
      runtime_mounts: [{ source: '.', target: '/runtime/../escape' }],
    }, { workspace: pathPlanWorkspace }),
    /runtime_mounts\[0\]\.target.*normalized absolute POSIX path/
  );
} finally {
  fs.rmSync(pathPlanWorkspace, { recursive: true, force: true });
}

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

const profileTmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-build-runner-profile-'));
try {
  const config = buildConfig({
    ...process.env,
    GITHUB_WORKSPACE: profileTmpRoot,
    RUNNER_TEMP: profileTmpRoot,
    WORKLOAD_ID: '',
    TARGET_REPO: 'Extra-Chill/example',
    PROFILE: '',
    RUNTIME_PROFILES: '{}',
    RUNTIME: '',
    LOOP_POLICY: '{}',
    TOOL_PROFILE: '{}',
    WORKLOAD: '{}',
    RUNTIME_WORKLOAD_PROFILE_JSON: JSON.stringify({
      runtime: 'local-shell',
      profile: 'profile-default',
      workload_id: 'profile-workload',
      workload: { label: 'Profile workload' },
      tool_profile: { workspace_tools: { inspect: { command: 'git status --short' } } },
      loop_policy: { mode: 'duration', max_revolutions: 2 },
    }),
  });

  assert.equal(config.runtime_id, 'local-shell');
  assert.equal(config.runtime_profile, 'profile-default');
  assert.equal(config.workload_id, 'profile-workload');
  assert.equal(config.workload_label, 'Profile workload');
  assert.deepEqual(config.loop_policy, { mode: 'duration', max_revolutions: 2 });
} finally {
  fs.rmSync(profileTmpRoot, { recursive: true, force: true });
}

const explicitProfileTmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-build-runner-profile-explicit-'));
try {
  const config = buildConfig({
    ...process.env,
    GITHUB_WORKSPACE: explicitProfileTmpRoot,
    RUNNER_TEMP: explicitProfileTmpRoot,
    WORKLOAD_ID: 'explicit-workload',
    TARGET_REPO: 'Extra-Chill/example',
    PROFILE: 'explicit-profile',
    RUNTIME_PROFILES: '{}',
    RUNTIME: 'local-shell',
    WORKLOAD: '{"label":"Explicit workload"}',
    LOOP_POLICY: '{"mode":"revolution"}',
    MAX_REVOLUTIONS: '5',
    RUNTIME_WORKLOAD_PROFILE_JSON: JSON.stringify({
      runtime: 'other-runtime',
      profile: 'profile-default',
      workload_id: 'profile-workload',
      workload: { label: 'Profile workload' },
      loop_policy: { mode: 'duration', max_revolutions: 2 },
    }),
  });

  assert.equal(config.runtime_id, 'local-shell');
  assert.equal(config.runtime_profile, 'explicit-profile');
  assert.equal(config.workload_id, 'explicit-workload');
  assert.equal(config.workload_label, 'Explicit workload');
  assert.deepEqual(config.loop_policy, { mode: 'revolution', max_revolutions: 5 });
} finally {
  fs.rmSync(explicitProfileTmpRoot, { recursive: true, force: true });
}

const materializedTmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-build-runner-config-path-plan-'));
try {
  const config = buildConfig({
    ...process.env,
    GITHUB_WORKSPACE: materializedTmpRoot,
    RUNNER_TEMP: materializedTmpRoot,
    WORKLOAD_ID: 'path-plan-fixture',
    TARGET_REPO: 'Extra-Chill/example',
    PROFILE: 'runtime-agent-ci',
    RUNTIME_PROFILES: '{}',
    RUNTIME: 'local-shell',
    RUNNER_WORKSPACE_CONFIG: '{"enabled":true}',
    PATH_MATERIALIZATION_PLAN: JSON.stringify({
      schema: PATH_MATERIALIZATION_PLAN_SCHEMA,
      paths: {
        runner_workspace_guest_checkout: '/runtime/workspace/example',
        transcript_host_dir: 'runtime-artifacts/path-plan-fixture',
        transcript_dir: '/runtime/workspace/example/runtime-artifacts/path-plan-fixture',
      },
      runtime_mounts: [{ source: '.', target: '/runtime/workspace/example', mode: 'readwrite' }],
    }),
  });

  assert.equal(config.runner_workspace.checkout_path, '/runtime/workspace/example');
  assert.equal(config.transcript_host_dir, path.join(materializedTmpRoot, 'runtime-artifacts/path-plan-fixture'));
  assert.equal(config.transcript_dir, '/runtime/workspace/example/runtime-artifacts/path-plan-fixture');
  assert.deepEqual(config.runtime_mounts, [{
    type: 'directory',
    source: materializedTmpRoot,
    target: '/runtime/workspace/example',
    mode: 'readwrite',
  }]);
  assert.equal(config.path_materialization_plan.schema, PATH_MATERIALIZATION_PLAN_SCHEMA);
} finally {
  fs.rmSync(materializedTmpRoot, { recursive: true, force: true });
}

const mappedSecretEnvPlan = buildSecretEnvPlan({
  secretEnv: ['PRIVATE_TOKEN'],
  runtimeEnv: { PUBLIC_MODE: 'test', PRIVATE_MODE: false },
  providerSecretEnvMapping: { token: 'UPSTREAM_PROVIDER_TOKEN' },
  secretEnvFallbacks: { PRIVATE_TOKEN: ['UPSTREAM_PROVIDER_TOKEN'] },
});
validateSecretEnvPlan(mappedSecretEnvPlan);
assert.deepEqual(mappedSecretEnvPlan.public_env, { PUBLIC_MODE: 'test' });
assert.deepEqual(mappedSecretEnvPlan.secret_env_names, ['PRIVATE_TOKEN']);
assert.deepEqual(mappedSecretEnvPlan.requirements, [{ name: 'PRIVATE_TOKEN', required: true }]);
assert.deepEqual(mappedSecretEnvPlan.env_name_mapping, {
  provider_secret_env: ['UPSTREAM_PROVIDER_TOKEN'],
  secret_env_fallbacks: ['UPSTREAM_PROVIDER_TOKEN'],
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
    SECRET_ENV_MAP: '{"OPENAI_API_KEY":"UPSTREAM_OPENAI_API_KEY"}',
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
  assert.deepEqual(canonicalSecretConfig.secret_env_fallbacks.OPENAI_API_KEY, ['UPSTREAM_OPENAI_API_KEY']);
  assert.deepEqual(canonicalSecretConfig.secret_env_map, { OPENAI_API_KEY: ['UPSTREAM_OPENAI_API_KEY'] });
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
  normalizeProviderPlugin('{"provider_secret_env":{"token":"PROVIDER_TOKEN"}}', 'fixture', true).provider_secret_env,
  { token: 'PROVIDER_TOKEN' }
);

process.stdout.write('Runtime agent full-run config loop policy checks passed\n');
