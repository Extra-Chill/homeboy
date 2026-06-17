'use strict';

const DATAMACHINE_AGENT_CI_COMPONENT_PATH_DEFAULTS = {
  contract_slug_map: {
    'agents-api': 'agents_api',
    'data-machine': 'agent_runtime',
    'data-machine-code': 'agent_runtime_tools',
  },
  path_aliases: {
    agents_api: [
      'explicit:agents_api',
      'runtime_component:agents_api',
      'contract:agents_api',
      'config:agents_api',
      'config_path:agents_api',
      'option:agentsApi',
    ],
    agent_runtime: [
      'explicit:agent_runtime',
      'runtime_component:agent_runtime',
      'runtime_component:data_machine',
      'contract:agent_runtime',
      'config:agent_runtime',
      'config_path:agent_runtime',
      'config:data_machine',
      'config_path:data_machine',
      'option:legacyRuntime',
    ],
    agent_runtime_tools: [
      'explicit:agent_runtime_tools',
      'runtime_component:agent_runtime_tools',
      'runtime_component:data_machine_code',
      'contract:agent_runtime_tools',
      'config:agent_runtime_tools',
      'config_path:agent_runtime_tools',
      'config:data_machine_code',
      'config_path:data_machine_code',
      'option:legacyRuntimeTools',
    ],
  },
  discovery: {
    agents_api: [
      { settings: ['wp_codebox_agents_api_path', 'agents_api_path'] },
      { env: 'HOMEBOY_WP_CODEBOX_AGENTS_API_PATH' },
      {
        bundled_provider: 'agent_runtime',
        paths: [
          'vendor/wordpress/agents-api',
          'vendor/automattic/agents-api',
        ],
      },
    ],
    agent_runtime: [
      { settings: ['wp_codebox_data_machine_path', 'data_machine_path'] },
      { env: 'HOMEBOY_DATA_MACHINE_PATH' },
      { active_plugin: 'data-machine' },
      { sibling: 'data-machine' },
    ],
    agent_runtime_tools: [
      { settings: ['wp_codebox_data_machine_code_path', 'data_machine_code_path'] },
      { env: 'HOMEBOY_DATA_MACHINE_CODE_PATH' },
      { active_plugin: 'data-machine-code' },
      { sibling: 'data-machine-code' },
    ],
  },
};

const DATAMACHINE_AGENT_CI_WORKSPACE_TOOLS = {
  readonly: [
    'workspace_ls',
    'workspace_read',
    'workspace_git_status',
  ],
  readwrite: [
    'workspace_run_runner_command',
    'workspace_write',
    'workspace_edit',
    'workspace_apply_patch',
    'workspace_delete',
    'workspace_git_add',
  ],
};

const DATAMACHINE_AGENT_CI_CAPABILITIES = [
  'tool:datamachine/run-agent-bundle',
  'tool:github_issue_publish',
  'tool:github_pull_request_publish',
  'tool:comment_github_pull_request',
  'ability:datamachine/run-agent-bundle',
  'ability:github_issue_publish',
  'ability:github_pull_request_publish',
  'ability:comment_github_pull_request',
];

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
