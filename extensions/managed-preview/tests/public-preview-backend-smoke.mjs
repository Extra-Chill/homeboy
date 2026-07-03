#!/usr/bin/env node

import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const extensionRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const scriptPath = resolve(extensionRoot, 'scripts/public-preview-backend.mjs');
const preflightScriptPath = resolve(extensionRoot, 'scripts/public-preview-preflight.mjs');

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

const brokerLifecycle = await listenBrokerLifecycle();
try {
  const backend = await runBackendUntilRegistered([
    scriptPath,
    '--local-url',
    'http://127.0.0.1:7331',
    '--public-url',
    'https://preview-broker.example/runs/run-123',
    '--broker-url',
    `http://127.0.0.1:${brokerLifecycle.port}/api/managed-previews`,
  ]);
  assert.equal(backend.started.status, 'starting');
  assert.equal(backend.started.registration.lifecycle.session_id, 'session-123');
  assert.equal(backend.started.registration.lifecycle.unregister_method, 'DELETE');
  assert.equal(backend.started.registration.lifecycle.heartbeat.supported, false);
  backend.child.kill('SIGTERM');
  const backendExit = await backend.closed;
  assert.equal(backendExit.status === 143 || backendExit.signal === 'SIGTERM', true, backendExit.stderr);
  assert.equal(brokerLifecycle.requests.filter((request) => request.method === 'POST').length, 2);
  assert.equal(brokerLifecycle.requests.some((request) => request.method === 'DELETE' && request.url === '/api/managed-previews/session-123'), true);
} finally {
  brokerLifecycle.server.close();
}

const invalidJsonBroker = await listenInvalidJsonBroker();
try {
  const invalidJson = await runAsync([
    scriptPath,
    '--local-url',
    'http://127.0.0.1:7331',
    '--public-url',
    'https://preview-broker.example/runs/run-123',
    '--broker-url',
    `http://127.0.0.1:${invalidJsonBroker.port}/api/managed-previews`,
  ]);
  assert.equal(invalidJson.status, 1, invalidJson.stderr);
  const invalidJsonError = JSON.parse(invalidJson.stderr);
  assert.equal(invalidJsonError.schema, 'homeboy/managed-preview-backend-error/v1');
  assert.match(invalidJsonError.error, /registration response was not valid JSON/);
} finally {
  invalidJsonBroker.server.close();
}

const runtimeServer = await listen('runtime-ready');
const mismatchServer = await listen('fetch failed');
const tmpRoot = mkdtempSync(join(os.tmpdir(), 'homeboy-public-preview-'));
try {
  const prepared = await runAsync([
    preflightScriptPath,
    '--allocate-port',
    '--prepare-only',
  ]);

  assert.equal(prepared.status, 0, prepared.stderr);
  const preparedJson = JSON.parse(prepared.stdout);
  assert.equal(preparedJson.schema, 'homeboy/public-preview-preflight/v1');
  assert.equal(preparedJson.status, 'prepared');
  assert.equal(typeof preparedJson.local_preview.port, 'number');
  assert.equal(preparedJson.public_preview, null);

  const metadataFile = resolve(tmpRoot, 'public-preview.json');
  const preflight = await runAsync([
    preflightScriptPath,
    '--preview-port',
    String(runtimeServer.port),
    '--local-url',
    `http://127.0.0.1:${runtimeServer.port}`,
    '--public-url',
    `http://127.0.0.1:${runtimeServer.port}/?token=secret`,
    '--allow-insecure-public-url',
    '--tunnel-provider',
    'smoke',
    '--tunnel-session-id',
    'session-123',
    '--expected-text',
    'runtime-ready',
    '--metadata-file',
    metadataFile,
  ]);

  assert.equal(preflight.status, 0, preflight.stderr);
  const preflightJson = JSON.parse(preflight.stdout);
  const metadataJson = JSON.parse(readFileSync(metadataFile, 'utf8'));
  assert.equal(preflightJson.schema, 'homeboy/public-preview-preflight/v1');
  assert.equal(preflightJson.local_preview.port, runtimeServer.port);
  assert.equal(preflightJson.public_preview.url, `http://127.0.0.1:${runtimeServer.port}/`);
  assert.equal(preflightJson.public_preview.provider, 'smoke');
  assert.equal(preflightJson.public_preview.session_id, 'session-123');
  assert.equal(preflightJson.public_preview.url.includes('secret'), false);
  assert.equal(preflightJson.preflight.public.body_sha256, preflightJson.preflight.local.body_sha256);
  assert.deepEqual(metadataJson, preflightJson);

  const mismatch = await runAsync([
    preflightScriptPath,
    '--preview-port',
    String(runtimeServer.port),
    '--local-url',
    `http://127.0.0.1:${runtimeServer.port}`,
    '--public-url',
    `http://127.0.0.1:${mismatchServer.port}`,
    '--allow-insecure-public-url',
    '--expected-text',
    'runtime-ready',
  ]);

  assert.equal(mismatch.status, 1, mismatch.stderr);
  const mismatchJson = JSON.parse(mismatch.stderr);
  assert.equal(mismatchJson.schema, 'homeboy/public-preview-preflight-error/v1');
  assert.equal(mismatchJson.code, 'expected_text_missing');
} finally {
  runtimeServer.server.close();
  mismatchServer.server.close();
  rmSync(tmpRoot, { recursive: true, force: true });
}

function run(args) {
  return spawnSync(process.execPath, args, {
    encoding: 'utf8',
    env: { ...process.env },
  });
}

function runAsync(args) {
  return new Promise((resolveRun) => {
    const child = spawn(process.execPath, args, {
      env: { ...process.env },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk;
    });
    child.on('close', (status) => {
      resolveRun({ status, stdout, stderr });
    });
  });
}

function runBackendUntilRegistered(args) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(process.execPath, args, {
      env: { ...process.env },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    let resolved = false;
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      stdout += chunk;
      if (resolved) {
        return;
      }
      try {
        const started = JSON.parse(stdout);
        resolved = true;
        resolveRun({ child, started, closed });
      } catch (_error) {}
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk;
    });
    const closed = new Promise((resolveClosed) => {
      child.on('close', (status, signal) => {
        resolveClosed({ status, signal, stdout, stderr });
      });
    });
    child.on('error', reject);
    child.on('close', (status) => {
      if (!resolved) {
        reject(new Error(`Backend exited before registration with status ${status}: ${stderr}`));
      }
    });
  });
}

function listenBrokerLifecycle() {
  const requests = [];
  let registerAttempts = 0;
  const server = http.createServer((request, response) => {
    requests.push({ method: request.method, url: request.url });
    if (request.method === 'POST' && request.url === '/api/managed-previews') {
      registerAttempts += 1;
      if (registerAttempts === 1) {
        response.writeHead(503, { 'content-type': 'application/json' });
        response.end(JSON.stringify({ error: 'temporary broker outage' }));
        return;
      }
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ session_id: 'session-123' }));
      return;
    }
    if (request.method === 'DELETE' && request.url === '/api/managed-previews/session-123') {
      response.writeHead(204);
      response.end();
      return;
    }
    response.writeHead(404, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ error: 'not found' }));
  });

  return listenServer(server, requests);
}

function listenInvalidJsonBroker() {
  const server = http.createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end('{not-json');
  });

  return listenServer(server, []);
}

function listenServer(server, requests) {
  return new Promise((resolveListen, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      resolveListen({ server, port: address.port, requests });
    });
  });
}

function listen(body) {
  const server = http.createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'text/plain' });
    response.end(body);
  });

  return new Promise((resolveListen, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      resolveListen({ server, port: address.port });
    });
  });
}
