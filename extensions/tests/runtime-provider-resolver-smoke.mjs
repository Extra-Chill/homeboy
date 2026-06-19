#!/usr/bin/env node
import assert from 'node:assert/strict';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const workspace = path.join(rootDir, 'fixture-workspace');
const {
	DEFAULT_RUNTIME_ID,
	resolveRuntimeProvider,
	runtimeRegistry,
} = require('../runtime-agent-ci/lib/runtime-provider-resolver.cjs');

const registry = runtimeRegistry({ repoRoot: rootDir });
assert.equal(DEFAULT_RUNTIME_ID, 'wp-codebox');
assert.ok(registry['wp-codebox'], 'wp-codebox is registered as the default runtime provider');

const runtime = resolveRuntimeProvider('wp-codebox', {
	repoRoot: rootDir,
	workspace,
	env: { AGENT_RUNTIME_REF: 'feature/runtime-ref' },
});

assert.equal(runtime.id, 'wp-codebox');
assert.equal(runtime.checkout.repo, 'Automattic/wp-codebox');
assert.equal(runtime.checkout.ref, 'feature/runtime-ref');
assert.equal(runtime.checkout.target, '.ci/wp-codebox');
assert.equal(runtime.checkout.targetPath, path.join(workspace, '.ci/wp-codebox'));
assert.deepEqual(runtime.setupCommands, [{ command: 'npm', args: ['install'], cwd: '.ci/wp-codebox' }]);
assert.deepEqual(runtime.buildCommands, [{ command: 'npm', args: ['run', 'build'], cwd: '.ci/wp-codebox' }]);
assert.equal(runtime.paths.runtime_bin, path.join(workspace, '.ci/wp-codebox/packages/cli/dist/index.js'));
assert.equal(runtime.paths.runtime_component, path.join(workspace, '.ci/wp-codebox/packages/wordpress-plugin'));
assert.equal(runtime.executor.backend, 'codebox');
assert.equal(runtime.executor.path, path.join(rootDir, 'agent-runtimes/wp-codebox/scripts/agent/homeboy-codebox-agent-task-executor.cjs'));

assert.throws(
	() => resolveRuntimeProvider('missing-runtime', { repoRoot: rootDir, workspace }),
	/Unsupported agent_runtime: missing-runtime\./
);

console.log('runtime provider resolver smoke passed');
