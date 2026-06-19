'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	normalizeRuntimeId,
	resolveRuntimeProvider,
	runtimeRegistry,
} = require('../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');
const {
	agentTaskRunnerSpec,
} = require('../../runtime-agent-ci/lib/agent-task-runner-contract');
const {
	runtimeAgentCiRunnerSpec,
} = require('../../runtime-agent-ci');
const {
	buildConfig,
} = require('../../.github/scripts/runtime-agent-full-run/build-runner-config.cjs');
const {
	dependencyEntries,
} = require('../../.github/scripts/runtime-agent-full-run/materialize-dependencies.cjs');

const repoRoot = path.join(__dirname, '..', '..');
const registry = runtimeRegistry({ repoRoot });

assert.equal(registry['wp-codebox'].id, 'wp-codebox');
assert.equal(registry['fake-runtime'], undefined);
assert.equal(registry['local-shell'], undefined);
assert.equal(registry['package'], undefined);
assert.equal(registry['homeboy-agent-task-core-contract'], undefined);
assert.equal(normalizeRuntimeId('codebox'), 'wp-codebox');
assert.equal(resolveRuntimeProvider('codebox', { repoRoot, registry }).id, 'wp-codebox');

const wpCodeboxRuntime = resolveRuntimeProvider('wp-codebox', { repoRoot, registry });
assert.equal(wpCodeboxRuntime.id, 'wp-codebox');
assert.equal(wpCodeboxRuntime.executor.backend, 'codebox');
assert.equal(wpCodeboxRuntime.executor.path, path.join(repoRoot, 'agent-runtimes/wp-codebox/scripts/agent/homeboy-codebox-agent-task-executor.cjs'));

const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-runtime-registry-'));
try {
	const runtimesRoot = path.join(tmpRoot, 'agent-runtimes');
	fs.mkdirSync(path.join(runtimesRoot, 'valid-runtime'), { recursive: true });
	fs.mkdirSync(path.join(runtimesRoot, 'adjacent-runtime'), { recursive: true });
	fs.mkdirSync(path.join(runtimesRoot, 'preferred-runtime'), { recursive: true });
	fs.mkdirSync(path.join(runtimesRoot, 'invalid-runtime'), { recursive: true });
	fs.writeFileSync(path.join(runtimesRoot, 'valid-runtime', 'valid-runtime.json'), JSON.stringify({
		schema: 'homeboy/agent-runtime-manifest/v1',
		id: 'valid-runtime',
		agent_task_executors: [],
	}));
	fs.writeFileSync(path.join(runtimesRoot, 'adjacent-runtime', 'manifest.json'), JSON.stringify({
		schema: 'homeboy/agent-runtime-manifest/v1',
		id: 'adjacent-runtime',
		agent_task_executors: [],
	}));
	fs.writeFileSync(path.join(runtimesRoot, 'preferred-runtime', 'preferred-runtime.json'), JSON.stringify({
		schema: 'homeboy/agent-runtime-manifest/v1',
		id: 'preferred-runtime',
		name: 'Exact manifest',
		agent_task_executors: [],
	}));
	fs.writeFileSync(path.join(runtimesRoot, 'preferred-runtime', 'z-adjacent.json'), JSON.stringify({
		schema: 'homeboy/agent-runtime-manifest/v1',
		id: 'preferred-runtime',
		name: 'Adjacent manifest',
		agent_task_executors: [],
	}));
	fs.writeFileSync(path.join(runtimesRoot, 'invalid-runtime', 'invalid-runtime.json'), JSON.stringify({
		schema: 'homeboy/agent-runtime-manifest/v1',
		name: 'missing id and executors',
	}));
	fs.writeFileSync(path.join(runtimesRoot, 'invalid-runtime', 'broken.json'), '{');

	const isolatedRegistry = runtimeRegistry({ repoRoot: tmpRoot });
	assert.deepEqual(Object.keys(isolatedRegistry), ['adjacent-runtime', 'preferred-runtime', 'valid-runtime']);
	assert.equal(isolatedRegistry['preferred-runtime'].name, 'Exact manifest');
} finally {
	fs.rmSync(tmpRoot, { recursive: true, force: true });
}

assert.deepEqual(dependencyEntries({ RUNTIME_PROVIDER: 'opencode', PROVIDER: 'opencode' }), []);

const configRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-runtime-config-'));
try {
	const config = buildConfig({
		GITHUB_WORKSPACE: configRoot,
		RUNNER_TEMP: path.join(configRoot, 'runner-temp'),
		WORKLOAD_ID: 'opencode-smoke',
		TARGET_REPO: 'Extra-Chill/example-target',
		RUNTIME: 'opencode',
		PROFILE: 'opencode-ci',
		RUNTIME_PROFILES: JSON.stringify({
			'opencode-ci': {
				id: 'opencode-ci',
				runtime_task_ability: 'opencode/run-task',
			},
		}),
		PROVIDER: 'opencode',
	});
	assert.equal(config.runtime_id, 'opencode');
	assert.equal(config.runtime_profile, 'opencode-ci');
	assert.equal(config.runtime_requirements.runtime_task_ability, 'opencode/run-task');
	assert.equal(config.runtime_bin, undefined);
} finally {
	fs.rmSync(configRoot, { recursive: true, force: true });
}

assert.throws(
	() => agentTaskRunnerSpec({ config: {} }),
	/backend is required/
);
assert.equal(
	agentTaskRunnerSpec({ backend: 'opencode', runtime: 'opencode', config: {} }).executor.backend,
	'opencode'
);
assert.equal(
	agentTaskRunnerSpec({ backend: 'opencode', runtime: 'opencode', config: {} }).executor.runtime,
	'opencode'
);

const runtimeProfile = {
	id: 'example-runtime-ci',
	runtime_task_ability: 'example/run-task',
};
assert.throws(
	() => runtimeAgentCiRunnerSpec({
		backend: 'opencode',
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}),
	/runtime is required/
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		runtime: 'opencode',
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.backend,
	'opencode'
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		runtime: 'opencode',
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.runtime,
	'opencode'
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		runtimeProvider: 'codebox',
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.runtime,
	'wp-codebox'
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		runtimeProvider: 'codebox',
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.backend,
	'codebox'
);

process.stdout.write('Generic agent runtime contract passed\n');
