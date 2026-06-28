'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const wpCodeboxRuntimeExecutor = path.join(
  __dirname,
  '..',
  '..',
  'agent-runtimes',
  'wp-codebox',
  'scripts',
  'agent',
  'homeboy-codebox-agent-task-executor.cjs'
);
const wpCodeboxTaskRunner = path.join(
  __dirname,
  '..',
  '..',
  'agent-runtimes',
  'wp-codebox',
  'scripts',
  'agent',
  'homeboy-wp-codebox-task-runner.cjs'
);
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(
  __dirname,
  '..',
  '..',
  'tests',
  'fixtures',
  'wp-codebox-core-runtime-contract.cjs'
);

const codexSecretEnv = [
  'AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN',
  'AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT',
  'AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID',
  'AI_PROVIDER_OPENAI_CODEX_FEDRAMP',
];

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-codebox-codex-auth-preflight-'));

try {
  const providerPluginPath = path.join(root, 'ai-provider-for-openai');
  fs.mkdirSync(providerPluginPath, { recursive: true });
  fs.writeFileSync(path.join(providerPluginPath, 'ai-provider-for-openai.php'), '<?php\n/* Plugin Name: AI Provider for OpenAI Codex */\n');

  const result = spawnSync(process.execPath, [wpCodeboxRuntimeExecutor], {
    encoding: 'utf8',
    input: JSON.stringify({
      schema: 'homeboy/agent-task-request/v1',
      task_id: 'codex-stale-refresh-token-preflight',
      executor: {
        backend: 'wp-codebox',
        model: 'gpt-5.5',
        secret_env: codexSecretEnv,
        config: {
          provider: 'codex',
          provider_plugin_paths: [providerPluginPath],
          wp_codebox_bin: '/bin/should-not-launch-wp-codebox',
        },
      },
      instructions: 'Verify Codex auth before launching WP Codebox.',
      workspace: { mode: 'ephemeral' },
    }),
    env: {
      ...process.env,
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'stale-access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'stale-refresh-token-value',
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '1',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'stale-account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
    },
  });

  assert.equal(result.status, 1, result.stderr || result.stdout);
  const outcome = JSON.parse(result.stdout);
  assert.equal(outcome.status, 'failed');
  assert.notEqual(outcome.failure_classification, 'provider');
  assert(!JSON.stringify(outcome).includes('stale-access-token-value'));
  assert(!JSON.stringify(outcome).includes('stale-refresh-token-value'));
  assert(!JSON.stringify(outcome).includes('credential refresh primitive'));

  const validAccessResult = spawnSync(process.execPath, [wpCodeboxTaskRunner], {
    encoding: 'utf8',
    input: JSON.stringify({
      schema: 'wp-codebox/task-input/v1',
      version: 1,
      goal: 'Use fresh Codex access token without refreshing first.',
      target: {},
      allowed_tools: [],
      expected_artifacts: [],
      structured_artifacts: [],
      agent_bundles: [],
      sandbox_tool_policy: {},
      policy: {},
      context: {},
      provider: 'codex',
      model: 'gpt-5.5',
      secret_env: codexSecretEnv,
      wp_codebox_bin: '/bin/false',
    }),
    env: {
      ...process.env,
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'valid-access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'rejecting-refresh-token-value',
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '4102444800',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
    },
  });
  assert.notEqual(validAccessResult.status, 0, 'fake WP Codebox command should fail after auth preflight');
  assert(!`${validAccessResult.stdout}${validAccessResult.stderr}`.includes('credential refresh primitive'));
  assert(!`${validAccessResult.stdout}${validAccessResult.stderr}`.includes('rejecting-refresh-token-value'));

  const authPath = path.join(root, 'codex-auth.json');
  fs.writeFileSync(authPath, JSON.stringify({
    tokens: {
      access_token: 'stale-access-token-value',
      refresh_token: 'stale-refresh-token-value',
      expires_at: '4102444800',
      account_id: 'account-id-value',
      fedramp: '0',
    },
    preserved: true,
  }, null, 2));

  const refreshResult = spawnSync(process.execPath, [wpCodeboxTaskRunner], {
    encoding: 'utf8',
    input: JSON.stringify({
      schema: 'wp-codebox/task-input/v1',
      version: 1,
      goal: 'Refresh Codex auth before launching WP Codebox.',
      target: {},
      allowed_tools: [],
      expected_artifacts: [],
      structured_artifacts: [],
      agent_bundles: [],
      sandbox_tool_policy: {},
      policy: {},
      context: {},
      provider: 'codex',
      model: 'gpt-5.5',
      secret_env: codexSecretEnv,
      wp_codebox_bin: '/bin/false',
    }),
    env: {
      ...process.env,
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'stale-access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'stale-refresh-token-value',
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '1',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
      HOMEBOY_WP_CODEBOX_CODEX_AUTH_PATH: authPath,
    },
  });
  assert.notEqual(refreshResult.status, 0, 'fake WP Codebox command should fail after auth preflight');
  assert(!`${refreshResult.stdout}${refreshResult.stderr}`.includes('credential refresh primitive'));
  const persisted = JSON.parse(fs.readFileSync(authPath, 'utf8'));
  assert.equal(persisted.preserved, true);
  assert.equal(persisted.tokens.access_token, 'stale-access-token-value');
  assert.equal(persisted.tokens.refresh_token, 'stale-refresh-token-value');
  assert.equal(persisted.tokens.account_id, 'account-id-value');
  assert.equal(persisted.tokens.expires_at, '4102444800');
  assert(!`${refreshResult.stdout}${refreshResult.stderr}`.includes('stale-refresh-token-value'));
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}

console.log('Codebox Codex auth preflight smoke passed');
