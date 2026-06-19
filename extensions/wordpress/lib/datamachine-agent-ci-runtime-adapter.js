'use strict';

/**
 * External dependencies
 */
const path = require('node:path');

/**
 * Internal dependencies
 */
const {
  DEFAULT_RUNTIME_ID,
  resolveRuntimeProvider,
} = require('../../agent-runtimes/lib/runtime-provider-resolver.cjs');
const {
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE,
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID,
} = require('../../datamachine-agent-ci');

const REPO_ROOT = path.resolve(__dirname, '..', '..');
const DATAMACHINE_AGENT_CI_DEFAULT_BACKEND = 'codebox';

function datamachineAgentCiRuntimeId(config = {}) {
  return config.runtimeProvider
    || config.runtime_provider
    || config.runtime
    || config.runtimeId
    || config.runtime_id
    || config.agentRuntime
    || config.agent_runtime
    || process.env.AGENT_RUNTIME
    || DEFAULT_RUNTIME_ID;
}

function datamachineAgentCiRuntimeProvider(config = {}, options = {}) {
  return resolveRuntimeProvider(datamachineAgentCiRuntimeId(config), {
    repoRoot: options.repoRoot || REPO_ROOT,
    registry: options.registry,
    workspace: options.workspace || config.component_path || config.workspace || process.cwd(),
    env: options.env,
  });
}

function datamachineAgentCiRuntimeBackend(config = {}, runtimeProvider) {
  return config.backend
    || config.runtimeBackend
    || config.runtime_backend
    || config.agentRuntimeBackend
    || config.agent_runtime_backend
    || runtimeProvider?.executor?.backend
    || DATAMACHINE_AGENT_CI_DEFAULT_BACKEND;
}

function datamachineAgentCiExecutorConfig(config = {}) {
  const runtimeId = datamachineAgentCiRuntimeId(config);
  return {
    ...config,
    runtime_provider: config.runtime_provider || config.runtimeProvider || runtimeId,
    runtime_id: config.runtime_id || config.runtimeId || runtimeId,
    runtime_profile: config.runtime_profile || config.runtimeProfile || DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID,
    runtime_profiles: {
      ...(config.runtime_profiles || config.runtimeProfiles || {}),
      [DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID]: DATAMACHINE_AGENT_CI_RUNTIME_PROFILE,
    },
  };
}

const datamachineAgentCiCodeboxExecutorConfig = datamachineAgentCiExecutorConfig;

module.exports = {
  DATAMACHINE_AGENT_CI_DEFAULT_BACKEND,
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE,
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID,
  datamachineAgentCiCodeboxExecutorConfig,
  datamachineAgentCiExecutorConfig,
  datamachineAgentCiRuntimeBackend,
  datamachineAgentCiRuntimeId,
  datamachineAgentCiRuntimeProvider,
};
