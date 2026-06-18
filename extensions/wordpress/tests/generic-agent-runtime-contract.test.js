'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
	resolveRuntimeProvider,
	runtimeRegistry,
} = require('../../agent-runtimes/lib/runtime-provider-resolver.cjs');
const {
	agentTaskRunnerSpec,
} = require('../../agent-runtimes/lib/agent-task-runner-contract');
const {
	runtimeAgentCiRunnerSpec,
} = require('../../runtime-agent-ci');

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
	fs.mkdirSync(path.join(runtimesRoot, 'invalid-runtime'), { recursive: true });
	fs.writeFileSync(path.join(runtimesRoot, 'valid-runtime', 'valid-runtime.json'), JSON.stringify({
		schema: 'homeboy/agent-runtime-manifest/v1',
		id: 'valid-runtime',
		agent_task_executors: [],
	}));
	fs.writeFileSync(path.join(runtimesRoot, 'invalid-runtime', 'invalid-runtime.json'), JSON.stringify({
		schema: 'homeboy/agent-runtime-manifest/v1',
		name: 'missing id and executors',
	}));
	fs.writeFileSync(path.join(runtimesRoot, 'invalid-runtime', 'broken.json'), '{');

	const isolatedRegistry = runtimeRegistry({ repoRoot: tmpRoot });
	assert.deepEqual(Object.keys(isolatedRegistry), ['valid-runtime']);
} finally {
	fs.rmSync(tmpRoot, { recursive: true, force: true });
}

assert.throws(
	() => agentTaskRunnerSpec({ config: {} }),
	/backend is required/
);
assert.equal(
	agentTaskRunnerSpec({ backend: 'fake-runtime', config: {} }).executor.backend,
	'fake-runtime'
);

const runtimeProfile = {
	id: 'example-runtime-ci',
	runtime_task_ability: 'example/run-task',
};
assert.throws(
	() => runtimeAgentCiRunnerSpec({
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}),
	/backend is required/
);
assert.equal(
	runtimeAgentCiRunnerSpec({
		backend: 'fake-runtime',
		ability: 'example/run-task',
		runtimeProfile: 'example-runtime-ci',
		runtimeProfiles: { 'example-runtime-ci': runtimeProfile },
	}).executor.backend,
	'fake-runtime'
);

process.stdout.write('Generic agent runtime contract passed\n');
