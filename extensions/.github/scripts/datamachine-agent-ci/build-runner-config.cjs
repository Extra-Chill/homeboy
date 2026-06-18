#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const {
  normalizeCommandList,
  normalizeContextRepositories,
  normalizePathSources,
  normalizeProviderPlugin,
  normalizeWritablePaths,
  parseJsonInput,
  splitCsv,
  writeGithubOutput,
} = require('./lib/common.cjs');
const { resolveRuntimeProvider } = require('../../../agent-runtimes/lib/runtime-provider-resolver.cjs');

function main() {
  const config = buildConfig(process.env);
  fs.mkdirSync(path.dirname(config._configPath), { recursive: true });
  fs.writeFileSync(config._configPath, `${JSON.stringify(withoutInternalKeys(config), null, 2)}\n`);
  writeGithubOutput({ config_path: config._configPath, transcript_host_dir: config.transcript_host_dir });
}

function buildConfig(env) {
  const workspace = required(env.GITHUB_WORKSPACE, 'GITHUB_WORKSPACE');
  const runnerTemp = required(env.RUNNER_TEMP, 'RUNNER_TEMP');
  const agentSlug = required(env.AGENT_SLUG, 'AGENT_SLUG');
  const flowSlug = required(env.FLOW_SLUG, 'FLOW_SLUG');
  const targetRepo = required(env.TARGET_REPO, 'TARGET_REPO');
  const bundlePath = env.BUNDLE_PATH || '';
  const componentSlug = path.basename(workspace);
  const transcriptHostDir = path.join(workspace, 'datamachine-agent-artifacts', agentSlug);
  const transcriptGuestDir = `/wordpress/wp-content/plugins/${componentSlug}/datamachine-agent-artifacts/${agentSlug}`;
  const runtimeId = env.AGENT_RUNTIME || 'wp-codebox';
  const runtimeProfile = env.RUNTIME_PROFILE || 'datamachine-agent-ci';
  const runtimeProfiles = parseJsonInput('runtime_profiles', env.RUNTIME_PROFILES || '{}', 'object', {});
  const runtime = resolveRuntimeProvider(runtimeId, { workspace, env });

  const providerPlugin = normalizeProviderPlugin(env.PROVIDER_PLUGIN || '{}', env.PROVIDER || 'openai', true);
  const validationDependencies = validationPaths(workspace, providerPlugin, env.PROVIDER || 'openai');
  const runtimeBin = runtime.paths.runtime_bin;
  if (!fs.existsSync(runtimeBin)) {
    throw new Error(`Runtime CLI build missing at ${runtimeBin}`);
  }

  const executeWorkflow = resolveExecuteWorkflowMounts(env.EXECUTE_WORKFLOW_PATH || '', workspace, componentSlug);
  const runnerWorkspace = parseJsonInput('runner_workspace', env.RUNNER_WORKSPACE_CONFIG || '{}', 'object', {});
  const runnerWorkspaceRepo = targetRepo.split('/')[1].replace(/\.git$/, '');
  const runnerWorkspaceGuestCheckout = `/workspace/${runnerWorkspaceRepo}`;
  const runnerWorkspaceMounts = runnerWorkspace.enabled === true ? [{
    type: 'directory',
    source: workspace,
    target: runnerWorkspaceGuestCheckout,
    mode: 'readwrite',
    metadata: { kind: 'runner-workspace', artifactExcludePaths: ['.ci/**'] },
  }] : [];
  const effectiveRunnerWorkspace = runnerWorkspace.enabled === true ? { ...runnerWorkspace, checkout_path: runnerWorkspaceGuestCheckout } : runnerWorkspace;
  const contextRepositories = normalizeContextRepositories(env.CONTEXT_REPOSITORIES || '[]');
  const appTokenRepos = splitCsv(env.APP_TOKEN_REPOS || targetRepo);
  const allowedRepos = parseJsonInput('allowed_repos', env.ALLOWED_REPOS || '[]', 'array', []);
  const providerCredentials = providerPlugin.credentials || {};
  const providerBenchEnv = {};
  for (const providerEnvName of Object.values(providerCredentials)) {
    if (typeof providerEnvName !== 'string' || providerEnvName.length === 0) {
      continue;
    }
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(providerEnvName)) {
      throw new Error(`Invalid provider credential env name: ${providerEnvName}`);
    }
    if (env[providerEnvName]) {
      providerBenchEnv[providerEnvName] = env[providerEnvName];
    }
  }

  const workloadRunBefore = parseJsonInput('workload_run_before', env.WORKLOAD_RUN_BEFORE || '[]', 'array', []);
  const workloadRunAfter = parseJsonInput('workload_run_after', env.WORKLOAD_RUN_AFTER || '[]', 'array', []);
  const extraRequiredAbilities = parseJsonInput('extra_required_abilities', env.EXTRA_REQUIRED_ABILITIES || '[]', 'array', []);
  const runtimeTask = runtimeTaskFromEnv(env);
  const runtimeTaskAbility = env.RUNTIME_TASK_ABILITY || runtimeProfiles[runtimeProfile]?.runtime_task_ability || 'datamachine/run-agent-bundle';
  const runtimeAbilityRequirements = Array.isArray(runtimeProfiles[runtimeProfile]?.ability_requirements)
    ? runtimeProfiles[runtimeProfile].ability_requirements
    : [runtimeTaskAbility];
  const executionKind = env.EXECUTION_KIND || (runtimeTask ? 'runtime_task' : 'agent_bundle');
  if (executionKind === 'agent_bundle' && !bundlePath) {
    throw new Error('BUNDLE_PATH is required for agent_bundle execution. Set execution_kind=runtime_task with runtime_task or ability_request for direct ability execution.');
  }
  const executeRequiredAbilities = runtimeTask
    ? [runtimeTask.ability]
    : executeWorkflow.path
      ? ['datamachine/import-agent', 'datamachine/execute-workflow', 'datamachine/drain-job']
      : runtimeProfile === 'datamachine-agent-ci'
        ? ['datamachine/import-agent', 'datamachine/run-flow', 'datamachine/drain-job']
        : runtimeAbilityRequirements;
  const runtimeComponents = parseJsonInput('runtime_components', env.RUNTIME_COMPONENTS || '{}', 'object', {});

  return {
    _configPath: path.join(runnerTemp, 'datamachine-agent-config.json'),
    component_id: 'datamachine-agent-ci-driver',
    component_path: workspace,
    workload_id: flowSlug,
    workload_label: `Run ${agentSlug} Data Machine agent`,
    validation_dependencies: validationDependencies.paths,
    runtime_id: runtimeId,
    runtime_profile: runtimeProfile,
    ...(Object.keys(runtimeProfiles).length > 0 ? { runtime_profiles: runtimeProfiles } : {}),
    execution_kind: executionKind,
    runtime_ref: env.AGENT_RUNTIME_REF || 'main',
    runtime_wordpress_version: env.RUNTIME_WORDPRESS_VERSION || '7.0',
    runtime_mounts: [
      {
        type: 'file',
        source: path.join(workspace, '.ci/homeboy-extensions/wordpress/tests/fixtures/datamachine-agent-ci-driver/datamachine-agent-ci-driver.php'),
        target: '/wordpress/wp-content/plugins/datamachine-agent-ci-driver/datamachine-agent-ci-driver.php',
        mode: 'readonly',
      },
      ...normalizePathSources(parseJsonInput('runtime_mounts', env.RUNTIME_MOUNTS || '[]', 'array', []), workspace),
      ...executeWorkflow.mounts,
      ...runnerWorkspaceMounts,
    ],
    wp_config_defines: parseJsonInput('extra_wp_config_defines', env.EXTRA_WP_CONFIG_DEFINES || '{}', 'object', {}),
    runtime_overlays: normalizePathSources(parseJsonInput('runtime_overlays', env.RUNTIME_OVERLAYS || '[]', 'array', []), workspace),
    workload_run_before: workloadRunBefore,
    workload_run_after: workloadRunAfter,
    required_abilities: Array.from(new Set([...executeRequiredAbilities, ...extraRequiredAbilities])),
    ...(bundlePath ? { bundle_path: path.join(workspace, bundlePath) } : {}),
    bundle_repo: env.BUNDLE_REPO || `https://github.com/${targetRepo}.git`,
    bundle_ref: env.BUNDLE_REF || env.GITHUB_SHA,
    bundle_path_in_repo: env.BUNDLE_PATH_IN_REPO || bundlePath,
    agent_slug: agentSlug,
    pipeline_slug: required(env.PIPELINE_SLUG, 'PIPELINE_SLUG'),
    flow_slug: flowSlug,
    provider: env.PROVIDER || 'openai',
    model: env.MODEL || 'gpt-5.5',
    provider_register_function: providerPlugin.register_function || '',
    provider_credentials: providerCredentials,
    runtime_bin: runtimeBin,
    runtime_components: {
      runtime: runtime.paths.runtime_component,
      agents_api: path.join(workspace, '.ci/agents-api'),
      data_machine: path.join(workspace, '.ci/data-machine'),
      data_machine_code: path.join(workspace, '.ci/data-machine-code'),
      ...runtimeComponents,
    },
    provider_plugin_paths: validationDependencies.providerPluginHostPath ? [validationDependencies.providerPluginHostPath] : [],
    github_token_env: 'HOMEBOY_GITHUB_APP_TOKEN',
    github_repository_token_env: 'GITHUB_TOKEN',
    github_profile_id: `${agentSlug}-ci`,
    target_repo: targetRepo,
    context_repositories: contextRepositories,
    verification_commands: normalizeCommandList('verification_commands', env.VERIFICATION_COMMANDS || '[]'),
    drift_checks: normalizeCommandList('drift_checks', env.DRIFT_CHECKS || '[]'),
    writable_paths: normalizeWritablePaths(env.WRITABLE_PATHS || ''),
    workspace_contract_checks: parseJsonInput('workspace_contract_checks', env.WORKSPACE_CONTRACT_CHECKS || '{}', 'object', {}),
    host_runner_lifecycle: true,
    datamachine_code_policy_attestation: {
      schema: 'homeboy/datamachine-agent-ci-dmc-policy/v1',
      target_repo: targetRepo,
      write_boundary: targetRepo,
      context_repositories: contextRepositories,
      context_repository_api: {
        upstream_issue: 'https://github.com/Extra-Chill/data-machine-code/issues/617',
        status: contextRepositories.length > 0 ? 'blocked_until_available' : 'not_requested',
      },
    },
    allowed_repos: allowedRepos.length > 0 ? allowedRepos : appTokenRepos.length > 0 ? appTokenRepos : [targetRepo],
    engine_key: env.ENGINE_KEY || '',
    tool_results_key: env.TOOL_RESULTS_KEY || 'github_tool_results',
    enable_terminal_actions: env.ENABLE_TERMINAL_ACTIONS === 'true',
    wp_cli_tool_name: env.WP_CLI_TOOL_NAME || 'run_wp_cli',
    max_turns: Number(env.MAX_TURNS || 12),
    prompt: env.PROMPT || '',
    step_budget: Number(env.STEP_BUDGET || 16),
    time_budget_ms: Number(env.TIME_BUDGET_MS || 600000),
    expected_artifacts: parseJsonInput('expected_artifacts', env.EXPECTED_ARTIFACTS || '[]', 'array', []),
    artifact_declarations: parseJsonInput('artifact_declarations', env.ARTIFACT_DECLARATIONS || '[]', 'array', []),
    output_mappings: parseJsonInput('output_mappings', env.OUTPUT_MAPPINGS || '{}', 'object', {}),
    component_contracts: parseJsonInput('component_contracts', env.COMPONENT_CONTRACTS || '[]', 'array', []),
    ...(runtimeTask ? { runtime_task: runtimeTask } : {}),
    ability_tools: parseJsonInput('ability_tools', env.ABILITY_TOOLS || '[]', 'array', []),
    tool_recorders: parseJsonInput('tool_recorders', env.TOOL_RECORDERS || '[]', 'array', []),
    pipeline_step_patches: parseJsonInput('pipeline_step_patches', env.PIPELINE_STEP_PATCHES || '[]', 'array', []),
    flow_step_patches: parseJsonInput('flow_step_patches', env.FLOW_STEP_PATCHES || '[]', 'array', []),
    runner_workspace: effectiveRunnerWorkspace,
    rules: parseJsonInput('rules', env.RULES || '{}', 'object', {}),
    general_rules: parseJsonInput('general_rules', env.GENERAL_RULES || '[]', 'array', []),
    task_rules: parseJsonInput('task_rules', env.TASK_RULES || '[]', 'array', []),
    probes: parseJsonInput('probes', env.PROBES || '{}', 'object', {}),
    wp_gym_eval: { benchmark_mode: env.WP_GYM_BENCHMARK_MODE === 'true' },
    execute_workflow_path: executeWorkflow.path,
    success_requires_pr: env.SUCCESS_REQUIRES_PR === 'true',
    success_completion_outcomes: parseJsonInput('success_completion_outcomes', env.SUCCESS_COMPLETION_OUTCOMES || '[]', 'array', []),
    artifact_export: {
      enabled: true,
      only_when_no_pr: true,
      repo: targetRepo,
      path_prefix: env.BUNDLE_PATH_IN_REPO || bundlePath,
      include_job_artifacts: false,
      branch_template: 'agent-artifacts/{agent_slug}-{run_id}-{provider}-{model}-{job_id}',
      commit_message_template: 'chore: persist {type} artifact',
      pr_title_template: '[{agent_slug}] {task_id} - {model_label} - {result_label}',
      pr_body_template: '## Result\n{result_table}\n\n## Checks\n{checks_table}\n\n## Tools\n{tools_table}\n\n## Review Artifacts\n{links_table}\n',
      ...parseJsonInput('artifact_export_config', env.ARTIFACT_EXPORT_CONFIG || '{}', 'object', {}),
    },
    dry_run: env.DRY_RUN === 'true',
    transcript_dir: transcriptGuestDir,
    transcript_host_dir: transcriptHostDir,
    bench_env: {
      GITHUB_TOKEN: env.GITHUB_REPOSITORY_TOKEN_VALUE || '',
      HOMEBOY_GITHUB_APP_TOKEN: env.GITHUB_APP_TOKEN_VALUE || '',
      GITHUB_RUN_ID: env.GITHUB_RUN_ID_VALUE || '',
      GITHUB_RUN_ATTEMPT: env.GITHUB_RUN_ATTEMPT_VALUE || '',
      ...providerBenchEnv,
    },
    ...(env.DAILY_MEMORY_ENABLED === 'true' ? { daily_memory_enabled: true } : {}),
    ...(env.DISABLE_DATAMACHINE_DIRECTIVES === 'true' ? { disable_datamachine_directives: true } : {}),
  };
}

