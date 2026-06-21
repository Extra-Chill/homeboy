'use strict';

const DATAMACHINE_AGENT_BUNDLE_KEYS = [
  'data_machine_bundle',
  'dataMachineBundle',
];

const DATAMACHINE_RUNTIME_COMPONENT_ALIASES = {
  data_machine: 'agent_runtime',
  data_machine_path: 'agent_runtime',
  data_machine_code: 'agent_runtime_tools',
  data_machine_code_path: 'agent_runtime_tools',
};

module.exports = {
  DATAMACHINE_AGENT_BUNDLE_KEYS,
  DATAMACHINE_RUNTIME_COMPONENT_ALIASES,
};
