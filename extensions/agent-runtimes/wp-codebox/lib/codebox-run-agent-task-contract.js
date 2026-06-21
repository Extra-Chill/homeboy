'use strict';

const {
  WP_CODEBOX_TASK_REQUEST_SCHEMA,
} = require('./wp-codebox-adapter-contract');
const {
  runtimeContractSchemas,
} = require('./wp-codebox-runtime-contract-source');

const RUNTIME_CONTRACT_SCHEMAS = runtimeContractSchemas();

const WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.agentTask.runRequest;
const WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.agentTask.runResult;
const WP_CODEBOX_AGENT_TASK_RUN_RESPONSE_SCHEMA = RUNTIME_CONTRACT_SCHEMAS.agentTask.legacyRunResponse;
const WP_CODEBOX_RUN_AGENT_TASK_RESULT_SCHEMA = WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA;
const WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND = 'run-agent-task';
const WP_CODEBOX_LEGACY_AGENT_TASK_RUN_CLI_COMMAND = 'agent-task-run';

function codeboxRunAgentTaskRequestFromTaskInput(taskInput, options = {}) {
  if (!taskInput || taskInput.schema !== WP_CODEBOX_TASK_REQUEST_SCHEMA) {
    throw new Error(`Codebox run-agent-task adapter requires ${WP_CODEBOX_TASK_REQUEST_SCHEMA} task input.`);
  }
  return withoutUndefinedValues({
    schema: WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA,
    version: 1,
    task_id: firstValue(options.taskId, options.task_id, taskInput.sandbox_session_id, taskInput.context?.agent_task_id),
    task_input: taskInput,
    artifacts_path: firstValue(options.artifactsPath, options.artifacts_path, taskInput.artifacts_path),
    callback_data: firstValue(options.callbackData, options.callback_data, taskInput.callback_data),
    compatibility: {
      legacy_input_schema: WP_CODEBOX_TASK_REQUEST_SCHEMA,
      legacy_cli_command: WP_CODEBOX_LEGACY_AGENT_TASK_RUN_CLI_COMMAND,
    },
  });
}

function codeboxRunAgentTaskInvocation(options = {}) {
  const useStableRunAgentTask = Boolean(options.useStableRunAgentTask || options.use_stable_run_agent_task);
  const input = useStableRunAgentTask
    ? codeboxRunAgentTaskRequestFromTaskInput(options.taskInput, options)
    : options.taskInput;
  if (!input || typeof input !== 'object' || Array.isArray(input)) {
    throw new Error('Codebox run-agent-task invocation requires taskInput.');
  }

  const args = [
    useStableRunAgentTask ? WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND : WP_CODEBOX_LEGACY_AGENT_TASK_RUN_CLI_COMMAND,
    `--input-file=${options.inputFilePlaceholder || '{{input_file}}'}`,
    '--json',
  ];
  const previewHold = firstValue(options.previewHold, options.preview_hold);
  if (previewHold) {
    args.push(`--preview-hold-seconds=${previewHold}`);
  }
  const previewPublicUrl = firstValue(options.previewPublicUrl, options.preview_public_url);
  if (previewPublicUrl) {
    args.push(`--preview-public-url=${previewPublicUrl}`);
  }

  return {
    contract: WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA,
    implementation: useStableRunAgentTask ? 'stable-run-agent-task' : 'legacy-agent-task-run-compat',
    input,
    args,
    result_schema: useStableRunAgentTask ? WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA : WP_CODEBOX_AGENT_TASK_RUN_RESPONSE_SCHEMA,
    result_key: 'agent_task_run_result',
  };
}

function isCodeboxLegacyAgentTaskRunResult(result) {
  return result?.schema === WP_CODEBOX_AGENT_TASK_RUN_RESPONSE_SCHEMA;
}

function legacyAgentTaskRunSessionArtifacts(result = {}) {
  if (!isCodeboxLegacyAgentTaskRunResult(result)) {
    return [];
  }
  const artifacts = [];
  if (typeof result.artifacts === 'string' && result.artifacts) {
    artifacts.push({
      id: result.session?.artifacts?.bundle_id || 'wp-codebox-artifacts',
      kind: 'codebox-artifact-directory',
      path: result.artifacts,
      metadata: {
        session_id: result.session?.id,
        preview_url: result.session?.artifacts?.preview_url,
      },
    });
  }
  if (result.session?.artifacts && typeof result.session.artifacts === 'object') {
    artifacts.push({
      id: result.session.artifacts.bundle_id || `wp-codebox-session-artifacts-${artifacts.length + 1}`,
      kind: 'codebox-session-artifacts',
      url: result.session.artifacts.preview_url,
      metadata: result.session.artifacts,
    });
  }
  return artifacts;
}

function legacyAgentTaskRunEvidenceRefs(result = {}) {
  if (!isCodeboxLegacyAgentTaskRunResult(result)) {
    return [];
  }
  return [
    result.session?.artifacts?.preview_url ? {
      kind: 'codebox-preview',
      uri: result.session.artifacts.preview_url,
      label: 'WP Codebox preview',
    } : null,
    typeof result.artifacts === 'string' && result.artifacts ? {
      kind: 'codebox-artifact-directory',
      uri: result.artifacts,
      label: 'WP Codebox artifacts',
    } : null,
  ].filter(Boolean);
}

function firstValue(...values) {
  return values.find((value) => value !== undefined && value !== null && value !== '');
}

function withoutUndefinedValues(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

module.exports = {
  WP_CODEBOX_AGENT_TASK_RUN_RESPONSE_SCHEMA,
  WP_CODEBOX_AGENT_TASK_RUN_RESULT_SCHEMA,
  WP_CODEBOX_LEGACY_AGENT_TASK_RUN_CLI_COMMAND,
  WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND,
  WP_CODEBOX_RUN_AGENT_TASK_REQUEST_SCHEMA,
  WP_CODEBOX_RUN_AGENT_TASK_RESULT_SCHEMA,
  codeboxRunAgentTaskInvocation,
  codeboxRunAgentTaskRequestFromTaskInput,
  isCodeboxLegacyAgentTaskRunResult,
  legacyAgentTaskRunEvidenceRefs,
  legacyAgentTaskRunSessionArtifacts,
};
