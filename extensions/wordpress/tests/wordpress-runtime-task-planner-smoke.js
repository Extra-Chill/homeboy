'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const {
	wordpressRuntimeTaskPlan,
	wordpressRuntimeTaskRequest,
	wordpressRuntimeTaskRunnerSpec,
} = require('../lib/wordpress-runtime-task-planner');
const {
	WORDPRESS_CRUD_OPERATION_RESULT_SCHEMA,
} = require('../lib/wordpress-generic-fuzz-primitives');
const {
	genericAgentTaskRequest,
} = require('../../agent-task-contracts');

const contract = JSON.parse(fs.readFileSync(path.join(
	__dirname,
	'..',
	'..',
	'agent-runtimes',
	'fixtures',
	'homeboy-agent-task-core-contract.json'
), 'utf8'));

const plan = wordpressRuntimeTaskPlan({
	planId: 'runtime-task-plan-smoke',
	ability: 'datamachine/run-runtime-task',
	abilityInput: { operation: 'extract' },
	dlaUrl: 'dla://runtime/import/demo-site',
	provider: 'codex',
	model: 'openai/gpt-5.5',
	runtimeProfile: 'wp-codebox',
	runtimeProfiles: {
		'wp-codebox': {
			backend: 'wp-codebox',
			runtime: 'wp-codebox',
		},
	},
	concurrency: 2,
	timeoutSeconds: 900,
	expectedArtifacts: ['runtime-task-result'],
	providerPluginPaths: ['/workspace/components/ai-provider-for-openai'],
	secretEnv: ['AI_PROVIDER_OPENAI_CODEX_REFRESH_TOKEN'],
	fanout: [
		{ taskId: 'runtime-task-plan-smoke-import', input: { site: 'import' }, matrix: { scenario: 'import' } },
		{ taskId: 'runtime-task-plan-smoke-verify', input: { site: 'verify' }, matrix: { scenario: 'verify' } },
	],
});

assert.equal(plan.schema, contract.schemas.plan);
assert.equal(plan.plan_id, 'runtime-task-plan-smoke');
assert.equal(plan.options.concurrency, 2);
assert.equal(plan.tasks.length, 2);
for (const task of plan.tasks) {
	assert.equal(task.schema, contract.schemas.request);
	assert.equal(task.executor.backend, 'wp-codebox');
	assert.equal(task.executor.runtime, 'wp-codebox');
	assert.equal(task.executor.config.runtime_id, 'wp-codebox');
	assert.equal(task.executor.config.runtime_task.ability, 'datamachine/run-runtime-task');
	assert.equal(task.executor.config.runtime_task.input.operation, 'extract');
	assert.equal(task.executor.config.runtime_task.input.dla_url, 'dla://runtime/import/demo-site');
	assert.equal(task.executor.config.runtime_task.input.provider, 'codex');
	assert.equal(task.executor.config.runtime_task.input.model, 'openai/gpt-5.5');
	assert.deepEqual(task.expected_artifacts, ['runtime-task-result']);
	assert.equal(task.limits.task_timeout_seconds, 900);
	assert.equal(task.parent_plan_id, 'runtime-task-plan-smoke');
	assert.deepEqual(task.policy, { read: 'sandbox', write: 'sandbox', apply: 'review' });
}
assert.deepEqual(plan.tasks.map((task) => task.metadata.fanout.scenario), ['import', 'verify']);
assert(!JSON.stringify(plan).includes('/Users/'), 'planner must not inject local user paths');

const genericBackendPlan = wordpressRuntimeTaskPlan({
	planId: 'runtime-task-generic-backend-smoke',
	ability: 'datamachine/run-runtime-task',
	runtimeBackend: 'opencode',
	runtimeId: 'opencode-local',
});
assert.equal(genericBackendPlan.metadata.backend, 'opencode');
assert.equal(genericBackendPlan.metadata.runtime, 'opencode-local');
assert.equal(genericBackendPlan.tasks[0].executor.backend, 'opencode');
assert.equal(genericBackendPlan.tasks[0].executor.runtime, 'opencode-local');
assert.equal(genericBackendPlan.tasks[0].executor.config.runtime_id, 'opencode-local');

const request = wordpressRuntimeTaskRequest({
	taskId: 'single-runtime-task-smoke',
	ability: 'example/materialize-artifact',
	abilityInput: { slug: 'example' },
	backend: 'wp-codebox',
	runtime: 'wp-codebox',
});
assert.equal(request.schema, contract.schemas.request);
assert.equal(request.task_id, 'single-runtime-task-smoke');
assert.equal(request.instructions, 'Run WordPress runtime ability example/materialize-artifact and return the declared artifacts.');
assert.deepEqual(request.inputs.ability_input, { slug: 'example' });

