'use strict';

/**
 * Internal dependencies
 */
const {
  datamachineAgentCiExecutorConfig,
  datamachineAgentCiRuntimeBackend,
  datamachineAgentCiRuntimeProvider,
} = require('./datamachine-agent-ci-runtime-adapter');
const datamachineAgentCi = require('../../datamachine-agent-ci');

function datamachineAgentCiBundleTaskRequest(options = {}) {
  return datamachineAgentCi.datamachineAgentCiBundleTaskRequest(datamachineAgentCiRuntimeOptions(options), {
    taskExecutorConfig: datamachineAgentCiTaskExecutorConfig,
  });
}

function datamachineAgentCiAbilityTaskRequest(options = {}) {
  return datamachineAgentCi.datamachineAgentCiAbilityTaskRequest(datamachineAgentCiRuntimeOptions(options), {
    taskExecutorConfig: datamachineAgentCiTaskExecutorConfig,
  });
}

function datamachineAgentCiRunnerSpec(options = {}) {
  return datamachineAgentCi.datamachineAgentCiRunnerSpec(datamachineAgentCiRuntimeOptions(options), {
    taskExecutorConfig: datamachineAgentCiTaskExecutorConfig,
  });
}

function datamachineAgentCiRuntimeOptions(options = {}) {
  const runtimeProvider = datamachineAgentCiRuntimeProvider(options);
  return {
    ...options,
    backend: datamachineAgentCiRuntimeBackend(options, runtimeProvider),
    runtime: options.runtime || options.runtimeId || options.runtime_id || runtimeProvider.id,
  };
}

function datamachineAgentCiTaskExecutorConfig(options = {}) {
  return datamachineAgentCiExecutorConfig(
    datamachineAgentCi.datamachineAgentCiTaskExecutorConfig(options)
  );
}

module.exports = {
  ...datamachineAgentCi,
  datamachineAgentCiAbilityTaskRequest,
  datamachineAgentCiBundleTaskRequest,
  datamachineAgentCiRunnerSpec,
  datamachineAgentCiTaskExecutorConfig,
};
