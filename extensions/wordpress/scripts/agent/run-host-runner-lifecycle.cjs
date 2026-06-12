#!/usr/bin/env node
'use strict';

/* eslint-disable no-console */

const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

function argValue(name, fallback = '') {
  const index = process.argv.indexOf(name);
  return index >= 0 && process.argv[index + 1] ? process.argv[index + 1] : fallback;
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function plainObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value);
}

function commandList(config, key) {
  const commands = Array.isArray(config[key]) ? config[key] : [];
  return commands.flatMap((entry) => {
    const command = typeof entry === 'string' ? entry : entry?.command;
    if (typeof command !== 'string' || command.trim() === '') {
      return [];
    }
    return [{
      command: command.trim(),
      description: typeof entry?.description === 'string' ? entry.description.trim() : '',
    }];
  });
}

function run(command, args, options = {}) {
  return spawnSync(command, args, {
    cwd: options.cwd,
    env: { ...process.env, ...(options.env || {}) },
    encoding: 'utf8',
    maxBuffer: 1024 * 1024 * 20,
  });
}

function prepareRunnerCommand(command) {
  const trimmed = String(command || '').trim();
  return /^pnpm(\s|$)/.test(trimmed) ? `corepack ${trimmed}` : trimmed;
}

function runShellCommand(commandConfig, workspace, key) {
  const started = process.hrtime.bigint();
  const preparedCommand = prepareRunnerCommand(commandConfig.command);
  const result = run('bash', ['-lc', preparedCommand], { cwd: workspace });
  const elapsedMs = Number(process.hrtime.bigint() - started) / 1000000;
  const exitCode = typeof result.status === 'number' ? result.status : 1;
  const stdout = (result.stdout || '').trim();
  const stderr = (result.stderr || '').trim();
  const spawnError = result.error ? result.error.message : '';
  const failureDetails = spawnError || stderr || stdout || `${key} exited ${exitCode}`;

  return {
    command: commandConfig.command,
    prepared_command: preparedCommand,
    executed_command: `bash -lc ${JSON.stringify(preparedCommand)}`,
    description: commandConfig.description,
    exit_code: exitCode,
    success: exitCode === 0,
    stdout,
    stderr,
    elapsed_ms: elapsedMs,
    workspace,
    error: exitCode === 0 ? spawnError : failureDetails,
  };
}

function runCommandChecks(config, workspace, key) {
  const commands = commandList(config, key);
  if (commands.length === 0) {
    return { enabled: false, checks: [] };
  }

  const checks = [];
  for (const commandConfig of commands) {
    const check = runShellCommand(commandConfig, workspace, key);
    checks.push(check);
    if (!check.success) {
      return {
        enabled: true,
        success: false,
        workspace,
        checks,
        error: check.error || `${key} failed: ${commandConfig.command}`,
      };
    }
  }

  return { enabled: true, success: true, workspace, checks };
}

function git(workspace, args, options = {}) {
  const result = run('git', args, { cwd: workspace, env: options.env });
  if (options.check !== false && result.status !== 0) {
    throw new Error((result.stderr || result.stdout || `git ${args.join(' ')} failed`).trim());
  }
  return result;
}

function gh(workspace, args, options = {}) {
  const result = run('gh', args, { cwd: workspace, env: options.env });
  if (options.check !== false && result.status !== 0) {
    throw new Error((result.stderr || result.stdout || `gh ${args.join(' ')} failed`).trim());
  }
  return result;
}

function changedFiles(workspace) {
  const status = git(workspace, ['status', '--porcelain', '--untracked-files=all']).stdout || '';
  return status.split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => line.slice(3).trim())
    .filter((file) => file && !file.startsWith('.ci/'));
}

function currentBranch(workspace) {
  const branch = (git(workspace, ['rev-parse', '--abbrev-ref', 'HEAD'], { check: false }).stdout || '').trim();
  return branch && branch !== 'HEAD' ? branch : '';
}

function renderTemplate(template, values) {
  return String(template || '').replace(/\{([^}]+)\}/g, (_, key) => {
    const value = values[key.trim()];
    return value === undefined || value === null ? '' : String(value);
  });
}

