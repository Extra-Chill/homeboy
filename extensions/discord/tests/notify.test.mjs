import assert from 'node:assert/strict';
import http from 'node:http';
import { once } from 'node:events';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const extensionPath = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const helper = path.join(extensionPath, 'scripts/notify.mjs');
const secretToken = 'bot-secret-123';
const secretWebhook = '/webhooks/webhook-id/webhook-secret-456';

await testBotThreadDelivery();
await testWebhookDelivery();
await testRateLimitRetry();
await testValidationBeforeNetwork();
await testAuthFailureClassification();
await testTruncation();
await testRedactionAndDryRun();
console.log('discord notification tests passed');

async function testBotThreadDelivery() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify({ DISCORD_BOT_TOKEN: secretToken, DISCORD_THREAD_ID: 'thread-1', DISCORD_API_BASE_URL: `${baseUrl}/api/v10` });
    assert.equal(result.status, 'delivered');
    assert.equal(result.delivery.mode, 'bot');
    assert.equal(result.delivery.destination, 'thread');
    assert.equal(requests.length, 1);
    assert.equal(requests[0].url, '/api/v10/channels/thread-1/messages');
    assert.equal(requests[0].headers.authorization, `Bot ${secretToken}`);
  });
}

async function testWebhookDelivery() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify({ DISCORD_WEBHOOK_URL: `${baseUrl}${secretWebhook}`, DISCORD_THREAD_ID: 'thread-2' });
    assert.equal(result.status, 'delivered');
    assert.equal(result.delivery.mode, 'webhook');
    assert.equal(result.delivery.destination, 'thread');
    assert.equal(requests[0].url, `${secretWebhook}?wait=true&thread_id=thread-2`);
    assert.equal(requests[0].headers.authorization, undefined);
  });
}

async function testRateLimitRetry() {
  let calls = 0;
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify({ DISCORD_BOT_TOKEN: secretToken, DISCORD_CHANNEL_ID: 'channel-1', DISCORD_API_BASE_URL: `${baseUrl}/api/v10` });
    assert.equal(result.status, 'delivered');
    assert.equal(result.attempts, 2);
    assert.equal(requests.length, 2);
  }, (_request, response) => {
    calls += 1;
    if (calls === 1) return response.writeHead(429, { 'content-type': 'application/json' }).end('{"retry_after":0}');
    response.writeHead(200).end('{}');
  });
}

async function testValidationBeforeNetwork() {
  const run = await notifyRaw({ DISCORD_BOT_TOKEN: secretToken, DISCORD_WEBHOOK_URL: 'https://discord.invalid/webhook', DISCORD_CHANNEL_ID: 'channel-1' });
  assert.equal(run.code, 1);
  const result = JSON.parse(run.stdout);
  assert.equal(result.status, 'failed');
  assert.equal(result.error.kind, 'input_error');
}

async function testAuthFailureClassification() {
  await withServer(async ({ baseUrl }) => {
    const run = await notifyRaw({ DISCORD_BOT_TOKEN: secretToken, DISCORD_CHANNEL_ID: 'channel-1', DISCORD_API_BASE_URL: `${baseUrl}/api/v10` });
    assert.equal(run.code, 1);
    assert.equal(JSON.parse(run.stdout).error.kind, 'auth_error');
  }, (_request, response) => response.writeHead(401).end('{}'));
}

async function testTruncation() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify({ DISCORD_BOT_TOKEN: secretToken, DISCORD_CHANNEL_ID: 'channel-1', DISCORD_API_BASE_URL: `${baseUrl}/api/v10` }, { body: 'x'.repeat(3000) });
    assert.equal(result.delivery.content_length, 2000);
    assert.equal(result.delivery.truncated, true);
    assert.equal(requests[0].body.content.length, 2000);
    assert.equal(requests[0].body.content.endsWith('...'), true);
  });
}

async function testRedactionAndDryRun() {
  const run = await notifyRaw({ DISCORD_WEBHOOK_URL: `https://example.invalid${secretWebhook}` }, { dryRun: true });
  assert.equal(run.code, 0);
  assert.doesNotMatch(run.stdout, /webhook-secret-456|bot-secret-123|example\.invalid/);
  const result = JSON.parse(run.stdout);
  assert.equal(result.status, 'dry_run');
  assert.equal(result.attempts, 0);
}

async function withServer(test, responder = (_request, response) => response.writeHead(200).end('{}')) {
  const requests = [];
  const server = http.createServer(async (request, response) => {
    let raw = '';
    for await (const chunk of request) raw += chunk;
    requests.push({ url: request.url, headers: request.headers, body: JSON.parse(raw) });
    responder(request, response);
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  try {
    await test({ baseUrl: `http://127.0.0.1:${server.address().port}`, requests });
  } finally {
    server.close();
    await once(server, 'close');
  }
}

function notify(env, overrides = {}) {
  return notifyRaw(env, overrides).then((run) => {
    assert.equal(run.code, 0, run.stderr);
    return JSON.parse(run.stdout);
  });
}

function notifyRaw(env, overrides = {}) {
  const args = ['--run-id', 'run-123', '--status', 'pass', '--title', 'homeboy run pass', '--body', overrides.body || 'Run completed'];
  if (overrides.dryRun) args.push('--dry-run');
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [helper, ...args], { env: { ...process.env, ...env } });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.once('error', reject);
    child.once('close', (code) => resolve({ code, stdout, stderr }));
  });
}