function validationPaths(workspace, providerPlugin, provider) {
  let paths = [];
  const ciDir = path.join(workspace, '.ci');
  if (fs.existsSync(ciDir)) {
    paths = fs.readdirSync(ciDir, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => path.join(ciDir, entry.name))
      .sort();
  }
  let providerPluginHostPath = '';
  if (!providerPlugin.repo && provider === 'openai' && fs.existsSync(path.join(ciDir, 'ai-provider-for-openai'))) {
    providerPluginHostPath = path.join(ciDir, 'ai-provider-for-openai');
  }
  if (providerPlugin.repo) {
    const providerPluginRepoName = providerPlugin.repo.split('/')[1];
    const providerPluginRoot = path.join(ciDir, providerPluginRepoName);
    providerPluginHostPath = providerPlugin.path === '.' ? providerPluginRoot : path.join(providerPluginRoot, providerPlugin.path);
    if (fs.existsSync(providerPluginHostPath)) {
      paths = paths.filter((entry) => entry !== providerPluginRoot).concat(providerPluginHostPath);
    }
  }
  return { paths, providerPluginHostPath };
}

function resolveExecuteWorkflowMounts(executeWorkflowPath, workspace, componentSlug) {
  if (!executeWorkflowPath || path.isAbsolute(executeWorkflowPath)) {
    return { path: executeWorkflowPath, mounts: [] };
  }
  const hostPath = path.join(workspace, executeWorkflowPath);
  const type = fs.existsSync(hostPath) && fs.statSync(hostPath).isFile() ? 'file' : 'directory';
  const guestPath = `/wordpress/wp-content/plugins/${componentSlug}/${executeWorkflowPath}`;
  return { path: guestPath, mounts: [{ type, source: hostPath, target: guestPath, mode: 'readonly' }] };
}

