#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
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
assert.equal(DEFAULT_RUNTIME_ID, 'local-shell');
assert.ok(registry['local-shell'], 'local-shell is registered as the default runtime provider');
assert.equal(registry['wp-codebox'], undefined, 'wp-codebox is a runtime dependency, not an agent-task provider');
assert.ok(registry.opencode, 'opencode is registered as a selectable runtime provider');

const defaultRuntime = resolveRuntimeProvider(undefined, { repoRoot: rootDir, workspace });
assert.equal(defaultRuntime.id, 'local-shell');
assert.equal(defaultRuntime.source.kind, 'repo');
assert.equal(defaultRuntime.source.source_path, path.join(rootDir, 'agent-runtimes/local-shell'));
assert.equal(defaultRuntime.source.manifest_path, path.join(rootDir, 'agent-runtimes/local-shell/local-shell.json'));
assert.equal(defaultRuntime.executor.backend, 'local-shell');
assert.equal(defaultRuntime.executor.path, path.join(rootDir, 'agent-runtimes/local-shell/scripts/agent/homeboy-local-shell-agent-task-executor.cjs'));

assert.throws(() => resolveRuntimeProvider('wp-codebox', { repoRoot: rootDir, workspace }), /Unsupported agent_runtime: wp-codebox/);

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
assert.equal(opencodeRuntime.manifest.name, 'OpenCode');
assert.equal(JSON.stringify(opencodeRuntime.manifest).includes('wp-codebox'), false);
assert.equal(JSON.stringify(opencodeRuntime.manifest).includes('WP Codebox'), false);

const tempDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-runtime-provider-')));
const externalRuntimeDir = path.join(tempDir, 'external-runtime');
fs.mkdirSync(path.join(externalRuntimeDir, 'scripts'), { recursive: true });
const externalManifestPath = path.join(externalRuntimeDir, 'external-runtime.json');
fs.writeFileSync(externalManifestPath, JSON.stringify({
	schema: 'homeboy/agent-runtime-manifest/v1',
	id: 'external-runtime',
	agent_task_executors: [executorFixture('external.agent', 'external-shell', 'active', [])],
}, null, 2));
const externalRuntime = resolveRuntimeProvider('external-runtime', {
	repoRoot: rootDir,
	runtimeManifestPath: externalManifestPath,
	workspace,
});
assert.equal(externalRuntime.source.kind, 'manifest');
assert.equal(externalRuntime.source.source_path, externalRuntimeDir);
assert.equal(externalRuntime.source.manifest_path, externalManifestPath);
assert.equal(externalRuntime.executor.path, path.join(externalRuntimeDir, 'scripts/external.agent.cjs'));

const packageRoot = path.join(tempDir, 'node_modules/@example/homeboy-runtime');
fs.mkdirSync(packageRoot, { recursive: true });
fs.writeFileSync(path.join(packageRoot, 'package.json'), JSON.stringify({
	name: '@example/homeboy-runtime',
	version: '1.0.0',
	homeboy: { agent_runtime_manifest: 'runtime-manifest.json' },
}, null, 2));
fs.writeFileSync(path.join(packageRoot, 'runtime-manifest.json'), JSON.stringify({
	schema: 'homeboy/agent-runtime-manifest/v1',
	id: 'packaged-runtime',
	agent_task_executors: [executorFixture('packaged.agent', 'package-shell', 'active', [])],
}, null, 2));
const packagedRuntime = resolveRuntimeProvider('packaged-runtime', {
	repoRoot: rootDir,
	runtimePackage: '@example/homeboy-runtime',
	packageBasePath: tempDir,
	workspace,
});
assert.equal(packagedRuntime.source.kind, 'package');
assert.equal(packagedRuntime.source.package, '@example/homeboy-runtime');
assert.equal(packagedRuntime.source.source_path, packageRoot);
assert.equal(packagedRuntime.source.manifest_path, path.join(packageRoot, 'runtime-manifest.json'));
assert.equal(packagedRuntime.executor.path, path.join(packageRoot, 'scripts/packaged.agent.cjs'));

assert.throws(
	() => runtimeRegistry({ repoRoot: rootDir, runtimePackage: '@example/missing-runtime', packageBasePath: tempDir }),
	/Runtime package @example\/missing-runtime could not be resolved .* Install the package or pass runtimeManifests\/runtimeManifestPath for local iteration\./
);

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
