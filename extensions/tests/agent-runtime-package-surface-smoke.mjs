#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const { resolveRuntimeProvider, runtimeRegistry } = require('../runtime-agent-ci/lib/runtime-provider-resolver.cjs');

const registry = runtimeRegistry({ repoRoot: rootDir });
const runtimeIds = Object.keys(registry).sort();

assert.ok(runtimeIds.includes('wp-codebox'), 'wp-codebox remains discoverable');
assert.ok(runtimeIds.includes('opencode'), 'opencode remains discoverable');
assert.ok(runtimeIds.includes('codex'), 'codex remains discoverable');
assert.ok(runtimeIds.includes('claude-code'), 'claude-code remains discoverable');
assert.ok(runtimeIds.includes('pi'), 'pi remains discoverable');
assert.ok(runtimeIds.includes('local-shell'), 'local-shell remains discoverable as the neutral runtime');
assert.equal(registry['fake-runtime'], undefined, 'fake-runtime must not ship as a discoverable runtime');

assert.throws(
  () => resolveRuntimeProvider('fake-runtime', { repoRoot: rootDir, registry }),
  /Unsupported agent_runtime: fake-runtime\./
);
assert.equal(resolveRuntimeProvider('local-shell', { repoRoot: rootDir, registry }).executor.backend, 'local-shell');

console.log('agent runtime package surface smoke passed');
