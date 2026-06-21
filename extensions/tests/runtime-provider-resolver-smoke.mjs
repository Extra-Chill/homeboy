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
assert.ok(registry.opencode, 'opencode is registered as a selectable runtime provider');

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
assert.equal(runtime.executor.id, 'wordpress.codebox-agent-task-executor');
assert.equal(runtime.executor.backend, 'codebox');
assert.equal(runtime.executor.path, path.join(rootDir, 'agent-runtimes/wp-codebox/scripts/agent/homeboy-codebox-agent-task-executor.cjs'));

const opencodeRuntime = resolveRuntimeProvider('opencode', {
	repoRoot: rootDir,
	workspace,
});
assert.equal(opencodeRuntime.id, 'opencode');
assert.equal(opencodeRuntime.checkout.repo, '');
assert.equal(opencodeRuntime.executor.backend, 'opencode');
assert.equal(opencodeRuntime.executor.path, path.join(rootDir, 'agent-runtimes/opencode/scripts/agent/homeboy-opencode-agent-task-executor.cjs'));
assert.equal(opencodeRuntime.manifest.agent_task_executors[0].status, 'active');
assert.equal(opencodeRuntime.manifest.agent_task_executors[0].capabilities.includes('nested_orchestrator'), true);

const multiRegistry = {
	'multi-executor': {
		schema: 'homeboy/agent-runtime-manifest/v1',
		id: 'multi-executor',
		agent_task_executors: [
			executorFixture('runtime.alpha', 'runner', 'active', ['repo_workspace', 'patch_artifacts']),
			executorFixture('runtime.beta', 'runner', 'active', ['repo_workspace', 'browser_runtime']),
			executorFixture('runtime.gamma', 'shell', 'active', ['repo_workspace']),
			executorFixture('runtime.retired', 'runner', 'disabled', ['repo_workspace']),
		],
	},
};

const explicitRuntime = resolveRuntimeProvider('multi-executor', {
	repoRoot: rootDir,
	registry: multiRegistry,
	executor: { id: 'runtime.beta' },
});
assert.equal(explicitRuntime.executor.id, 'runtime.beta');
assert.equal(explicitRuntime.executor.backend, 'runner');

const backendRuntime = resolveRuntimeProvider('multi-executor', {
	repoRoot: rootDir,
	registry: multiRegistry,
	executor: { backend: 'shell' },
});
assert.equal(backendRuntime.executor.id, 'runtime.gamma');

const capabilityRuntime = resolveRuntimeProvider('multi-executor', {
	repoRoot: rootDir,
	registry: multiRegistry,
	executor: { backend: 'runner', capability: 'browser_runtime' },
});
assert.equal(capabilityRuntime.executor.id, 'runtime.beta');

const disabledRuntime = resolveRuntimeProvider('multi-executor', {
	repoRoot: rootDir,
	registry: multiRegistry,
	executor: { backend: 'runner', status: 'disabled' },
});
assert.equal(disabledRuntime.executor.id, 'runtime.retired');

assert.throws(
	() => resolveRuntimeProvider('multi-executor', { repoRoot: rootDir, registry: multiRegistry }),
	/Ambiguous agent_task_executors for runtime multi-executor match status=active: runtime\.alpha .*runtime\.beta/
);

assert.throws(
	() => resolveRuntimeProvider('multi-executor', {
		repoRoot: rootDir,
		registry: multiRegistry,
		executor: { id: 'runtime.retired' },
	}),
	/No agent_task_executors for runtime multi-executor match id=runtime\.retired status=active\./
);

assert.throws(
	() => resolveRuntimeProvider('missing-runtime', { repoRoot: rootDir, workspace }),
	/Unsupported agent_runtime: missing-runtime\./
);

console.log('runtime provider resolver smoke passed');

function executorFixture(id, backend, status, capabilities) {
	return {
		schema: 'homeboy/agent-task-executor-provider/v1',
		id,
		backend,
		status,
		capabilities,
		invocation: {
			argv: ['node', `{{runtime_path}}/scripts/${id}.cjs`],
		},
	};
}
