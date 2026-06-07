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
  'external-broker',
  '--local-url',
  'http://127.0.0.1:7331',
  '--public-url',
  'https://preview-broker.example/runs/run-123',
  '--broker-url',
  'https://preview-broker.example/api/managed-previews',
  '--target-id',
  'wpcom-ai-landing',
  '--target-url',
  'https://wordpress.com/ai/',
  '--route',
  'landing=https://wordpress.com/ai/',
  '--route',
  'builder_handoff=https://wordpress.com/setup/ai-site-builder',
  '--dry-run',
]);

assert.equal(planned.status, 0, planned.stderr);
const plannedJson = JSON.parse(planned.stdout);
assert.equal(plannedJson.schema, 'homeboy/managed-preview-backend-start/v1');
assert.equal(plannedJson.status, 'planned');
assert.equal(plannedJson.provider, 'external-broker');
assert.equal(plannedJson.registration.status, 'planned');
assert.equal(plannedJson.registration.broker_url, 'https://preview-broker.example/api/managed-previews');
assert.equal(plannedJson.target.id, 'wpcom-ai-landing');
assert.equal(plannedJson.target.url, 'https://wordpress.com/ai/');
assert.equal(plannedJson.target.routes.landing, 'https://wordpress.com/ai/');
assert.equal(plannedJson.target.routes.builder_handoff, 'https://wordpress.com/setup/ai-site-builder');

const blocked = run([
  scriptPath,
  '--local-url',
  'http://calypso.localhost:3000',
  '--public-url',
  'https://preview-broker.example/runs/run-123',
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
assert.match(blockedJson.reason, /requires HOMEBOY_PREVIEW_BROKER_URL|--broker-url/);

const preserved = run([
  scriptPath,
  '--local-url',
  'http://calypso.localhost:3000',
  '--public-url',
  'https://preview-broker.example/runs/run-123',
  '--broker-url',
  'https://preview-broker.example/api/managed-previews',
  '--target-id',
  'calypso-start',
  '--target-url',
  'http://calypso.localhost:3000/start',
  '--route',
  'start=http://calypso.localhost:3000/start',
  '--route',
  'builder_handoff=http://calypso.localhost:3000/setup/ai-site-builder',
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
assert.equal(preservedJson.host_preservation.mode, 'broker-must-prove-host-preservation');
assert.equal(preservedJson.registration.request.target.id, 'calypso-start');
assert.equal(preservedJson.registration.request.target.routes.start, 'http://calypso.localhost:3000/start');
assert.equal(preservedJson.registration.request.target.routes.builder_handoff, 'http://calypso.localhost:3000/setup/ai-site-builder');

function run(args) {
  return spawnSync(process.execPath, args, {
    encoding: 'utf8',
    env: { ...process.env },
  });
}