function runtimeTaskFromEnv(env) {
  const runtimeTask = parseJsonInput('runtime_task', env.RUNTIME_TASK || '{}', 'object', {});
  if (Object.keys(runtimeTask).length > 0) {
    if (!runtimeTask.ability || typeof runtimeTask.ability !== 'string') {
      throw new Error('runtime_task.ability is required when runtime_task is supplied.');
    }
    return runtimeTask;
  }

  const abilityRequest = parseJsonInput('ability_request', env.ABILITY_REQUEST || '{}', 'object', {});
  const abilityInput = parseJsonInput('ability_input', env.ABILITY_INPUT || '{}', 'object', {});
  if (Object.keys(abilityRequest).length === 0 && Object.keys(abilityInput).length === 0) {
    return null;
  }
  if (!abilityRequest.ability || typeof abilityRequest.ability !== 'string') {
    throw new Error('ability_request.ability is required when ability_request or ability_input is supplied.');
  }

  return {
    ...abilityRequest,
    input: {
      ...(abilityRequest.input && typeof abilityRequest.input === 'object' && !Array.isArray(abilityRequest.input) ? abilityRequest.input : {}),
      ...abilityInput,
    },
  };
}

function withoutInternalKeys(config) {
  return Object.fromEntries(Object.entries(config).filter(([key]) => !key.startsWith('_')));
}

function required(value, name) {
  if (!value) {
    throw new Error(`${name} is required`);
  }
  return value;
}

try {
  main();
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}

module.exports = { buildConfig };
