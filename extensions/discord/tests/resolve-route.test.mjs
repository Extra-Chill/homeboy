import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const extensionPath = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const resolver = path.join(extensionPath, 'scripts/resolve-route.mjs');
const request = {
  schema: 'homeboy/notification-route-resolver-request/v1',
  transport: 'discord.run-completion',
};
const threadOneId = '323456789012345678';
const threadTwoId = '423456789012345678';

await testMatchedRoute();
await testMissingContextIsUnmatched();
await testInvalidRequestsFailClosed();
await testInvalidContextFailsClosedWithoutDisclosure();
await testConcurrentInvocationsDoNotCrossRoutes();
console.log('discord route resolver tests passed');

async function testMatchedRoute() {
  const result = await resolve({ KIMAKI_THREAD_ID: threadOneId });
  assert.equal(result.code, 0);
  assert.equal(result.stderr, '');
  assert.deepEqual(JSON.parse(result.stdout), {
    schema: 'homeboy/notification-route-resolver/v1',
    status: 'matched',
    route: `discord:v1:thread:${threadOneId}`,
  });
  assert.equal(result.stdout.split('\n').filter(Boolean).length, 1);
}

async function testMissingContextIsUnmatched() {
  const result = await resolve({ DISCORD_THREAD_ID: threadOneId });
  assert.equal(result.code, 0);
  assert.deepEqual(JSON.parse(result.stdout), {
    schema: 'homeboy/notification-route-resolver/v1',
    status: 'unmatched',
  });
}

async function testInvalidRequestsFailClosed() {
  const invalidRequests = [
    '{',
    JSON.stringify({ ...request, schema: 'unsupported' }),
    JSON.stringify({ ...request, transport: 'other.transport' }),
    JSON.stringify({ ...request, unexpected: true }),
  ];
  for (const input of invalidRequests) {
    const result = await resolve({ KIMAKI_THREAD_ID: threadOneId }, input);
    assert.equal(result.code, 2);
    assert.equal(result.stdout, '');
    assert.equal(result.stderr, 'Invalid notification route resolver request\n');
  }
}

async function testInvalidContextFailsClosedWithoutDisclosure() {
  const secret = 'token=do-not-disclose';
  for (const threadId of ['123', 'not-a-snowflake', '1'.repeat(21), secret]) {
    const result = await resolve({ KIMAKI_THREAD_ID: threadId });
    assert.equal(result.code, 2);
    assert.equal(result.stdout, '');
    assert.equal(result.stderr, 'Invalid Discord thread attribution\n');
    assert.equal(`${result.stdout}${result.stderr}`.includes(threadId), false);
  }
}

async function testConcurrentInvocationsDoNotCrossRoutes() {
  const [first, second] = await Promise.all([
    resolve({ KIMAKI_THREAD_ID: threadOneId }),
    resolve({ KIMAKI_THREAD_ID: threadTwoId }),
  ]);
  assert.equal(JSON.parse(first.stdout).route, `discord:v1:thread:${threadOneId}`);
  assert.equal(JSON.parse(second.stdout).route, `discord:v1:thread:${threadTwoId}`);
}

function resolve(env = {}, input = JSON.stringify(request)) {
  return new Promise((resolveResult, reject) => {
    const child = spawn(process.execPath, [resolver], { env, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => (stdout += chunk));
    child.stderr.on('data', (chunk) => (stderr += chunk));
    child.on('error', reject);
    child.on('close', (code) => resolveResult({ code, stdout, stderr }));
    child.stdin.end(input);
  });
}
