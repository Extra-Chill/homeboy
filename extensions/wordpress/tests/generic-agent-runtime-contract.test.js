'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
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
assert.equal(registry['fake-runtime'].id, 'fake-runtime');
assert.equal(registry['package'], undefined);
assert.equal(registry['homeboy-agent-task-core-contract'], undefined);

const fakeRuntime = resolveRuntimeProvider('fake-runtime', { repoRoot, registry });
assert.equal(fakeRuntime.id, 'fake-runtime');
assert.equal(fakeRuntime.executor.backend, 'fake-runtime');
assert.equal(fakeRuntime.executor.path, path.join(repoRoot, 'agent-runtimes/fake-runtime/scripts/agent/fake-agent-task-executor.cjs'));

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

assert.deepEqual(dependencyEntries({ RUNTIME_PROVIDER: 'fake-runtime', PROVIDER: 'fake-runtime' }), []);

const configRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-runtime-config-'));
try {
	const config = buildConfig({
		GITHUB_WORKSPACE: configRoot,
		RUNNER_TEMP: path.join(configRoot, 'runner-temp'),
		WORKLOAD_ID: 'fake-runtime-smoke',
		TARGET_REPO: 'Extra-Chill/example-target',
		RUNTIME_PROVIDER: 'fake-runtime',
		RUNTIME_PROFILE: 'fake-runtime-ci',
		RUNTIME_PROFILES: JSON.stringify({
			'fake-runtime-ci': {
				id: 'fake-runtime-ci',
				runtime_task_ability: 'fake-runtime/run-task',
			},
		}),
		PROVIDER: 'fake-runtime',
	});
	assert.equal(config.runtime_id, 'fake-runtime');
	assert.equal(config.runtime_profile, 'fake-runtime-ci');
	assert.equal(config.runtime_requirements.runtime_task_ability, 'fake-runtime/run-task');
	assert.equal(config.runtime_bin, undefined);
} finally {
	fs.rmSync(configRoot, { recursive: true, force: true });
}

assert.throws(
	() => agentTaskRunnerSpec({ config: {} }),
	/backend is required/
);
assert.equal(
	agentTaskRunnerSpec({ backend: 'fake-runtime', runtime: 'fake-runtime', config: {} }).executor.backend,
	'fake-runtime'
);
assert.equal(
	agentTaskRunnerSpec({ backend: 'fake-runtime', runtime: 'fake-runtime', config: {} }).executor.runtime,
	'fake-runtime'
);

const runtimeProfile = {
	id: 'example-runtime-ci',
	runtime_task_ability: 'example/run-task',
};
assert.throws(
	() => runtimeAgentCiRunnerSpec({
		backend: 'fake-runtime',
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}),
	/runtime is required/
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		backend: 'fake-runtime',
		runtime: 'fake-runtime',
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.backend,
	'fake-runtime'
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		backend: 'fake-runtime',
		runtime: 'fake-runtime',
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.runtime,
	'fake-runtime'
);

process.stdout.write('Generic agent runtime contract passed\n');
