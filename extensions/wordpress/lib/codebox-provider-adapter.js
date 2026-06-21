'use strict';

/**
 * Internal dependencies
 */
const applyAdapter = require('./wp-codebox-apply-adapter');
const recipeHelper = require('./wp-codebox-recipe-helper');
const runtimeProvider = require('./audit-wp-codebox-runtime-provider');

const CODEBOX_PROVIDER_ADAPTER_ID = 'homeboy/codebox-provider-adapter/v1';

function runAgentTask(taskRequest, options = {}) {
  if (options.sync) {
    return runtimeProvider.executeWpCodeboxTaskRequest(taskRequest, options);
  }

  return runtimeProvider.executeWpCodeboxTaskRequestAsync(taskRequest, options);
}

function runAgentTaskSync(taskRequest, options = {}) {
  return runtimeProvider.executeWpCodeboxTaskRequest(taskRequest, options);
}

function runRecipe(options = {}) {
  return recipeHelper.runWpCodeboxRecipe(options);
}

function loadArtifactBundle(bundlePath) {
  return applyAdapter.loadWpCodeboxArtifactBundle(bundlePath);
}

function normalizeOutcome(taskRequest, parsed, artifact, success, errorMessage = '', timedOut = false) {
  return runtimeProvider.taskOutcome(taskRequest, parsed, artifact, success, errorMessage, timedOut);
}

function preflightApply(options = {}) {
  return applyAdapter.normalizeWpCodeboxPreflight(options);
}

async function preflightApplyAsync(options = {}) {
  return applyAdapter.normalizeWpCodeboxPreflightAsync(options);
}

module.exports = {
  CODEBOX_PROVIDER_ADAPTER_ID,
  runAgentTask,
  runAgentTaskSync,
  runRecipe,
  loadArtifactBundle,
  normalizeOutcome,
  preflightApply,
  preflightApplyAsync,

  ADAPTER_ID: applyAdapter.ADAPTER_ID,
  APPLY_RESULT_SCHEMA: applyAdapter.APPLY_RESULT_SCHEMA,
  DEFAULT_TASK_TIMEOUT_SECONDS: runtimeProvider.DEFAULT_TASK_TIMEOUT_SECONDS,
  RUN_SCHEMA: runtimeProvider.RUN_SCHEMA,
  TASK_SCHEMA: runtimeProvider.TASK_SCHEMA,
  applyApprovedWpCodeboxArtifact: applyAdapter.applyApprovedWpCodeboxArtifact,
  createWpCodeboxTaskRequest: runtimeProvider.createWpCodeboxTaskRequest,
  executeWpCodeboxTaskRequest: runtimeProvider.executeWpCodeboxTaskRequest,
  executeWpCodeboxTaskRequestAsync: runtimeProvider.executeWpCodeboxTaskRequestAsync,
  loadWpCodeboxArtifactBundle: applyAdapter.loadWpCodeboxArtifactBundle,
  normalizeWpCodeboxPreflight: applyAdapter.normalizeWpCodeboxPreflight,
  normalizeWpCodeboxPreflightAsync: applyAdapter.normalizeWpCodeboxPreflightAsync,
  pullRequestUrls: runtimeProvider.pullRequestUrls,
  runWpCodeboxApplyPreflight: applyAdapter.runWpCodeboxApplyPreflight,
  runWpCodeboxRecipe: recipeHelper.runWpCodeboxRecipe,
  sandboxSessionId: runtimeProvider.sandboxSessionId,
  taskOutcome: runtimeProvider.taskOutcome,
  taskOutcomeSucceeded: runtimeProvider.taskOutcomeSucceeded,
  verifyWpCodeboxPayload: applyAdapter.verifyWpCodeboxPayload,
  wpCodeboxApplyRequestFromBundle: applyAdapter.wpCodeboxApplyRequestFromBundle,
  wpCodeboxApplyRequestFromBundleAsync: applyAdapter.wpCodeboxApplyRequestFromBundleAsync,
  wpCodeboxAuditRuntimeOptions: runtimeProvider.wpCodeboxAuditRuntimeOptions,
  wpCodeboxChangeArtifactFromBundle: applyAdapter.wpCodeboxChangeArtifactFromBundle,
  wpCodeboxChangeArtifactFromPreflight: applyAdapter.wpCodeboxChangeArtifactFromPreflight,
};
