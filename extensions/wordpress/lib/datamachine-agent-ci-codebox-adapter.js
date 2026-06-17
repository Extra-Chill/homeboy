'use strict';

/**
 * Internal dependencies
 */
const {
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE,
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID,
} = require('../../datamachine-agent-ci');

function datamachineAgentCiCodeboxExecutorConfig(config = {}) {
  return {
    ...config,
    runtime_profile: config.runtime_profile || config.runtimeProfile || DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID,
    runtime_profiles: {
      ...(config.runtime_profiles || config.runtimeProfiles || {}),
      [DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID]: DATAMACHINE_AGENT_CI_RUNTIME_PROFILE,
    },
  };
}

module.exports = {
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE,
  DATAMACHINE_AGENT_CI_RUNTIME_PROFILE_ID,
  datamachineAgentCiCodeboxExecutorConfig,
};
