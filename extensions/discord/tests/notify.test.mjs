import assert from 'node:assert/strict';
import http from 'node:http';
import { once } from 'node:events';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs';
import os from 'node:os';

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
await testDeprecatedBotTokenAlias();
await testOwningSessionThreadDeliversThroughCommandSender();
await testOwningSessionThreadDeliversThroughHttpSender();
await testSessionWithoutSenderUsesRest();
await testSessionSenderMisconfigurationFailsClosed();
await testDeprecatedSessionAliases();
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

// A chat bridge drops messages authored by its own bot, so a REST post to the
// thread that owns this run is invisible to that session. The owning thread is
// delivered through the configured session sender so the notification becomes
// a real turn.
async function testOwningSessionThreadDeliversThroughCommandSender() {
  const { stub, readArgv, cleanup } = argvStub();

  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      {
        DISCORD_BOT_TOKEN: secretToken,
        HOMEBOY_SESSION_THREAD_ID: threadOneId,
        // Whitespace-separated, run without a shell: a command plus subcommand.
        HOMEBOY_SESSION_SEND_COMMAND: `${stub} send`,
        DISCORD_API_BASE_URL: `${baseUrl}/api/v10`,
      },
      { route: threadRoute(threadOneId), body: 'cook finished' },
    );
    assert.equal(result.status, 'delivered');
    assert.equal(result.delivery.mode, 'session');
    assert.equal(result.delivery.destination, 'session_thread');
    assert.equal(result.deprecations, undefined);
    assert.equal(requests.length, 0);
  });

  const argv = readArgv();
  assert.deepEqual(argv.slice(0, 4), ['send', '--thread', threadOneId, '--prompt']);
  assert.match(argv[4], /cook finished/);

  // A run that only announces its start carries no outcome to act on, so it is
  // posted for the human to read rather than interrupting the agent.
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      {
        DISCORD_BOT_TOKEN: secretToken,
        HOMEBOY_SESSION_THREAD_ID: threadOneId,
        HOMEBOY_SESSION_SEND_COMMAND: stub,
        DISCORD_API_BASE_URL: `${baseUrl}/api/v10`,
      },
      { route: threadRoute(threadOneId), status: 'started' },
    );
    assert.equal(result.delivery.mode, 'bot');
    assert.equal(requests[0].url, `/api/v10/channels/${threadOneId}/messages`);
  });

  // The route, not the delivering process, names the owner. A long-lived
  // daemon started from another (or a stale) session still delivers the
  // routed thread through the sender, and needs no REST credentials to do it.
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      {
        HOMEBOY_SESSION_THREAD_ID: threadOneId,
        HOMEBOY_SESSION_SEND_COMMAND: stub,
        DISCORD_API_BASE_URL: `${baseUrl}/api/v10`,
      },
      { route: threadRoute(threadTwoId), status: 'durable_failure', body: 'cook needs attention' },
    );
    assert.equal(result.status, 'delivered');
    assert.equal(result.delivery.mode, 'session');
    assert.equal(requests.length, 0);
  });
  assert.deepEqual(readArgv().slice(0, 2), ['--thread', threadTwoId]);

  // A host with no session context at all still delivers through a configured
  // sender: the sender, not the thread variable, makes it a session host.
  await withServer(async ({ requests }) => {
    const result = await notify({ HOMEBOY_SESSION_SEND_COMMAND: stub }, { route: threadRoute(threadTwoId) });
    assert.equal(result.delivery.mode, 'session');
    assert.equal(requests.length, 0);
  });

  // Channel routes are not sessions; they keep REST delivery.
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      { DISCORD_BOT_TOKEN: secretToken, HOMEBOY_SESSION_SEND_COMMAND: stub, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` },
      { route: channelRoute(channelId) },
    );
    assert.equal(result.delivery.mode, 'bot');
    assert.equal(requests[0].url, `/api/v10/channels/${channelId}/messages`);
  });

  // A failing sender is a typed delivery failure, not a silent success.
  const failingStub = path.join(path.dirname(stub), 'failing-stub.mjs');
  fs.writeFileSync(failingStub, '#!/usr/bin/env node\nprocess.stderr.write("thread gone");\nprocess.exit(3);\n');
  fs.chmodSync(failingStub, 0o755);
  const failing = await notifyRaw(
    { HOMEBOY_SESSION_THREAD_ID: threadOneId, HOMEBOY_SESSION_SEND_COMMAND: failingStub },
    { route: threadRoute(threadOneId) },
  );
  assert.equal(failing.code, 1);
  const failure = JSON.parse(failing.stdout);
  assert.equal(failure.error.kind, 'delivery_error');
  assert.match(failure.error.message, /status 3: thread gone/);

  cleanup();
}

async function testOwningSessionThreadDeliversThroughHttpSender() {
  const sendToken = 'session-send-secret-789';
  const tokenDir = fs.mkdtempSync(path.join(os.tmpdir(), 'discord-notify-token-'));
  const tokenFile = path.join(tokenDir, 'token');
  fs.writeFileSync(tokenFile, `${sendToken}\n`);

  // Streams NDJSON progress ending in a zero exit, as a bridge's local send
  // endpoint does.
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      {
        DISCORD_BOT_TOKEN: secretToken,
        HOMEBOY_SESSION_THREAD_ID: threadOneId,
        HOMEBOY_SESSION_SEND_URL: `${baseUrl}/bridge/send`,
        HOMEBOY_SESSION_SEND_TOKEN_FILE: tokenFile,
        DISCORD_API_BASE_URL: `${baseUrl}/api/v10`,
      },
      { route: threadRoute(threadOneId), body: 'cook finished' },
    );
    assert.equal(result.status, 'delivered');
    assert.equal(result.delivery.mode, 'session');
    assert.equal(requests.length, 1);
    assert.equal(requests[0].url, '/bridge/send');
    assert.equal(requests[0].headers.authorization, `Bearer ${sendToken}`);
    assert.equal(requests[0].body.options.thread, threadOneId);
    assert.match(requests[0].body.options.prompt, /cook finished/);
  }, (_request, response) =>
    response.writeHead(200, { 'content-type': 'application/x-ndjson' })
      .end('{"stream":"stdout","data":"sent"}\n{"exit":0}\n'));

  // A non-zero exit reported by the endpoint is a delivery failure.
  await withServer(async ({ baseUrl }) => {
    const run = await notifyRaw(
      { HOMEBOY_SESSION_THREAD_ID: threadOneId, HOMEBOY_SESSION_SEND_URL: `${baseUrl}/bridge/send`, HOMEBOY_SESSION_SEND_TOKEN: sendToken },
      { route: threadRoute(threadOneId) },
    );
    assert.equal(run.code, 1);
    assert.equal(JSON.parse(run.stdout).error.kind, 'delivery_error');
  }, (_request, response) => response.writeHead(200).end('{"exit":1}\n'));

  // A rejected token is an auth failure that never echoes the token.
  await withServer(async ({ baseUrl }) => {
    const run = await notifyRaw(
      { HOMEBOY_SESSION_THREAD_ID: threadOneId, HOMEBOY_SESSION_SEND_URL: `${baseUrl}/bridge/send`, HOMEBOY_SESSION_SEND_TOKEN: sendToken },
      { route: threadRoute(threadOneId) },
    );
    assert.equal(run.code, 1);
    const result = JSON.parse(run.stdout);
    assert.equal(result.error.kind, 'auth_error');
    assert.equal(result.error.http_status, 401);
    assert.doesNotMatch(run.stdout, /session-send-secret-789/);
  }, (_request, response) => response.writeHead(401).end('unauthorized'));

  // Dry run validates the sender without network I/O or secret output.
  const dry = await notifyRaw(
    { HOMEBOY_SESSION_THREAD_ID: threadOneId, HOMEBOY_SESSION_SEND_URL: 'http://127.0.0.1:9/bridge/send', HOMEBOY_SESSION_SEND_TOKEN_FILE: tokenFile },
    { route: threadRoute(threadOneId), dryRun: true },
  );
  assert.equal(dry.code, 0);
  assert.equal(JSON.parse(dry.stdout).delivery.mode, 'session');
  assert.doesNotMatch(dry.stdout, /session-send-secret-789|bridge\/send/);

  fs.rmSync(tokenDir, { recursive: true, force: true });
}

// Without a sender, a thread route is still reachable over REST; only the
// host decides whether a session turn is possible. Session context alone is
// not a sender.
async function testSessionWithoutSenderUsesRest() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      { DISCORD_BOT_TOKEN: secretToken, HOMEBOY_SESSION_THREAD_ID: threadOneId, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` },
      { route: threadRoute(threadOneId) },
    );
    assert.equal(result.delivery.mode, 'bot');
    assert.equal(requests[0].url, `/api/v10/channels/${threadOneId}/messages`);
  });
}

