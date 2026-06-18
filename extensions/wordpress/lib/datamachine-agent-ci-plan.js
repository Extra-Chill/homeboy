'use strict';

/**
 * Internal dependencies
 */
const {
  datamachineAgentCiCodeboxExecutorConfig,
} = require('./datamachine-agent-ci-codebox-adapter');
const datamachineAgentCi = require('../../datamachine-agent-ci');

function datamachineAgentCiBundleTaskRequest(options = {}) {
  return datamachineAgentCi.datamachineAgentCiBundleTaskRequest(datamachineAgentCiCodeboxOptions(options), {
    taskExecutorConfig: datamachineAgentCiTaskExecutorConfig,
  });
}

function datamachineAgentCiAbilityTaskRequest(options = {}) {
  return datamachineAgentCi.datamachineAgentCiAbilityTaskRequest(datamachineAgentCiCodeboxOptions(options), {
    taskExecutorConfig: datamachineAgentCiTaskExecutorConfig,
  });
}

function datamachineAgentCiRunnerSpec(options = {}) {
  return datamachineAgentCi.datamachineAgentCiRunnerSpec(datamachineAgentCiCodeboxOptions(options), {
    taskExecutorConfig: datamachineAgentCiTaskExecutorConfig,
  });
}

function datamachineAgentCiCodeboxOptions(options = {}) {
  return {
    ...options,
    backend: options.backend || options.runtimeBackend || options.runtime_backend || 'codebox',
  };
}

function datamachineAgentCiTaskExecutorConfig(options = {}) {
  return datamachineAgentCiCodeboxExecutorConfig(
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
