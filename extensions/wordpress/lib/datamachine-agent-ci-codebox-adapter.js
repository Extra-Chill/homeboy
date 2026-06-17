'use strict';

/**
 * Internal dependencies
 */
const {
  DATAMACHINE_AGENT_CI_CAPABILITIES,
  DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS,
  DATAMACHINE_AGENT_CI_WORKSPACE_TOOLS,
} = require('../../datamachine-agent-ci');

function datamachineAgentCiCodeboxExecutorConfig(config = {}) {
  return {
    ...config,
    runtime_requirements: {
      ...(config.runtime_requirements || {}),
      component_path_defaults: config.component_path_defaults || DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS,
      workspace_tools: config.workspace_tools || DATAMACHINE_AGENT_CI_WORKSPACE_TOOLS,
      capabilities: uniqueStrings([
        ...DATAMACHINE_AGENT_CI_CAPABILITIES,
        ...normalizeArray(config.runtime_requirements?.capabilities),
      ]),
    },
    component_path_defaults: config.component_path_defaults || DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS,
    workspace_tools: config.workspace_tools || DATAMACHINE_AGENT_CI_WORKSPACE_TOOLS,
  };
}

function normalizeArray(value) {
  return Array.isArray(value) ? value.filter(Boolean) : [];
}

function uniqueStrings(values) {
  return Array.from(new Set(values.filter((value) => typeof value === 'string' && value.trim() !== '')));
}

module.exports = {
  DATAMACHINE_AGENT_CI_CAPABILITIES,
  DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS,
  DATAMACHINE_AGENT_CI_WORKSPACE_TOOLS,
  datamachineAgentCiCodeboxExecutorConfig,
};
