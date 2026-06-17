'use strict';

/**
 * Internal dependencies
 */
const {
  datamachineAgentCiCodeboxExecutorConfig,
} = require('./datamachine-agent-ci-codebox-adapter');
const datamachineAgentCi = require('../../datamachine-agent-ci');

function datamachineAgentCiBundleTaskRequest(options = {}) {
  return datamachineAgentCi.datamachineAgentCiBundleTaskRequest(options, {
    taskExecutorConfig: datamachineAgentCiTaskExecutorConfig,
  });
}

function datamachineAgentCiAbilityTaskRequest(options = {}) {
  return datamachineAgentCi.datamachineAgentCiAbilityTaskRequest(options, {
    taskExecutorConfig: datamachineAgentCiTaskExecutorConfig,
  });
}

function datamachineAgentCiRunnerSpec(options = {}) {
  return datamachineAgentCi.datamachineAgentCiRunnerSpec(options, {
    taskExecutorConfig: datamachineAgentCiTaskExecutorConfig,
  });
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
