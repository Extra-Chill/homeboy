'use strict';

require('../../runtime-agent-ci/tests/helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	normalizeRuntimeId,
	DEFAULT_RUNTIME_ID,
	resolveRuntimeProvider,
	runtimeRegistry,
} = require('../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');
const {
	agentTaskRunnerSpec,
} = require('../../agent-task-contracts');
const {
	runtimeAgentCiRunnerSpec,
} = require('../../runtime-agent-ci/provider-adapters');
const {
	buildConfig,
} = require('../../.github/scripts/runtime-agent-full-run/build-runner-config.cjs');
const {
	dependencyEntries,
} = require('../../runtime-agent-ci/lib/materialize-dependencies.cjs');

const repoRoot = path.join(__dirname, '..', '..');
const registry = runtimeRegistry({ repoRoot });

assert.equal(registry['wp-codebox'], undefined);
assert.equal(registry['local-shell'].id, 'local-shell');
assert.equal(registry['fake-runtime'], undefined);
assert.equal(registry['package'], undefined);
assert.equal(registry['homeboy-agent-task-core-contract'], undefined);
assert.equal(normalizeRuntimeId('codebox'), 'codebox');
assert.equal(DEFAULT_RUNTIME_ID, 'local-shell');
assert.equal(resolveRuntimeProvider(undefined, { repoRoot, registry }).id, 'local-shell');
assert.throws(() => resolveRuntimeProvider('codebox', { repoRoot, registry }), /Unsupported agent_runtime: codebox/);
assert.equal(normalizeRuntimeId('wp-codebox'), 'wp-codebox');
assert.throws(() => resolveRuntimeProvider('wp-codebox', { repoRoot, registry }), /Unsupported agent_runtime: wp-codebox/);
assert.equal(resolveRuntimeProvider('local-shell', { repoRoot, registry }).executor.backend, 'local-shell');

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
assert.equal(
	runtimeAgentCiRunnerSpec({
		backend: 'opencode',
		ability: 'example/run-task',
		runtime_profile: 'example-runtime-ci',
		runtime_profiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.runtime,
	undefined
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		runtime: 'opencode',
		ability: 'example/run-task',
		runtime_profile: 'example-runtime-ci',
		runtime_profiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.backend,
	'opencode'
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		runtime: 'opencode',
		ability: 'example/run-task',
		runtime_profile: 'example-runtime-ci',
		runtime_profiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.runtime,
	'opencode'
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		runtime: 'wp-codebox',
		ability: 'example/run-task',
		runtime_profile: 'example-runtime-ci',
		runtime_profiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.runtime,
	'wp-codebox'
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		runtime: 'wp-codebox',
		ability: 'example/run-task',
		runtime_profile: 'example-runtime-ci',
		runtime_profiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.backend,
	'wp-codebox'
);

process.stdout.write('Generic agent runtime contract passed\n');
