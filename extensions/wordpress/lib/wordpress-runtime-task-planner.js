'use strict';

/**
 * External dependencies
 */
const crypto = require('node:crypto');

/**
 * Internal dependencies
 */
const {
	GENERIC_AGENT_TASK_PLAN_SCHEMA,
	GENERIC_AGENT_TASK_REQUEST_SCHEMA,
	genericAgentTaskPlan,
	genericAgentTaskRequest,
	genericAgentTaskRunnerSpec,
} = require('./generic-agent-task-plan');

const WORDPRESS_RUNTIME_TASK_PLAN_SCHEMA = GENERIC_AGENT_TASK_PLAN_SCHEMA;
const WORDPRESS_RUNTIME_TASK_REQUEST_SCHEMA = GENERIC_AGENT_TASK_REQUEST_SCHEMA;
const WORDPRESS_RUNTIME_TASK_COMPATIBILITY_BACKEND = undefined;
const WORDPRESS_RUNTIME_TASK_COMPATIBILITY_RUNTIME = undefined;
const WORDPRESS_RUNTIME_TASK_DEFAULT_BACKEND = undefined;
const WORDPRESS_RUNTIME_TASK_DEFAULT_RUNTIME = undefined;
const WORDPRESS_RUNTIME_TASK_DEFAULT_POLICY = {
	read: 'sandbox',
	write: 'sandbox',
	apply: 'review',
};

function wordpressRuntimeTaskPlan(options = {}) {
	const planId = requiredString(options.planId || options.plan_id, 'planId');
	const runtimeProfile = wordpressRuntimeTaskProfile(options);
	const backend = wordpressRuntimeTaskBackend(options, undefined, runtimeProfile);
	const runtime = wordpressRuntimeTaskRuntime(options, undefined, runtimeProfile);
	const taskOptions = normalizeTaskOptions(options);
	const tasks = taskOptions.map((taskOption, index) => wordpressRuntimeTaskRequest({
		...options,
		...taskOption,
		backend: wordpressRuntimeTaskBackend({ ...options, ...taskOption }, backend, runtimeProfile),
		runtime: wordpressRuntimeTaskRuntime({ ...options, ...taskOption }, runtime, runtimeProfile),
		planId,
		parentPlanId: planId,
		taskId: taskOption.taskId || taskOption.task_id || taskIdForPlan(planId, index, taskOption),
		fanout: taskOption.fanout || taskOption.matrix || taskOption.metadata?.fanout,
	}));

	return genericAgentTaskPlan({
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
			runtime_profile: runtimeProfile.id,
			runtime,
			backend,
		}),
	});
}

function wordpressRuntimeTaskRequest(options = {}) {
	const taskId = requiredString(options.taskId || options.task_id, 'taskId');
	const ability = requiredString(options.ability, 'ability');
	const abilityInput = runtimeAbilityInput(options);
	const runnerSpec = wordpressRuntimeTaskRunnerSpec({
		...options,
		ability,
		abilityInput,
	});

	return genericAgentTaskRequest({
		schema: WORDPRESS_RUNTIME_TASK_REQUEST_SCHEMA,
		task_id: taskId,
		group_key: options.groupKey || options.group_key,
		parent_plan_id: options.parentPlanId || options.parent_plan_id || options.planId || options.plan_id,
		cwd: options.cwd,
		repo: options.repo,
		workspace: options.workspace,
		goal: options.goal || options.instructions || instructionsForAbility(ability),
		instructions: options.instructions || instructionsForAbility(ability),
		inputs: stripUndefined({
			...(options.inputs || {}),
			ability,
			ability_input: abilityInput,
		}),
		source_refs: normalizeArray(options.sourceRefs || options.source_refs),
		policy: options.policy || WORDPRESS_RUNTIME_TASK_DEFAULT_POLICY,
		metadata: stripUndefined({
			...(options.metadata || {}),
			fanout: options.fanout,
		}),
		includeArtifactDeclarations: options.includeArtifactDeclarations === false
			? false
			: normalizeArray(options.artifactDeclarations || options.artifact_declarations).length > 0,
		runnerSpec,
	});
}

