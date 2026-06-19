'use strict';

/**
 * External dependencies
 */
const crypto = require('node:crypto');

/**
 * Internal dependencies
 */
const {
	AGENT_TASK_REQUEST_SCHEMA,
} = require('../../runtime-agent-ci/lib/agent-task-provider-contract');
const {
	agentTaskRequestFromRunnerSpec,
	agentTaskRunnerSpec,
} = require('../../runtime-agent-ci/lib/agent-task-runner-contract');

const WORDPRESS_RUNTIME_TASK_PLAN_SCHEMA = 'homeboy/agent-task-plan/v1';
const WORDPRESS_RUNTIME_TASK_COMPATIBILITY_BACKEND = 'codebox';
const WORDPRESS_RUNTIME_TASK_COMPATIBILITY_RUNTIME = 'wp-codebox';
const WORDPRESS_RUNTIME_TASK_DEFAULT_BACKEND = WORDPRESS_RUNTIME_TASK_COMPATIBILITY_BACKEND;
const WORDPRESS_RUNTIME_TASK_DEFAULT_RUNTIME = WORDPRESS_RUNTIME_TASK_COMPATIBILITY_RUNTIME;
const WORDPRESS_RUNTIME_TASK_DEFAULT_POLICY = {
	read: 'sandbox',
	write: 'sandbox',
	apply: 'review',
};

function wordpressRuntimeTaskPlan(options = {}) {
	const planId = requiredString(options.planId || options.plan_id, 'planId');
	const backend = wordpressRuntimeTaskBackend(options);
	const runtime = wordpressRuntimeTaskRuntime(options);
	const taskOptions = normalizeTaskOptions(options);
	const tasks = taskOptions.map((taskOption, index) => wordpressRuntimeTaskRequest({
		...options,
		...taskOption,
		backend: wordpressRuntimeTaskBackend({ ...options, ...taskOption }, backend),
		runtime: wordpressRuntimeTaskRuntime({ ...options, ...taskOption }, runtime),
		planId,
		parentPlanId: planId,
		taskId: taskOption.taskId || taskOption.task_id || taskIdForPlan(planId, index, taskOption),
		fanout: taskOption.fanout || taskOption.matrix || taskOption.metadata?.fanout,
	}));

	return stripUndefined({
		schema: WORDPRESS_RUNTIME_TASK_PLAN_SCHEMA,
		plan_id: planId,
		tasks,
		options: stripUndefined({
			concurrency: numberOrUndefined(options.concurrency),
			fail_fast: options.failFast ?? options.fail_fast,
		}),
		metadata: stripUndefined({
			...(options.metadata || {}),
			planner: 'homeboy-extension-wordpress/wordpress-runtime-task-planner',
			runtime,
			backend,
		}),
	});
}

function wordpressRuntimeTaskRequest(options = {}) {
	const taskId = requiredString(options.taskId || options.task_id, 'taskId');
	const ability = requiredString(options.ability, 'ability');
	const abilityInput = runtimeAbilityInput(options);
	const runnerRequest = agentTaskRequestFromRunnerSpec({
		runnerSpec: wordpressRuntimeTaskRunnerSpec({
			...options,
			ability,
			abilityInput,
		}),
	});

	return stripUndefined({
		schema: AGENT_TASK_REQUEST_SCHEMA,
		task_id: taskId,
		group_key: options.groupKey || options.group_key,
		parent_plan_id: options.parentPlanId || options.parent_plan_id || options.planId || options.plan_id,
		cwd: options.cwd,
		repo: options.repo,
		workspace: options.workspace,
		executor: runnerRequest.executor,
		instructions: options.instructions || instructionsForAbility(ability),
		inputs: stripUndefined({
			...(options.inputs || {}),
			ability,
			ability_input: abilityInput,
		}),
		source_refs: normalizeArray(options.sourceRefs || options.source_refs),
		policy: options.policy || WORDPRESS_RUNTIME_TASK_DEFAULT_POLICY,
		limits: runnerRequest.limits,
		expected_artifacts: runnerRequest.expected_artifacts,
		metadata: stripUndefined({
			...(options.metadata || {}),
			fanout: options.fanout,
		}),
	});
}

function wordpressRuntimeTaskRunnerSpec(options = {}) {
	const config = wordpressRuntimeTaskExecutorConfig(options);
	return agentTaskRunnerSpec({
		backend: wordpressRuntimeTaskBackend(options),
		runtime: wordpressRuntimeTaskRuntime(options),
		config,
		secret_env: normalizeArray(options.secretEnv || options.secret_env),
		task_timeout_seconds: numberOrUndefined(options.taskTimeoutSeconds || options.task_timeout_seconds || options.timeoutSeconds || options.timeout_seconds),
		limits: options.limits,
		expected_artifacts: options.expectedArtifacts || options.expected_artifacts,
	});
}

function wordpressRuntimeTaskBackend(options = {}, fallback = WORDPRESS_RUNTIME_TASK_COMPATIBILITY_BACKEND) {
	return options.backend || options.runtimeBackend || options.runtime_backend || options.agentRuntimeBackend || options.agent_runtime_backend || fallback;
}

