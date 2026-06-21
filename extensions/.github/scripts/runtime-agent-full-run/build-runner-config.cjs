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
const { DEFAULT_RUNTIME_ID, resolveRuntimeProvider } = require('../../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');
const {
  runtimeAgentCiFirstNonEmptyArray,
  runtimeAgentCiFirstNonEmptyObject,
  runtimeAgentCiTaskFromRequest,
} = require('../../../runtime-agent-ci');

function main() {
  const config = buildConfig(process.env);
  fs.mkdirSync(path.dirname(config._configPath), { recursive: true });
  fs.writeFileSync(config._configPath, `${JSON.stringify(withoutInternalKeys(config), null, 2)}\n`);
  writeGithubOutput({ config_path: config._configPath, transcript_host_dir: config.transcript_host_dir });
}

function buildConfig(env) {
  const workspace = required(env.GITHUB_WORKSPACE, 'GITHUB_WORKSPACE');
  const runnerTemp = required(env.RUNNER_TEMP, 'RUNNER_TEMP');
  const workloadId = required(env.WORKLOAD_ID, 'WORKLOAD_ID');
  const targetRepo = required(env.TARGET_REPO, 'TARGET_REPO');
  const componentId = env.COMPONENT_ID || path.basename(workspace);
  const componentPath = env.COMPONENT_PATH || workspace;
  const runtimeId = env.RUNTIME || env.RUNTIME_PROVIDER || env.BACKEND || DEFAULT_RUNTIME_ID;
  const runtimeProfile = required(env.PROFILE || env.RUNTIME_PROFILE, 'PROFILE or RUNTIME_PROFILE');
  const runtimeProfiles = parseJsonInput('runtime_profiles', env.RUNTIME_PROFILES || '{}', 'object', {});
  const runtime = resolveRuntimeProvider(runtimeId, { workspace, env });
  const runtimeBin = runtime.paths.runtime_bin;
  if (runtimePathRequired(runtime, 'runtime_bin') && (!runtimeBin || !fs.existsSync(runtimeBin))) {
    throw new Error(`Runtime CLI build missing at ${runtimeBin || 'runtime.paths.runtime_bin'}`);
  }

  const providerPlugin = normalizeProviderPlugin(env.PROVIDER_PLUGIN || '{}', env.PROVIDER || '', false);
  const validationDependencies = validationPaths(workspace, providerPlugin, env.PROVIDER || '');
  const providerSecretEnvMapping = providerPlugin.provider_secret_env || {};
  const providerBenchEnv = {};
  for (const providerEnvName of Object.values(providerSecretEnvMapping)) {
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

  const runtimeTask = runtimeTaskFromEnv(env);
  const runtimeExecution = parseJsonInput('runtime_execution', env.RUNTIME_EXECUTION || '{}', 'object', {});
  const runtimeOutputProjections = runtimeAgentCiFirstNonEmptyObject(
    parseJsonInput('runtime_output_projections', env.RUNTIME_OUTPUT_PROJECTIONS || '{}', 'object', {}),
    parseJsonInput('engine_data_outputs', env.ENGINE_DATA_OUTPUTS || '{}', 'object', {})
  );
  const evidenceProjections = runtimeAgentCiFirstNonEmptyArray(
    parseJsonInput('evidence_projections', env.EVIDENCE_PROJECTIONS || '[]', 'array', []),
    parseJsonInput('tool_recorders', env.TOOL_RECORDERS || '[]', 'array', [])
  );
  const runtimeComponents = parseJsonInput('runtime_components', env.RUNTIME_COMPONENTS || '{}', 'object', {});
  const runtimeOverlays = normalizePathSources(parseJsonInput('runtime_overlays', env.RUNTIME_OVERLAYS || '[]', 'array', []), workspace);
  const componentContracts = parseJsonInput('component_contracts', env.COMPONENT_CONTRACTS || '[]', 'array', []);
  const runtimeEnv = parseJsonInput('runtime_env', env.RUNTIME_ENV || '{}', 'object', {});
  const runtimeConfig = parseJsonInput('runtime_config', env.RUNTIME_CONFIG || '{}', 'object', {});
  const providerPluginPaths = validationDependencies.providerPluginHostPath ? [validationDependencies.providerPluginHostPath] : [];
  const resolvedRuntimeProfile = runtimeProfiles[runtimeProfile] || {};
  const effectiveRuntimeProfile = runtimeProfilePayload(runtime, {
    id: runtimeProfile,
    profile: resolvedRuntimeProfile,
    componentContracts,
    runtimeOverlays,
    runtimeEnv,
    providerPluginPaths,
  });
  const effectiveRuntimeProfiles = {
    ...runtimeProfiles,
    [runtimeProfile]: effectiveRuntimeProfile,
  };
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
  const appTokenRepos = splitCsv(env.APP_TOKEN_REPOS || targetRepo);
  const allowedRepos = parseJsonInput('allowed_repos', env.ALLOWED_REPOS || '[]', 'array', []);
  const transcriptHostDir = path.join(workspace, 'runtime-agent-artifacts', workloadId);
  const transcriptGuestDir = `/wordpress/wp-content/plugins/${componentId}/runtime-agent-artifacts/${workloadId}`;

  return {
    ...runtimeConfig,
    _configPath: path.join(runnerTemp, 'runtime-agent-full-run-config.json'),
    component_id: componentId,
    component_path: componentPath,
    workload_id: workloadId,
    workload_label: env.WORKLOAD_LABEL || `Run ${workloadId}`,
    validation_dependencies: validationDependencies.paths,
    runtime_id: runtime.id,
    runtime_profile: runtimeProfile,
    runtime_profiles: effectiveRuntimeProfiles,
    runtime_ref: env.RUNTIME_REF || 'main',
    runtime_wordpress_version: env.RUNTIME_WORDPRESS_VERSION || '7.0',
    execution_kind: env.EXECUTION_KIND || (runtimeTask ? 'runtime_task' : 'runtime_execution'),
    runtime_mounts: [
      ...normalizePathSources(parseJsonInput('runtime_mounts', env.RUNTIME_MOUNTS || '[]', 'array', []), workspace),
      ...runnerWorkspaceMounts,
    ],
    wp_config_defines: parseJsonInput('extra_wp_config_defines', env.EXTRA_WP_CONFIG_DEFINES || '{}', 'object', {}),
    runtime_overlays: runtimeOverlays,
    workload_run_before: parseJsonInput('workload_run_before', env.WORKLOAD_RUN_BEFORE || '[]', 'array', []),
    workload_run_after: parseJsonInput('workload_run_after', env.WORKLOAD_RUN_AFTER || '[]', 'array', []),
    required_abilities: parseJsonInput('required_abilities', env.REQUIRED_ABILITIES || '[]', 'array', []),
    provider: env.PROVIDER || '',
    model: env.MODEL || '',
    provider_register_function: providerPlugin.register_function || '',
    provider_secret_env_mapping: providerSecretEnvMapping,
    ...(runtimeBin ? { runtime_bin: runtimeBin } : {}),
    runtime_components: {
      ...(runtime.paths.runtime_component ? { runtime: runtime.paths.runtime_component } : {}),
      ...runtimeComponents,
    },
    runtime_component_paths: runtimeComponents,
    provider_plugin_paths: providerPluginPaths,
    runtime_requirements: effectiveRuntimeProfile,
    github_token_env: 'HOMEBOY_GITHUB_APP_TOKEN',
    github_repository_token_env: 'GITHUB_TOKEN',
    github_profile_id: env.GITHUB_PROFILE_ID || `${workloadId}-ci`,
    target_repo: targetRepo,
    context_repositories: normalizeContextRepositories(env.CONTEXT_REPOSITORIES || '[]'),
    verification_commands: normalizeCommandList('verification_commands', env.VERIFICATION_COMMANDS || '[]'),
    drift_checks: normalizeCommandList('drift_checks', env.DRIFT_CHECKS || '[]'),
    writable_paths: normalizeWritablePaths(env.WRITABLE_PATHS || ''),
    workspace_contract_checks: parseJsonInput('workspace_contract_checks', env.WORKSPACE_CONTRACT_CHECKS || '{}', 'object', {}),
    host_runner_lifecycle: true,
    allowed_repos: allowedRepos.length > 0 ? allowedRepos : appTokenRepos.length > 0 ? appTokenRepos : [targetRepo],
    engine_key: env.ENGINE_KEY || '',
    tool_results_key: env.TOOL_RESULTS_KEY || 'tool_results',
    max_turns: Number(env.MAX_TURNS || 12),
    prompt: env.PROMPT || '',
    step_budget: Number(env.STEP_BUDGET || 16),
    time_budget_ms: Number(env.TIME_BUDGET_MS || 600000),
    expected_artifacts: parseJsonInput('expected_artifacts', env.EXPECTED_ARTIFACTS || '[]', 'array', []),
    artifact_declarations: parseJsonInput('artifact_declarations', env.ARTIFACT_DECLARATIONS || '[]', 'array', []),
    output_mappings: parseJsonInput('output_mappings', env.OUTPUT_MAPPINGS || '{}', 'object', {}),
    runtime_output_projections: runtimeOutputProjections,
    component_contracts: componentContracts,
    ...(runtimeTask ? { runtime_task: runtimeTask } : {}),
    ...(Object.keys(runtimeExecution).length > 0 ? { runtime_execution: runtimeExecution } : {}),
    ability_tools: parseJsonInput('ability_tools', env.ABILITY_TOOLS || '[]', 'array', []),
    ability_requirements: parseJsonInput('ability_requirements', env.ABILITY_REQUIREMENTS || '[]', 'array', []),
    evidence_projections: evidenceProjections,
    tool_recorders: parseJsonInput('tool_recorders', env.TOOL_RECORDERS || '[]', 'array', []),
    runner_workspace: effectiveRunnerWorkspace,
    ignored_workspace_paths: parseJsonInput('ignored_workspace_paths', env.IGNORED_WORKSPACE_PATHS || '[]', 'array', []),
    callback_data: parseJsonInput('callback_data', env.CALLBACK_DATA || '{}', 'object', {}),
    rules: parseJsonInput('rules', env.RULES || '{}', 'object', {}),
    general_rules: parseJsonInput('general_rules', env.GENERAL_RULES || '[]', 'array', []),
    task_rules: parseJsonInput('task_rules', env.TASK_RULES || '[]', 'array', []),
    probes: parseJsonInput('probes', env.PROBES || '{}', 'object', {}),
    artifact_export: {
      enabled: true,
      only_when_no_pr: true,
      repo: targetRepo,
      path_prefix: env.ARTIFACT_EXPORT_PATH_PREFIX || '',
      include_job_artifacts: false,
      branch_template: 'agent-artifacts/{workload_id}-{run_id}-{provider}-{model}-{job_id}',
      commit_message_template: 'chore: persist {type} artifact',
      pr_title_template: '[{workload_id}] {task_id} - {model_label} - {result_label}',
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
      OPENAI_API_KEY: env.OPENAI_API_KEY || '',
      ...providerBenchEnv,
    },
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

function runtimeTaskFromEnv(env) {
  return runtimeAgentCiTaskFromRequest(
    parseJsonInput('runtime_task', env.RUNTIME_TASK || '{}', 'object', {}),
    parseJsonInput('ability_request', env.ABILITY_REQUEST || '{}', 'object', {}),
    parseJsonInput('ability_input', env.ABILITY_INPUT || '{}', 'object', {})
  );
}

function isCodeboxRuntime(runtime) {
  return runtime?.id === 'wp-codebox' || runtime?.executor?.backend === 'codebox';
}

function runtimeProfilePayload(runtime, options) {
  if (!isCodeboxRuntime(runtime)) {
    return options.profile;
  }
  const { codeboxRuntimeProfilePayload } = require('../../../agent-runtimes/wp-codebox/lib/codebox-runtime-profile');
  return codeboxRuntimeProfilePayload(options);
}

function runtimePathRequired(runtime, pathName) {
  const requiredPaths = runtime?.manifest?.ci_materialization?.required_paths;
  return isCodeboxRuntime(runtime) || (Array.isArray(requiredPaths) && requiredPaths.includes(pathName));
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

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}

module.exports = { buildConfig, isCodeboxRuntime, runtimePathRequired };
