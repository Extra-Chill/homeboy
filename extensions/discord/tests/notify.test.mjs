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
const guildId = '123456789012345678';
const channelId = '223456789012345678';
const threadOneId = '323456789012345678';
const threadTwoId = '423456789012345678';

await testConcurrentThreadRoutesDoNotCrossDeliver();
await testGuildlessThreadRoute();
await testGuildlessChannelRoute();
await testDynamicChannelRoute();
await testOperationsChannelDelivery();
await testRouteLessBotFailsClosed();
await testWebhookDelivery();
await testRateLimitRetry();
await testMalformedRouteBeforeNetwork();
await testCrossModeRouteBeforeNetwork();
await testAuthFailureClassification();
await testTruncation();
await testRedactionAndDryRun();
await testTransportFlagIsAccepted();
await testKimakiBotTokenAlias();
console.log('discord notification tests passed');

// Homeboy appends --transport alongside --route whenever a caller selects a
// transport explicitly, so rejecting the flag broke every explicitly-routed
// notification. Nothing exercised it before, which is why that shipped.
async function testTransportFlagIsAccepted() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      { DISCORD_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` },
      { transport: 'discord.run-completion', route: channelRoute(channelId) },
    );
    assert.equal(result.status, 'delivered');
    assert.equal(result.delivery.route_kind, 'channel');
    assert.equal(requests[0].url, `/api/v10/channels/${channelId}/messages`);
  });

  // An unknown flag must still be refused, so accepting --transport does not
  // turn the parser permissive.
  const run = await notifyRaw(
    { DISCORD_BOT_TOKEN: secretToken },
    { route: channelRoute(channelId) },
    ['--totally-unknown', 'x'],
  );
  assert.equal(run.code, 1);
  assert.match(run.stdout, /input_error/);
}

async function testKimakiBotTokenAlias() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      { KIMAKI_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` },
      { route: threadRoute(threadOneId) },
    );
    assert.equal(result.status, 'delivered');
    assert.equal(result.delivery.mode, 'bot');
    assert.equal(requests[0].url, `/api/v10/channels/${threadOneId}/messages`);
    assert.equal(requests[0].headers.authorization, `Bot ${secretToken}`);
  });
}

async function testConcurrentThreadRoutesDoNotCrossDeliver() {
  await withServer(async ({ baseUrl, requests }) => {
    const env = { DISCORD_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` };
    const [first, second] = await Promise.all([
      notify(env, { route: threadRoute(threadOneId), runId: 'run-one' }),
      notify(env, { route: threadRoute(threadTwoId), runId: 'run-two' }),
    ]);
    assert.equal(first.delivery.route_kind, 'thread');
    assert.equal(second.delivery.route_kind, 'thread');
    assert.equal(first.delivery.destination, 'dynamic_thread');
    assert.equal(second.delivery.destination, 'dynamic_thread');
    assert.deepEqual(
      requests.map((request) => [request.url, request.body.content.includes('run-one') ? 'run-one' : 'run-two']).sort(),
      [
        [`/api/v10/channels/${threadOneId}/messages`, 'run-one'],
        [`/api/v10/channels/${threadTwoId}/messages`, 'run-two'],
      ],
    );
  });
}

async function testGuildlessThreadRoute() {
  // Canonical guild-less form emitted by the kimaki notification bridge
  // (wp-coding-agents #261): discord:v1:thread:<destination-id>.
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      { DISCORD_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` },
      { route: `discord:v1:thread:${threadOneId}` },
    );
    assert.equal(result.delivery.route_kind, 'thread');
    assert.equal(result.delivery.destination, 'dynamic_thread');
    assert.equal(requests[0].url, `/api/v10/channels/${threadOneId}/messages`);
  });
}

async function testGuildlessChannelRoute() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      { DISCORD_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` },
      { route: `discord:v1:channel:${channelId}` },
    );
    assert.equal(result.delivery.route_kind, 'channel');
    assert.equal(result.delivery.destination, 'dynamic_channel');
    assert.equal(requests[0].url, `/api/v10/channels/${channelId}/messages`);
  });
}

async function testDynamicChannelRoute() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify({ DISCORD_BOT_TOKEN: secretToken, DISCORD_OPERATIONS_CHANNEL_ID: threadOneId, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` }, { route: channelRoute(channelId) });
    assert.equal(result.delivery.route_kind, 'channel');
    assert.equal(result.delivery.destination, 'dynamic_channel');
    assert.equal(requests[0].url, `/api/v10/channels/${channelId}/messages`);
  });
}

async function testOperationsChannelDelivery() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify({ DISCORD_BOT_TOKEN: secretToken, DISCORD_OPERATIONS_CHANNEL_ID: channelId, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` }, { route: '' });
    assert.equal(result.delivery.route_kind, 'operations');
    assert.equal(result.delivery.destination, 'operations_channel');
    assert.equal(requests[0].url, `/api/v10/channels/${channelId}/messages`);
  });
}

async function testRouteLessBotFailsClosed() {
  await withServer(async ({ baseUrl, requests }) => {
    const run = await notifyRaw({ DISCORD_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` });
    assert.equal(run.code, 1);
    assert.equal(JSON.parse(run.stdout).error.kind, 'input_error');
    assert.equal(requests.length, 0);
  });
}