function normalizeBaseRef(value) {
  const ref = String(value || '').trim();
  if (!ref) {
    return '';
  }
  return ref
    .replace(/^refs\/heads\//, '')
    .replace(/^remotes\/origin\//, '')
    .replace(/^origin\//, '');
}

function publicationBase(config) {
  const artifactExport = plainObject(config.artifact_export) ? config.artifact_export : {};
  const workspaceConfig = plainObject(config.runner_workspace) ? config.runner_workspace : {};
  return normalizeBaseRef(
    workspaceConfig.base
      || workspaceConfig.base_branch
      || workspaceConfig.base_ref
      || workspaceConfig.from
      || artifactExport.base
      || artifactExport.base_branch
      || artifactExport.base_ref
      || process.env.GITHUB_BASE_REF
      || 'main',
  ) || 'main';
}

function publicationTemplates(config, values) {
  const artifactExport = plainObject(config.artifact_export) ? config.artifact_export : {};
  const workspaceConfig = plainObject(config.runner_workspace) ? config.runner_workspace : {};
  return {
    branch: renderTemplate(
      workspaceConfig.branch || artifactExport.branch_template || 'agent-artifacts/{agent_slug}-{run_id}',
      values,
    ),
    commitMessage: renderTemplate(
      workspaceConfig.commit_message || artifactExport.commit_message_template || 'chore: persist Data Machine agent workspace changes',
      values,
    ),
    title: renderTemplate(
      artifactExport.pr_title_template || 'Persist Data Machine agent workspace changes',
      values,
    ),
    body: renderTemplate(
      artifactExport.pr_body_template || '## Result\n\nData Machine agent workspace changes are ready for review.\n',
      values,
    ),
    base: publicationBase(config),
  };
}

function pushWorkspaceBranch(workspace, branch) {
  const fetch = git(workspace, ['fetch', 'origin', `+refs/heads/${branch}:refs/remotes/origin/${branch}`], { check: false });
  const args = ['push', '-u', 'origin', `HEAD:${branch}`];
  if (fetch.status === 0) {
    args.splice(1, 0, `--force-with-lease=refs/heads/${branch}`);
  }
  git(workspace, args);
}

function publishWorkspace(config, results, scenario, workspace, files) {
  if (files.length === 0) {
    return { opened: false, changed: false };
  }

  const metadata = scenario.metadata || {};
  const values = {
    agent_slug: config.agent_slug || 'datamachine-agent',
    run_id: process.env.GITHUB_RUN_ID || metadata.run_id || metadata.job_id || 'run',
    provider: config.provider || '',
    model: config.model || '',
    model_label: config.model || '',
    job_id: metadata.job_id || '',
    task_id: config.workload_id || scenario.id || '',
    result_label: 'workspace changes',
    result_table: '',
    checks_table: '',
    tools_table: '',
    links_table: '',
  };
  const templates = publicationTemplates(config, values);
  const branch = templates.branch.replace(/[^A-Za-z0-9._/-]+/g, '-').replace(/^-+|-+$/g, '') || `agent-artifacts/${values.agent_slug}-${values.run_id}`;

  if (process.env.HOMEBOY_HOST_LIFECYCLE_DRY_RUN === '1' || config.dry_run) {
    return { opened: false, changed: true, dry_run: true, head: branch, files };
  }

  if (currentBranch(workspace) !== branch) {
    git(workspace, ['checkout', '-B', branch]);
  }

  git(workspace, ['add', '--', ...files]);
  const staged = git(workspace, ['diff', '--cached', '--name-only'], { check: false }).stdout.trim().split('\n').filter(Boolean);
  if (staged.length === 0) {
    return { opened: false, changed: false, head: branch, files: [] };
  }

  git(workspace, ['config', 'user.name', 'homeboy-agent-ci']);
  git(workspace, ['config', 'user.email', '41898282+github-actions[bot]@users.noreply.github.com']);
  git(workspace, ['commit', '-m', templates.commitMessage]);
  pushWorkspaceBranch(workspace, branch);

  const existing = gh(workspace, ['pr', 'view', branch, '--json', 'url', '--jq', '.url'], { check: false });
  const existingUrl = existing.status === 0 ? existing.stdout.trim() : '';
  const url = existingUrl || gh(workspace, [
    'pr', 'create',
    '--head', branch,
    '--base', templates.base,
    '--title', templates.title,
    '--body', templates.body,
  ]).stdout.trim();

  return {
    opened: Boolean(url),
    changed: true,
    tool_name: 'host_runner_workspace_publication',
    source: 'host_runner_lifecycle',
    success: Boolean(url),
    repo: config.target_repo || process.env.GITHUB_REPOSITORY || '',
    head: branch,
    base: templates.base,
    url,
    files: staged,
  };
}

function scenarioById(results, scenarioId) {
  const scenarios = Array.isArray(results.scenarios) ? results.scenarios : [];
  return scenarios.find((item) => item && item.id === scenarioId) || scenarios[0];
}

function ensureMetadata(scenario) {
  scenario.metrics = plainObject(scenario.metrics) ? scenario.metrics : {};
  scenario.metadata = plainObject(scenario.metadata) ? scenario.metadata : {};
  scenario.metadata.engine_data = plainObject(scenario.metadata.engine_data) ? scenario.metadata.engine_data : {};
  return scenario.metadata;
}

function completionOutcomeSatisfied(scenario) {
  return Boolean(
    scenario?.metadata?.completion_outcome_satisfied
      || scenario?.metadata?.engine_data?.completion_outcome_satisfied
      || scenario?.metrics?.completion_outcome_satisfied,
  );
}

function recordLifecycle(results, scenario, lifecycle) {
  const metadata = ensureMetadata(scenario);
  metadata.engine_data.runner_verification_results = lifecycle.verification;
  metadata.engine_data.runner_drift_check_results = lifecycle.drift;
  metadata.engine_data.runner_workspace_capture = lifecycle.capture;
  metadata.engine_data.runner_workspace_publication = lifecycle.publication;
  metadata.runner_workspace_capture = lifecycle.capture;
  metadata.runner_workspace_publication = lifecycle.publication;
  metadata.verification_results = lifecycle.verification;
  metadata.drift_check_results = lifecycle.drift;
  scenario.metrics.verification_commands_succeeded = !lifecycle.verification.enabled || lifecycle.verification.success ? 1 : 0;
  scenario.metrics.drift_checks_succeeded = !lifecycle.drift.enabled || lifecycle.drift.success ? 1 : 0;
  scenario.metrics.pr_opened = lifecycle.publication.opened ? 1 : 0;
  scenario.metrics.file_written = lifecycle.capture.changed ? 1 : 0;
  if (!lifecycle.success) {
    scenario.status = 'failed';
    scenario.summary = lifecycle.error;
    metadata.error_message = lifecycle.error;
  }
  results.status = lifecycle.success ? (results.status || 'completed') : 'failed';
}

function main() {
  const resultsPath = argValue('--results');
  const configPath = argValue('--config');
  const scenarioId = argValue('--scenario');
  const workspace = path.resolve(argValue('--workspace', process.cwd()));
  if (!resultsPath || !configPath) {
    throw new Error('Usage: run-host-runner-lifecycle.cjs --results <path> --config <path> --scenario <id> [--workspace <path>]');
  }

  const results = readJson(resultsPath);
  const config = readJson(configPath);
  const scenario = scenarioById(results, scenarioId);
  if (!scenario) {
    throw new Error(`Scenario not found in results: ${scenarioId || '(first scenario)'}`);
  }

  const verification = runCommandChecks(config, workspace, 'verification_commands');
  const drift = verification.enabled && !verification.success
    ? { enabled: false, checks: [], skipped_reason: 'verification_commands_failed' }
    : runCommandChecks(config, workspace, 'drift_checks');
  const files = changedFiles(workspace);
  const capture = { enabled: true, changed: files.length > 0, files, workspace };
  let publication = { opened: false };
  let success = (!verification.enabled || verification.success) && (!drift.enabled || drift.success);
  let error = '';

  if (!success) {
    error = verification.enabled && !verification.success
      ? (verification.error || 'verification_commands failed')
      : (drift.error || 'drift_checks failed');
  } else {
    try {
      publication = publishWorkspace(config, results, scenario, workspace, files);
      if (capture.changed && !publication.opened && !publication.dry_run) {
        success = false;
        error = 'Agent wrote files without opening a pull request';
      } else if (config.success_requires_pr && !publication.opened && !completionOutcomeSatisfied(scenario)) {
        success = false;
        error = 'Agent completed without opening a pull request';
      }
    } catch (publicationError) {
      success = false;
      error = publicationError.message;
      publication = { opened: false, success: false, error };
    }
  }

  recordLifecycle(results, scenario, { verification, drift, capture, publication, success, error });
  writeJson(resultsPath, results);
  if (!success) {
    throw new Error(error);
  }
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}

module.exports = {
  prepareRunnerCommand,
  runShellCommand,
};
