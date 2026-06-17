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
      task_id: 'codex-expired-auth-preflight',
      executor: {
        backend: 'codebox',
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
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'expired-access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'expired-refresh-token-value',
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '1',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'expired-account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
    },
  });

  assert.equal(result.status, 1, result.stderr || result.stdout);
  const outcome = JSON.parse(result.stdout);
  assert.equal(outcome.status, 'failed');
  assert.equal(outcome.failure_classification, 'provider');
  assert.equal(outcome.diagnostics[0].class, 'codebox.preflight.codex_auth');
  assert.match(outcome.diagnostics[0].data.stderr, /AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT/);
  assert.match(outcome.diagnostics[0].data.stderr, /Refresh Codex OAuth credentials/);
  assert(!JSON.stringify(outcome).includes('expired-access-token-value'));
  assert(!JSON.stringify(outcome).includes('expired-refresh-token-value'));
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}

console.log('Codebox Codex auth preflight smoke passed');