function wordpressRuntimeTaskRuntime(options = {}, fallback = WORDPRESS_RUNTIME_TASK_COMPATIBILITY_RUNTIME) {
	return options.runtime || options.runtimeId || options.runtime_id || options.agentRuntime || options.agent_runtime || fallback;
}

function wordpressRuntimeTaskExecutorConfig(options = {}) {
	const runtime = wordpressRuntimeTaskRuntime(options);
	return stripUndefined({
		...(options.config || {}),
		provider: options.provider,
		model: options.model,
		runtime,
		runtime_id: options.runtimeId || options.runtime_id || runtime,
		runtime_bin: options.runtimeBin || options.runtime_bin,
		provider_plugin_paths: options.providerPluginPaths || options.provider_plugin_paths,
		homeboy_extensions: options.homeboyExtensions || options.homeboy_extensions,
		runtime_component_paths: options.runtimeComponentPaths || options.runtime_component_paths,
		component_contracts: options.componentContracts || options.component_contracts,
		ability_tools: options.abilityTools || options.ability_tools,
		structured_artifacts: options.structuredArtifacts || options.structured_artifacts,
		runtime_env: options.runtimeEnv || options.runtime_env,
		runtime_config_mounts: options.runtimeConfigMounts || options.runtime_config_mounts,
		runtime_state_mounts: options.runtimeStateMounts || options.runtime_state_mounts,
		max_turns: options.maxTurns || options.max_turns,
		task_timeout_seconds: numberOrUndefined(options.taskTimeoutSeconds || options.task_timeout_seconds || options.timeoutSeconds || options.timeout_seconds),
		runtime_task: {
			ability: options.ability,
			input: options.abilityInput || options.ability_input || {},
		},
	});
}

function runtimeAbilityInput(options = {}) {
	return stripUndefined({
		...(options.abilityInput || options.ability_input || {}),
		...(options.dlaUrl || options.dla_url ? { dla_url: options.dlaUrl || options.dla_url } : {}),
		...(options.provider && !hasOwn(options.abilityInput || options.ability_input || {}, 'provider') ? { provider: options.provider } : {}),
		...(options.model && !hasOwn(options.abilityInput || options.ability_input || {}, 'model') ? { model: options.model } : {}),
	});
}

function normalizeTaskOptions(options = {}) {
	const tasks = normalizeArray(options.tasks);
	if (tasks.length > 0) {
		return tasks;
	}
	const fanout = normalizeArray(options.fanout);
	if (fanout.length > 0) {
		return fanout.map((entry) => ({
			...entry,
			abilityInput: {
				...objectOrEmpty(options.abilityInput || options.ability_input),
				...objectOrEmpty(entry.abilityInput || entry.ability_input || entry.input),
			},
			fanout: entry.fanout || entry.matrix || entry,
		}));
	}
	return [{}];
}

function taskIdForPlan(planId, index, taskOption = {}) {
	if (index === 0 && !taskOption.fanout && !taskOption.matrix) {
		return `${planId}-task`;
	}
	const fingerprint = crypto.createHash('sha256').update(JSON.stringify(taskOption)).digest('hex').slice(0, 10);
	return `${planId}-${index + 1}-${fingerprint}`;
}

function instructionsForAbility(ability) {
	return `Run WordPress runtime ability ${ability} and return the declared artifacts.`;
}

function normalizeArray(value) {
	return Array.isArray(value) ? value.filter(Boolean) : [];
}

function objectOrEmpty(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : {};
}

function requiredString(value, name) {
	if (typeof value !== 'string' || value.trim() === '') {
		throw new Error(`${name} is required.`);
	}
	return value;
}

function hasOwn(value, key) {
	return Boolean(value && typeof value === 'object' && Object.prototype.hasOwnProperty.call(value, key));
}

function numberOrUndefined(value) {
	if (value === undefined || value === null || value === '') {
		return undefined;
	}
	const parsed = Number(value);
	return Number.isFinite(parsed) ? parsed : undefined;
}

function stripUndefined(value) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return value;
	}
	return Object.fromEntries(
		Object.entries(value).filter(([, entry]) => entry !== undefined)
	);
}

module.exports = {
	WORDPRESS_RUNTIME_TASK_DEFAULT_BACKEND,
	WORDPRESS_RUNTIME_TASK_COMPATIBILITY_BACKEND,
	WORDPRESS_RUNTIME_TASK_COMPATIBILITY_RUNTIME,
	WORDPRESS_RUNTIME_TASK_DEFAULT_POLICY,
	WORDPRESS_RUNTIME_TASK_DEFAULT_RUNTIME,
	WORDPRESS_RUNTIME_TASK_PLAN_SCHEMA,
	wordpressRuntimeTaskBackend,
	wordpressRuntimeTaskExecutorConfig,
	wordpressRuntimeTaskPlan,
	wordpressRuntimeTaskRequest,
	wordpressRuntimeTaskRuntime,
	wordpressRuntimeTaskRunnerSpec,
};