assert.deepEqual(
	request,
	genericAgentTaskRequest({
		schema: contract.schemas.request,
		task_id: 'single-runtime-task-smoke',
		parent_plan_id: undefined,
		goal: 'Run WordPress runtime ability example/materialize-artifact and return the declared artifacts.',
		instructions: 'Run WordPress runtime ability example/materialize-artifact and return the declared artifacts.',
		inputs: {
			ability: 'example/materialize-artifact',
			ability_input: { slug: 'example' },
		},
		source_refs: [],
		policy: { read: 'sandbox', write: 'sandbox', apply: 'review' },
		metadata: {},
		runnerSpec: wordpressRuntimeTaskRunnerSpec({
			taskId: 'single-runtime-task-smoke',
			ability: 'example/materialize-artifact',
			abilityInput: { slug: 'example' },
			backend: 'wp-codebox',
			runtime: 'wp-codebox',
		}),
	})
);

const crudRequest = wordpressRuntimeTaskRequest({
	taskId: 'crud-runtime-task-smoke',
	ability: 'wordpress/execute-crud-operation',
	abilityInput: {
		operation: {
			action: 'create',
			resource_type: 'post',
			capability_context: { required: ['edit_posts'] },
			rollback_policy: { strategy: 'delete-created' },
		},
	},
	backend: 'wp-codebox',
	runtime: 'wp-codebox',
});
assert.deepEqual(crudRequest.inputs.ability_input.expected_result_contracts, [WORDPRESS_CRUD_OPERATION_RESULT_SCHEMA]);
assert.deepEqual(crudRequest.expected_artifacts, ['wordpress-crud-operation-result']);
assert.deepEqual(crudRequest.metadata.expected_result_contracts, [WORDPRESS_CRUD_OPERATION_RESULT_SCHEMA]);
assert.deepEqual(crudRequest.executor.config.expected_result_contracts, [WORDPRESS_CRUD_OPERATION_RESULT_SCHEMA]);
assert.deepEqual(crudRequest.executor.config.runtime_task.input.expected_result_contracts, [WORDPRESS_CRUD_OPERATION_RESULT_SCHEMA]);

const explicitArtifactCrudRequest = wordpressRuntimeTaskRequest({
	taskId: 'crud-runtime-task-explicit-artifact-smoke',
	ability: 'wordpress/execute-crud-operation',
	abilityInput: { operation: { action: 'delete', resource_type: 'post' } },
	expectedArtifacts: ['runtime-log'],
	backend: 'wp-codebox',
	runtime: 'wp-codebox',
});
assert.deepEqual(explicitArtifactCrudRequest.expected_artifacts, ['runtime-log', 'wordpress-crud-operation-result']);

const script = path.join(__dirname, '..', 'scripts', 'agent', 'homeboy-wordpress-runtime-task-plan.cjs');
const result = spawnSync(process.execPath, [
	script,
	'--plan-id', 'cli-runtime-task-plan-smoke',
	'--ability', 'example/validate-artifact',
	'--backend', 'wp-codebox',
	'--runtime-id', 'wp-codebox',
	'--ability-input', '{"artifact":"report.json"}',
	'--dla-url', 'https://dla.example/export/123',
	'--expected-artifact', 'validation-report',
	'--timeout-seconds', '60',
], { encoding: 'utf8' });
assert.equal(result.status, 0, result.stderr || result.stdout);
const cliPlan = JSON.parse(result.stdout);
assert.equal(cliPlan.schema, contract.schemas.plan);
assert.equal(cliPlan.tasks[0].schema, contract.schemas.request);
assert.equal(cliPlan.tasks[0].executor.runtime, 'wp-codebox');
assert.equal(cliPlan.tasks[0].executor.config.runtime_id, 'wp-codebox');
assert.equal(cliPlan.tasks[0].executor.config.runtime_task.input.dla_url, 'https://dla.example/export/123');
assert.deepEqual(cliPlan.tasks[0].expected_artifacts, ['validation-report']);
assert.equal(cliPlan.tasks[0].limits.task_timeout_seconds, 60);

const installCopy = fs.mkdtempSync(path.join(fs.realpathSync(os.tmpdir()), 'homeboy-wordpress-extension-install-'));
try {
	const installedExtension = path.join(installCopy, 'wordpress');
	fs.cpSync(path.join(__dirname, '..'), installedExtension, {
		recursive: true,
		filter: (source) => !source.split(path.sep).includes('node_modules'),
	});
	fs.cpSync(path.join(__dirname, '..', '..', 'agent-task-contracts'), path.join(installCopy, 'agent-task-contracts'), {
		recursive: true,
	});
	const installedRequire = spawnSync(process.execPath, [
		'-e',
		"require(process.argv[1]); process.stdout.write('installed planner require passed\\n');",
		path.join(installedExtension, 'lib', 'wordpress-runtime-task-planner.js'),
	], { encoding: 'utf8' });
	assert.equal(installedRequire.status, 0, installedRequire.stderr || installedRequire.stdout);
} finally {
	fs.rmSync(installCopy, { recursive: true, force: true });
}

process.stdout.write('WordPress runtime task planner smoke passed\n');
