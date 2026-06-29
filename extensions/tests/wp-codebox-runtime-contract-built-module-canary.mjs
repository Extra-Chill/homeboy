#!/usr/bin/env node
// Loads the canonical wp-codebox runtime contract manifest through the runner's
// own loader, against the REAL built @automattic/wp-codebox-core module pointed
// at by HOMEBOY_WP_CODEBOX_CORE_MODULE. Unit smokes exercise a hand-written
// fixture, which can silently drift from the published contract (as it did when
// Automattic/wp-codebox#1637 dropped schemas.agentTask.legacyRunResponse). This
// canary asserts the manifest actually resolves from the built module the same
// way the "Build runner config" step does, so a contract drift fails CI here
// instead of in every downstream runtime-agent-full-run.

import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const coreModule = process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE || process.env.WP_CODEBOX_CORE_MODULE;
assert.ok(coreModule, 'HOMEBOY_WP_CODEBOX_CORE_MODULE (or WP_CODEBOX_CORE_MODULE) must point at the built wp-codebox runtime-core contracts module.');
assert.ok(fs.existsSync(coreModule), `Built wp-codebox runtime-core module not found at ${coreModule}. Build packages/runtime-core before running the canary.`);

const {
  loadCanonicalRuntimeContractSource,
  loadCanonicalRuntimeContractSourceSync,
  RUNTIME_CONTRACT_MANIFEST_SCHEMA,
} = await import(path.join(rootDir, 'agent-runtimes', 'wp-codebox', 'lib', 'wp-codebox-runtime-contract-source.js'));

// Sync path mirrors build-runner-config.cjs.
const sync = loadCanonicalRuntimeContractSourceSync({ required: true });
assert.equal(sync.canonical, true, 'Sync loader must resolve the canonical runtime contract manifest from the built module.');
assert.equal(sync.manifest.schema, RUNTIME_CONTRACT_MANIFEST_SCHEMA);
assert.equal(typeof sync.manifest.schemas.agentTask.runResult, 'string', 'Canonical manifest must publish schemas.agentTask.runResult.');

// Async path mirrors the providerRuntime consumers.
const asyncResult = await loadCanonicalRuntimeContractSource({ required: true });
assert.equal(asyncResult.canonical, true, 'Async loader must resolve the canonical runtime contract manifest from the built module.');
assert.deepEqual(asyncResult.manifest, sync.manifest, 'Async and sync loaders must resolve the same canonical manifest.');

console.log(`wp-codebox runtime contract built-module canary passed (schema ${sync.manifest.schema}, source ${sync.source})`);
