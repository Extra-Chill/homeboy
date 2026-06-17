'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn, spawnSync } = require('node:child_process');

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
let oauthServer;

function waitForFile(filePath) {
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    if (fs.existsSync(filePath)) {
      return fs.readFileSync(filePath, 'utf8').trim();
    }
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 50);
  }
  throw new Error(`Timed out waiting for ${filePath}`);
}

function createRejectingOAuthServer() {
  const script = path.join(root, 'fixture-rejecting-codex-oauth-server.js');
  const portPath = path.join(root, 'fixture-rejecting-codex-oauth-port');
  fs.writeFileSync(script, `#!/usr/bin/env node
'use strict';
const fs = require('node:fs');
const http = require('node:http');
const portPath = process.argv[2];
const server = http.createServer((request, response) => {
  request.resume();
  response.writeHead(401, { 'Content-Type': 'application/json' });
  response.end(JSON.stringify({ error: 'invalid_grant' }));
});
server.listen(0, '127.0.0.1', () => {
  fs.writeFileSync(portPath, String(server.address().port));
});
`);
  const child = spawn(process.execPath, [script, portPath], { stdio: ['ignore', 'ignore', 'pipe'] });
  const port = waitForFile(portPath);
  return {
    url: `http://127.0.0.1:${port}/oauth/token`,
    stop() {
      child.kill();
    },
  };
}

try {
  oauthServer = createRejectingOAuthServer();
  const providerPluginPath = path.join(root, 'ai-provider-for-openai');
  fs.mkdirSync(providerPluginPath, { recursive: true });
  fs.writeFileSync(path.join(providerPluginPath, 'ai-provider-for-openai.php'), '<?php\n/* Plugin Name: AI Provider for OpenAI Codex */\n');

  const result = spawnSync(process.execPath, [wpCodeboxRuntimeExecutor], {
    encoding: 'utf8',
    input: JSON.stringify({
      schema: 'homeboy/agent-task-request/v1',
      task_id: 'codex-stale-refresh-token-preflight',
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
      AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN: 'stale-access-token-value',
      AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN: 'stale-refresh-token-value',
      AI_PROVIDER_OPENAI_CODEX_EXPIRES_AT: '4102444800',
      AI_PROVIDER_OPENAI_CODEX_ACCOUNT_ID: 'stale-account-id-value',
      AI_PROVIDER_OPENAI_CODEX_FEDRAMP: '0',
      HOMEBOY_WP_CODEBOX_CODEX_TOKEN_URL: oauthServer.url,
    },
  });

  assert.equal(result.status, 1, result.stderr || result.stdout);
  const outcome = JSON.parse(result.stdout);
  assert.equal(outcome.status, 'failed');
  assert.equal(outcome.failure_classification, 'provider');
  assert.equal(outcome.diagnostics[0].class, 'codebox.preflight.codex_auth');
  assert.match(outcome.diagnostics[0].data.stderr, /OAuth refresh returned HTTP 401/);
  assert.match(outcome.diagnostics[0].data.stderr, /Refresh Codex OAuth credentials/);
  assert(!JSON.stringify(outcome).includes('stale-access-token-value'));
  assert(!JSON.stringify(outcome).includes('stale-refresh-token-value'));
} finally {
  if (oauthServer) {
    oauthServer.stop();
  }
  fs.rmSync(root, { recursive: true, force: true });
}

console.log('Codebox Codex auth preflight smoke passed');
