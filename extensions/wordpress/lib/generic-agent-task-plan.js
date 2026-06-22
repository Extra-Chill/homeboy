'use strict';

const {
	agentTaskRequestFromRunnerSpec,
	agentTaskRunnerSpec,
} = require('./agent-task-runner-contract');

const GENERIC_AGENT_TASK_PLAN_SCHEMA = 'homeboy/agent-task-plan/v1';
const GENERIC_AGENT_TASK_REQUEST_SCHEMA = 'homeboy/agent-task-request/v1';
const GENERIC_RUNTIME_EXECUTION_DESCRIPTOR_SCHEMA = 'homeboy/runtime-execution/v1';

function genericAgentTaskRunnerSpec(options = {}) {
	const config = options.config || options.executorConfig || options.executor_config;
	return agentTaskRunnerSpec({
		backend: options.backend,
		runtime: options.runtime || options.runtimeId || options.runtime_id,
		config,
		secret_env: normalizeArray(options.secretEnv || options.secret_env),
		task_timeout_seconds: options.taskTimeoutSeconds || options.task_timeout_seconds,
		artifact_declarations: options.artifactDeclarations || options.artifact_declarations,
		limits: options.limits,
		expected_artifacts: options.expectedArtifacts || options.expected_artifacts,
	});
}

function genericAgentTaskRequest(options = {}) {
	const taskId = requiredString(options.taskId || options.task_id, 'taskId');
	const runnerRequest = agentTaskRequestFromRunnerSpec({
		runnerSpec: options.runnerSpec || options.runner_spec || genericAgentTaskRunnerSpec(options),
	});
	const sourceRefs = options.sourceRefs || options.source_refs;
	return stripUndefined({
		schema: options.schema || GENERIC_AGENT_TASK_REQUEST_SCHEMA,
		task_id: taskId,
		group_key: options.groupKey || options.group_key,
		parent_plan_id: options.parentPlanId || options.parent_plan_id,
		cwd: options.cwd,
		repo: options.repo,
		workspace: options.workspace,
		executor: runnerRequest.executor,
		instructions: options.instructions,
		inputs: options.inputs,
		source_refs: sourceRefs === undefined ? undefined : normalizeArray(sourceRefs),
		policy: options.policy,
		limits: runnerRequest.limits,
		artifact_declarations: options.includeArtifactDeclarations === false ? undefined : runnerRequest.artifact_declarations,
		expected_artifacts: runnerRequest.expected_artifacts,
		metadata: options.metadata,
	});
}

function genericAgentTaskPlan(options = {}) {
	const planId = requiredString(options.planId || options.plan_id, 'planId');
	const tasks = normalizeArray(options.tasks);
	if (tasks.length === 0) {
		throw new Error('tasks must contain at least one task request.');
	}
	return stripUndefined({
		schema: options.schema || GENERIC_AGENT_TASK_PLAN_SCHEMA,
		plan_id: planId,
		tasks,
		options: options.options,
		metadata: options.metadata,
	});
}

function normalizeArray(value) {
	return Array.isArray(value) ? value.filter(Boolean) : [];
}

function requiredString(value, name) {
	if (typeof value !== 'string' || value.trim() === '') {
		throw new Error(`${name} is required.`);
	}
	return value;
}

function stripUndefined(value) {
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return value;
	}
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
	GENERIC_AGENT_TASK_PLAN_SCHEMA,
	GENERIC_AGENT_TASK_REQUEST_SCHEMA,
	GENERIC_RUNTIME_EXECUTION_DESCRIPTOR_SCHEMA,
	genericAgentTaskPlan,
	genericAgentTaskRequest,
	genericAgentTaskRunnerSpec,
};