async function testSessionSenderMisconfigurationFailsClosed() {
  for (const env of [
    { HOMEBOY_SESSION_SEND_COMMAND: 'bridge send', HOMEBOY_SESSION_SEND_URL: 'http://127.0.0.1:9/send', HOMEBOY_SESSION_SEND_TOKEN: 'x' },
    { HOMEBOY_SESSION_SEND_URL: 'http://127.0.0.1:9/send' },
    { HOMEBOY_SESSION_SEND_URL: 'ftp://127.0.0.1/send', HOMEBOY_SESSION_SEND_TOKEN: 'x' },
    { HOMEBOY_SESSION_SEND_URL: 'http://127.0.0.1:9/send', HOMEBOY_SESSION_SEND_TOKEN_FILE: '/nonexistent/secret-path/token' },
  ]) {
    const run = await notifyRaw({ ...env, HOMEBOY_SESSION_THREAD_ID: threadOneId }, { route: threadRoute(threadOneId) });
    assert.equal(run.code, 1);
    assert.equal(JSON.parse(run.stdout).error.kind, 'input_error');
    assert.doesNotMatch(run.stdout, /secret-path/);
  }
}

// The bridge-specific names this extension used to read keep working for one
// release and are reported as deprecated on the result envelope.
async function testDeprecatedSessionAliases() {
  const { stub, readArgv, cleanup } = argvStub();

  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      {
        KIMAKI_BOT_TOKEN: secretToken,
        KIMAKI_THREAD_ID: threadOneId,
        KIMAKI_CLI: stub,
        DISCORD_API_BASE_URL: `${baseUrl}/api/v10`,
      },
      { route: threadRoute(threadOneId), body: 'cook finished' },
    );
    assert.equal(result.status, 'delivered');
    assert.equal(result.delivery.mode, 'session');
    assert.equal(requests.length, 0);
    assert.deepEqual(result.deprecations.map((entry) => entry.name).sort(), ['KIMAKI_CLI', 'KIMAKI_THREAD_ID']);
    assert.equal(result.deprecations.find((entry) => entry.name === 'KIMAKI_THREAD_ID').replacement, 'HOMEBOY_SESSION_THREAD_ID');
  });
  assert.deepEqual(readArgv().slice(0, 4), ['send', '--thread', threadOneId, '--prompt']);

  // As with the generic contract, a legacy host delivers the routed thread even
  // when its own session is a different (or stale) one, with no REST
  // credentials. KIMAKI_CLI alone also marks a legacy host.
  for (const env of [{ KIMAKI_THREAD_ID: threadOneId, KIMAKI_CLI: stub }, { KIMAKI_CLI: stub }]) {
    await withServer(async ({ requests }) => {
      const result = await notify(env, { route: threadRoute(threadTwoId), status: 'durable_failure' });
      assert.equal(result.status, 'delivered');
      assert.equal(result.delivery.mode, 'session');
      assert.equal(requests.length, 0);
    });
    assert.deepEqual(readArgv().slice(0, 3), ['send', '--thread', threadTwoId]);
  }

  // The generic sender wins over the deprecated aliases when both are set, and
  // then no deprecated name is in use.
  const { stub: genericStub, readArgv: readGenericArgv, cleanup: cleanupGeneric } = argvStub();
  await withServer(async ({ requests }) => {
    const result = await notify(
      { HOMEBOY_SESSION_SEND_COMMAND: genericStub, KIMAKI_THREAD_ID: threadOneId, KIMAKI_CLI: stub },
      { route: threadRoute(threadOneId) },
    );
    assert.equal(result.delivery.mode, 'session');
    assert.equal(result.deprecations, undefined);
    assert.equal(requests.length, 0);
  });
  assert.deepEqual(readGenericArgv().slice(0, 3), ['--thread', threadOneId, '--prompt']);
  cleanupGeneric();

  cleanup();
}

