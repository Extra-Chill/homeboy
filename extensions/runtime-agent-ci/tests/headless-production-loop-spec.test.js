'use strict';

const assert = require('node:assert/strict');
const { buildGenericAgentLoopRequest } = require('../lib/generic-agent-loop-runner');
const {
  materializeHeadlessProductionLoopSpec,
  providerDefaultSecretEnv,
} = require('../lib/headless-production-loop-spec');

const baseSpec = {
  loop_id: 'generic-production-loop',
  task_id: 'domain-workload',
  workload_id: 'domain-workload',
  target_repo: 'example/domain-workload',
  component_path: '/workspace/domain-workload',
  runtime_profile: 'domain-runtime-profile',
  runtime_profiles: {
    'domain-runtime-profile': {
      id: 'domain-runtime-profile',
      runtime_task_ability: 'runtime/run-task',
    },
  },
  loop_policy: {
    accepted_statuses: ['succeeded'],
  },
};

const codeboxProfile = {
  'codebox-codex': {
    id: 'codebox-codex',
    runtime_task_ability: 'wp-codebox/run-runtime-package',
    runtime_bundle_ability: 'wp-codebox/run-runtime-package',
  },
};

const codexProfile = {
  'standalone-codex': {
    id: 'standalone-codex',
    runtime_task_ability: 'codex/run-task',
  },
};

const codeboxSpec = materializeHeadlessProductionLoopSpec(baseSpec, {
  revolutions: 4,
  runtime_id: 'wp-codebox',
  runtime_profile: 'codebox-codex',
  runtime_profiles: codeboxProfile,
  provider: 'codex',
  model: 'gpt-5.5',
});

assert.equal(codeboxSpec.tasks[0].loop_policy.max_iterations, 4);
assert.equal(codeboxSpec.tasks[0].runtime_id, 'wp-codebox');
assert.equal(codeboxSpec.tasks[0].runtime_profile, 'codebox-codex');

const codeboxRequest = buildGenericAgentLoopRequest({
  plan: codeboxSpec.tasks[0],
  runtime: { id: 'wp-codebox', executor: { backend: 'wp-codebox' } },
});
assert.equal(codeboxRequest.executor.backend, 'wp-codebox');
assert.equal(codeboxRequest.executor.config.provider, 'codex');
assert.equal(codeboxRequest.executor.config.model, 'gpt-5.5');
assert.deepEqual(codeboxRequest.executor.secret_env, []);
assert.deepEqual(providerDefaultSecretEnv('codex', {
  executor: {
    provider_defaults: {
      codex: { secret_env: ['CODEX_TOKEN'] },
    },
  },
}), ['CODEX_TOKEN']);

const explicitSecretEnvSpec = materializeHeadlessProductionLoopSpec(baseSpec, {
  provider: 'codex',
  secret_env: ['CUSTOM_CODEX_SECRET'],
});
assert.deepEqual(explicitSecretEnvSpec.tasks[0].secret_env, ['CUSTOM_CODEX_SECRET']);

const swappedSpec = materializeHeadlessProductionLoopSpec(baseSpec, {
  revolutions: 2,
  runtime_id: 'codex',
  runtime_profile: 'standalone-codex',
  runtime_profiles: codexProfile,
  provider: 'codex',
});
const swappedRequest = buildGenericAgentLoopRequest({
  plan: swappedSpec.tasks[0],
  runtime: { id: 'codex', executor: { backend: 'codex' } },
});
assert.equal(swappedRequest.executor.backend, 'codex');
assert.equal(swappedRequest.executor.config.runtime_profile, 'standalone-codex');
assert.equal(swappedRequest.executor.config.runtime_profiles['standalone-codex'].runtime_task_ability, 'codex/run-task');

process.stdout.write('Headless production loop spec profile swap checks passed\n');
