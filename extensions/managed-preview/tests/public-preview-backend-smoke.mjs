#!/usr/bin/env node

import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const extensionRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const scriptPath = resolve(extensionRoot, 'scripts/public-preview-backend.mjs');

const planned = run([
  scriptPath,
  '--provider',
  'kimaki',
  '--local-url',
  'http://127.0.0.1:7331',
  '--public-url',
  'https://site-preview.kimaki.dev',
  '--dry-run',
]);

assert.equal(planned.status, 0, planned.stderr);
const plannedJson = JSON.parse(planned.stdout);
assert.equal(plannedJson.schema, 'homeboy/managed-preview-backend-start/v1');
assert.equal(plannedJson.status, 'planned');
assert.equal(plannedJson.provider, 'kimaki');
assert.deepEqual(plannedJson.args.slice(0, 5), ['tunnel', '--port', '7331', '--host', '127.0.0.1']);
assert.ok(plannedJson.args.includes('site-preview'));

const blocked = run([
  scriptPath,
  '--provider',
  'kimaki',
  '--local-url',
  'http://calypso.localhost:3000',
  '--public-url',
  'https://calypso-preview.kimaki.dev',
  '--expected-effective-origin',
  'http://calypso.localhost:3000',
  '--expected-config-hostname',
  'calypso.localhost',
  '--require-host-preservation',
  '--dry-run',
]);

assert.equal(blocked.status, 78, blocked.stderr);
const blockedJson = JSON.parse(blocked.stderr);
assert.equal(blockedJson.schema, 'homeboy/managed-preview-backend-blocker/v1');
assert.equal(blockedJson.status, 'blocked');
assert.match(blockedJson.reason, /requires browser-effective origin http:\/\/calypso\.localhost:3000/);

const preserved = run([
  scriptPath,
  '--provider',
  'kimaki',
  '--local-url',
  'http://calypso.localhost:3000',
  '--public-url',
  'http://calypso.localhost:3000',
  '--expected-effective-origin',
  'http://calypso.localhost:3000',
  '--expected-config-hostname',
  'calypso.localhost',
  '--require-host-preservation',
  '--dry-run',
]);

assert.equal(preserved.status, 0, preserved.stderr);
const preservedJson = JSON.parse(preserved.stdout);
assert.equal(preservedJson.host_preservation.supported, true);
assert.equal(preservedJson.host_preservation.mode, 'public-url-preserves-browser-origin');

function run(args) {
  return spawnSync(process.execPath, args, {
    encoding: 'utf8',
    env: { ...process.env, HOMEBOY_KIMAKI_BIN: 'kimaki' },
  });
}