async function testDeprecatedBotTokenAlias() {
  await withServer(async ({ baseUrl, requests }) => {
    const result = await notify(
      { KIMAKI_BOT_TOKEN: secretToken, DISCORD_API_BASE_URL: `${baseUrl}/api/v10` },
      { route: threadRoute(threadOneId) },
    );
    assert.equal(result.status, 'delivered');
    assert.equal(result.delivery.mode, 'bot');
    assert.equal(requests[0].url, `/api/v10/channels/${threadOneId}/messages`);
    assert.equal(requests[0].headers.authorization, `Bot ${secretToken}`);
    assert.deepEqual(result.deprecations, [{ name: 'KIMAKI_BOT_TOKEN', replacement: 'DISCORD_BOT_TOKEN' }]);
  });
}

function argvStub() {
  const stubDir = fs.mkdtempSync(path.join(os.tmpdir(), 'discord-notify-sender-'));
  const argvLog = path.join(stubDir, 'argv.json');
  const stub = path.join(stubDir, 'sender-stub.mjs');
  fs.writeFileSync(
    stub,
    `#!/usr/bin/env node\nimport fs from 'node:fs';\nfs.writeFileSync(${JSON.stringify(argvLog)}, JSON.stringify(process.argv.slice(2)));\n`,
  );
  fs.chmodSync(stub, 0o755);
  return {
    stub,
    readArgv: () => JSON.parse(fs.readFileSync(argvLog, 'utf8')),
    cleanup: () => fs.rmSync(stubDir, { recursive: true, force: true }),
  };
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
  // Canonical guild-less form emitted by the session route resolver
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
    requests.push({ url: request.url, headers: request.headers, body: raw ? JSON.parse(raw) : undefined });
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
  const args = ['--run-id', overrides.runId || 'run-123', '--status', overrides.status || 'pass', '--title', 'homeboy run pass', '--body', overrides.body || 'Run completed'];
  if (overrides.transport !== undefined) args.push('--transport', overrides.transport);
  if (overrides.route !== undefined) args.push('--route', overrides.route);
  if (overrides.dryRun) args.push('--dry-run');
  args.push(...extraArgs);
  return new Promise((resolve, reject) => {
    const childEnv = { ...process.env };
    for (const name of Object.keys(childEnv)) {
      if (/^(DISCORD_|HOMEBOY_SESSION_|KIMAKI_|ROADIE_)/.test(name)) delete childEnv[name];
    }
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