function wordpressRuntimeTaskRunnerSpec(options = {}) {
	const config = wordpressRuntimeTaskExecutorConfig(options);
	const runtimeProfile = wordpressRuntimeTaskProfile(options);
	return genericAgentTaskRunnerSpec({
		backend: wordpressRuntimeTaskBackend(options, undefined, runtimeProfile),
		runtime: wordpressRuntimeTaskRuntime(options, undefined, runtimeProfile),
		config,
		secret_env: normalizeArray(options.secretEnv || options.secret_env),
		task_timeout_seconds: numberOrUndefined(options.taskTimeoutSeconds || options.task_timeout_seconds || options.timeoutSeconds || options.timeout_seconds),
		limits: options.limits,
		artifact_declarations: options.artifactDeclarations || options.artifact_declarations,
		expected_artifacts: options.expectedArtifacts || options.expected_artifacts,
	});
}

function wordpressRuntimeTaskBackend(options = {}, fallback, runtimeProfile = wordpressRuntimeTaskProfile(options)) {
	return options.backend
		|| options.runtimeBackend
		|| options.runtime_backend
		|| options.agentRuntimeBackend
		|| options.agent_runtime_backend
		|| runtimeProfile.backend
		|| runtimeProfile.runtime_backend
		|| runtimeProfile.executor_backend
		|| runtimeProfile.executor?.backend
		|| fallback;
}

function wordpressRuntimeTaskRuntime(options = {}, fallback, runtimeProfile = wordpressRuntimeTaskProfile(options)) {
	return options.runtime
		|| options.runtimeId
		|| options.runtime_id
		|| options.agentRuntime
		|| options.agent_runtime
		|| runtimeProfile.runtime
		|| runtimeProfile.runtime_id
		|| runtimeProfile.id
		|| fallback;
}

function wordpressRuntimeTaskExecutorConfig(options = {}) {
	const runtimeProfile = wordpressRuntimeTaskProfile(options);
	const runtime = wordpressRuntimeTaskRuntime(options, undefined, runtimeProfile);
	return stripUndefined({
		...(options.config || {}),
		provider: options.provider,
		model: options.model,
		runtime,
		runtime_id: options.runtimeId || options.runtime_id || runtime,
		runtime_profile: runtimeProfile.id,
		runtime_profiles: options.runtimeProfiles || options.runtime_profiles,
		runtime_bin: options.runtimeBin || options.runtime_bin,
		provider_plugin_paths: options.providerPluginPaths || options.provider_plugin_paths,
		homeboy_extensions: options.homeboyExtensions || options.homeboy_extensions,
		runtime_component_paths: options.runtimeComponentPaths || options.runtime_component_paths,
		component_contracts: options.componentContracts || options.component_contracts,
		runtime_requirements: options.runtimeRequirements || options.runtime_requirements,
		ability_tools: options.abilityTools || options.ability_tools,
		structured_artifacts: options.structuredArtifacts || options.structured_artifacts,
		runtime_env: options.runtimeEnv || options.runtime_env,
		runtime_mounts: options.runtimeMounts || options.runtime_mounts,
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

function wordpressRuntimeTaskProfile(options = {}) {
	const profile = options.runtimeProfile || options.runtime_profile || options.profile;
	if (profile && typeof profile === 'object' && !Array.isArray(profile)) {
		return profile;
	}
	const profileId = typeof profile === 'string' && profile.trim()
		? profile.trim()
		: options.runtimeProfileId || options.runtime_profile_id;
	const profiles = options.runtimeProfiles || options.runtime_profiles || options.config?.runtime_profiles || options.config?.runtimeProfiles || {};
	if (profileId && profiles[profileId] && typeof profiles[profileId] === 'object' && !Array.isArray(profiles[profileId])) {
		return { id: profileId, ...profiles[profileId] };
	}
	return profileId ? { id: profileId } : {};
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
	WORDPRESS_RUNTIME_TASK_REQUEST_SCHEMA,
	wordpressRuntimeTaskProfile,
	wordpressRuntimeTaskBackend,
	wordpressRuntimeTaskExecutorConfig,
	wordpressRuntimeTaskPlan,
	wordpressRuntimeTaskRequest,
	wordpressRuntimeTaskRuntime,
	wordpressRuntimeTaskRunnerSpec,
};