async function testWebhookDelivery() {
  await withServer(async ({ baseUrl, requests }) => {
    const [defaultResult, threadResult] = await Promise.all([
      notify({ DISCORD_WEBHOOK_URL: `${baseUrl}${secretWebhook}` }),
      notify({ DISCORD_WEBHOOK_URL: `${baseUrl}${secretWebhook}` }, { route: threadRoute(threadTwoId) }),
    ]);
    assert.equal(defaultResult.delivery.destination, 'webhook_default');
    assert.equal(threadResult.delivery.destination, 'dynamic_thread');
    assert.deepEqual(requests.map((request) => request.url).sort(), [
      `${secretWebhook}?wait=true`,
      `${secretWebhook}?wait=true&thread_id=${threadTwoId}`,
    ]);
    assert.equal(requests[0].headers.authorization, undefined);
  });
}

async function testRateLimitRetry() {
  let calls = 0;
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify({ DISCORD_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` }, { route: channelRoute(channelId) });
    assert.equal(result.status, 'delivered');
    assert.equal(result.attempts, 2);
    assert.equal(requests.length, 2);
  }, (_request, response) => {
    calls += 1;
    if (calls === 1) return response.writeHead(429, { 'content-type': 'application/json' }).end('{"retry_after":0}');
    response.writeHead(200).end('{}');
  });
}

async function testMalformedRouteBeforeNetwork() {
  await withServer(async ({ baseUrl, requests }) => {
    const run = await notifyRaw({ DISCORD_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` }, { route: 'discord:v1:thread:not-a-guild:not-a-thread' });
    assert.equal(run.code, 1);
    assert.equal(JSON.parse(run.stdout).error.kind, 'input_error');
    assert.equal(requests.length, 0);
  });
}

async function testCrossModeRouteBeforeNetwork() {
  await withServer(async ({ baseUrl, requests }) => {
    const run = await notifyRaw({ DISCORD_WEBHOOK_URL: `${baseUrl}${secretWebhook}` }, { route: channelRoute(channelId) });
    assert.equal(run.code, 1);
    assert.equal(JSON.parse(run.stdout).error.kind, 'input_error');
    assert.equal(requests.length, 0);
  });
}

async function testAuthFailureClassification() {
  await withServer(async ({ baseUrl }) => {
    const run = await notifyRaw({ DISCORD_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` }, { route: channelRoute(channelId) });
    assert.equal(run.code, 1);
    assert.equal(JSON.parse(run.stdout).error.kind, 'auth_error');
  }, (_request, response) => response.writeHead(401).end('{}'));
}

async function testTruncation() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify({ DISCORD_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` }, { route: channelRoute(channelId), body: 'x'.repeat(3000) });
    assert.equal(result.delivery.content_length, 2000);
    assert.equal(result.delivery.truncated, true);
    assert.equal(requests[0].body.content.length, 2000);
    assert.equal(requests[0].body.content.endsWith('...'), true);
  });
}

async function testRedactionAndDryRun() {
  for (const env of [
    { DISCORD_BOT_TOKEN: secretToken, DISCORD_OPERATIONS_CHANNEL_ID: channelId },
    { DISCORD_WEBHOOK_URL: `https://example.invalid${secretWebhook}` },
  ]) {
    const run = await notifyRaw(env, { dryRun: true });
    assert.equal(run.code, 0);
    assert.doesNotMatch(run.stdout, /webhook-secret-456|bot-secret-123|example\.invalid/);
    const result = JSON.parse(run.stdout);
    assert.equal(result.status, 'dry_run');
    assert.equal(result.attempts, 0);
  }
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

function notifyRaw(env, overrides = {}, extraArgs = []) {
  const args = ['--run-id', overrides.runId || 'run-123', '--status', 'pass', '--title', 'homeboy run pass', '--body', overrides.body || 'Run completed'];
  if (overrides.transport !== undefined) args.push('--transport', overrides.transport);
  if (overrides.route !== undefined) args.push('--route', overrides.route);
  if (overrides.dryRun) args.push('--dry-run');
  args.push(...extraArgs);
  return new Promise((resolve, reject) => {
    const childEnv = { ...process.env };
    for (const name of ['DISCORD_BOT_TOKEN', 'DISCORD_WEBHOOK_URL', 'DISCORD_OPERATIONS_CHANNEL_ID', 'DISCORD_API_BASE_URL', 'KIMAKI_BOT_TOKEN']) delete childEnv[name];
    const child = spawn(process.execPath, [helper, ...args], { env: { ...childEnv, ...env } });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.once('error', reject);
    child.once('close', (code) => resolve({ code, stdout, stderr }));
  });
}

function channelRoute(id) {
  return `discord:v1:channel:${guildId}:${id}`;
}

function threadRoute(id) {
  return `discord:v1:thread:${guildId}:${id}`;
}
