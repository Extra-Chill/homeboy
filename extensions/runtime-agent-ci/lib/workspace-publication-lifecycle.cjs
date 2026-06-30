'use strict';

/* eslint-disable camelcase */

/**
 * External dependencies
 */
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const { loopGateSummary, withLoopGateResult } = require('./loop-lifecycle.cjs');

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

function defaultRun(command, args, options = {}) {
  return spawnSync(command, args, {
    cwd: options.cwd,
    env: { ...(options.baseEnv || process.env), ...(options.env || {}) },
    encoding: 'utf8',
    maxBuffer: 1024 * 1024 * 20,
  });
}

function lifecycleHooks(hooks = {}) {
  const env = hooks.env || process.env;
  const run = typeof hooks.run === 'function'
    ? hooks.run
    : (command, args, options = {}) => defaultRun(command, args, { ...options, baseEnv: env });
  return {
    env,
    run,
    now: typeof hooks.now === 'function' ? hooks.now : () => Date.now(),
    hrtime: typeof hooks.hrtime === 'function' ? hooks.hrtime : () => process.hrtime.bigint(),
    tmpdir: hooks.tmpdir || env.RUNNER_TEMP || env.TMPDIR || '/tmp',
  };
}

function prepareRunnerCommand(command) {
  const trimmed = String(command || '').trim();
  return /^pnpm(\s|$)/.test(trimmed) ? `corepack ${trimmed}` : trimmed;
}

function runShellCommand(commandConfig, workspace, key, hooks = {}) {
  const adapter = lifecycleHooks(hooks);
  const started = adapter.hrtime();
  const preparedCommand = prepareRunnerCommand(commandConfig.command);
  const result = adapter.run('bash', ['-lc', preparedCommand], { cwd: workspace });
  const elapsedMs = Number(adapter.hrtime() - started) / 1000000;
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

function runCommandChecks(config, workspace, key, hooks = {}) {
  const commands = commandList(config, key);
  if (commands.length === 0) {
    return withLifecycleGateResult(key, { enabled: false, success: true, checks: [] });
  }

  const checks = [];
  for (const commandConfig of commands) {
    const check = runShellCommand(commandConfig, workspace, key, hooks);
    checks.push(check);
    if (!check.success) {
      const failed = {
        enabled: true,
        success: false,
        workspace,
        checks,
        error: check.error || `${key} failed: ${commandConfig.command}`,
      };
      return withLifecycleGateResult(key, failed);
    }
  }

  return withLifecycleGateResult(key, { enabled: true, success: true, workspace, checks });
}

function hasCommandChecks(config, key) {
  return commandList(config, key).length > 0;
}

function withLifecycleGateResult(id, result) {
  return withLoopGateResult(id, result);
}

function git(workspace, args, options = {}) {
  const adapter = lifecycleHooks(options.hooks || {});
  const result = adapter.run('git', args, { cwd: workspace, env: options.env });
  if (options.check !== false && result.status !== 0) {
    throw new Error((result.stderr || result.stdout || `git ${args.join(' ')} failed`).trim());
  }
  return result;
}

function gh(workspace, args, options = {}) {
  const adapter = lifecycleHooks(options.hooks || {});
  const result = adapter.run('gh', args, { cwd: workspace, env: options.env });
  if (options.check !== false && result.status !== 0) {
    throw new Error((result.stderr || result.stdout || `gh ${args.join(' ')} failed`).trim());
  }
  return result;
}

function ignoredWorkspacePathPrefixes(config = {}) {
  let configured = [];
  if (Array.isArray(config.ignored_workspace_paths)) {
    configured = config.ignored_workspace_paths;
  } else if (Array.isArray(config.ignoredWorkspacePaths)) {
    configured = config.ignoredWorkspacePaths;
  }
  return ['.ci', 'runtime-agent-artifacts', ...configured]
    .map(normalizePathPattern)
    .filter(Boolean)
    .map((entry) => (entry.endsWith('/') ? entry : `${entry}/`));
}

function changedFiles(workspace, config = {}, hooks = {}) {
  const status = git(workspace, ['status', '--porcelain', '--untracked-files=all'], { hooks }).stdout || '';
  const ignoredPrefixes = ignoredWorkspacePathPrefixes(config);
  return status.split('\n')
    .filter(Boolean)
    .map((line) => line.slice(3).trim())
    .filter((file) => file && !ignoredPrefixes.some((prefix) => file.startsWith(prefix)));
}

function normalizePathPattern(value) {
  return String(value || '').trim().replace(/^\.\//, '').replace(/^\/+/, '').replace(/\\/g, '/');
}

function writablePathPatterns(config) {
  const paths = Array.isArray(config.writable_paths) ? config.writable_paths : [];
  return paths.map(normalizePathPattern).filter(Boolean);
}

function globPatternToRegExp(pattern) {
  let source = '^';
  const normalized = normalizePathPattern(pattern);
  for (let index = 0; index < normalized.length; index += 1) {
    const char = normalized[index];
    const next = normalized[index + 1];
    if (char === '*' && next === '*') {
      if (normalized[index + 2] === '/') {
        source += '(?:.*/)?';
        index += 2;
      } else {
        source += '.*';
        index += 1;
      }
    } else if (char === '*') {
      source += '[^/]*';
    } else if (char === '?') {
      source += '[^/]';
    } else {
      source += char.replace(/[|\\{}()[\]^$+?.]/g, '\\$&');
    }
  }
  source += '$';
  return new RegExp(source);
}

function validateWritablePaths(config, files) {
  const patterns = writablePathPatterns(config);
  if (patterns.length === 0) {
    return withLifecycleGateResult('writable_paths', { enabled: false, patterns: [], rejected_files: [] });
  }

  const matchers = patterns.map(globPatternToRegExp);
  const rejected = files
    .map((file) => normalizePathPattern(file))
    .filter((file) => file && !matchers.some((matcher) => matcher.test(file)));
  const success = rejected.length === 0;
  return withLifecycleGateResult('writable_paths', {
    enabled: true,
    success,
    patterns,
    rejected_files: rejected,
    error: success ? '' : `Changed files outside writable_paths: ${rejected.join(', ')}`,
  });
}

function declaredSideEffectPatterns(config) {
  const lifecycle = plainObject(config.workspace_lifecycle) ? config.workspace_lifecycle : {};
  const artifactExport = plainObject(config.artifact_export) ? config.artifact_export : {};
  const configured = [
    lifecycle.side_effect_paths,
    lifecycle.declared_side_effect_paths,
    config.declared_side_effect_paths,
    config.verification_side_effect_paths,
    config.allowed_side_effect_paths,
    artifactExport.side_effect_paths,
  ].find(Array.isArray) || [];
  return configured.map(normalizePathPattern).filter(Boolean);
}

function evaluateSideEffectPolicy(config, files) {
  const patterns = declaredSideEffectPatterns(config);
  const normalizedFiles = files.map(normalizePathPattern).filter(Boolean);
  if (normalizedFiles.length === 0) {
    return withLifecycleGateResult('side_effect_policy', { enabled: patterns.length > 0, success: true, patterns, files: [], accepted_files: [], rejected_files: [] });
  }

  const matchers = patterns.map(globPatternToRegExp);
  const accepted = normalizedFiles.filter((file) => matchers.some((matcher) => matcher.test(file)));
  const rejected = normalizedFiles.filter((file) => !accepted.includes(file));
  const success = rejected.length === 0;
  return withLifecycleGateResult('side_effect_policy', {
    enabled: true,
    success,
    patterns,
    files: normalizedFiles,
    accepted_files: accepted,
    rejected_files: rejected,
    error: success ? '' : `Verification side-effect files outside declared policy: ${rejected.join(', ')}`,
  });
}

function calculatePublishSet(agentFiles, sideEffectPolicy) {
  const files = new Set(agentFiles.map(normalizePathPattern).filter(Boolean));
  for (const file of sideEffectPolicy.accepted_files || []) {
    files.add(normalizePathPattern(file));
  }
  return Array.from(files);
}

function workspaceContractConfig(config) {
  return plainObject(config.workspace_contract_checks) ? config.workspace_contract_checks : {};
}

function contractPathEntries(config, key) {
  const checks = Array.isArray(config[key]) ? config[key] : [];
  return checks.flatMap((entry) => {
    const checkPath = typeof entry === 'string' ? entry : entry?.path;
    if (typeof checkPath !== 'string' || checkPath.trim() === '') {
      return [];
    }
    return [{
      path: normalizePathPattern(checkPath),
      description: typeof entry?.description === 'string' ? entry.description.trim() : '',
    }];
  }).filter((entry) => entry.path);
}

function contractGlobEntries(config) {
  const checks = Array.isArray(config.glob_min_count) ? config.glob_min_count : [];
  return checks.flatMap((entry) => {
    if (!plainObject(entry)) {
      return [];
    }
    const glob = typeof entry.glob === 'string' ? normalizePathPattern(entry.glob) : '';
    const min = Number(entry.min);
    if (!glob || !Number.isInteger(min) || min < 0) {
      return [];
    }
    return [{
      glob,
      min,
      description: typeof entry.description === 'string' ? entry.description.trim() : '',
    }];
  });
}

function contractEntryPointEntries(config) {
  const checks = Array.isArray(config.entry_points) ? config.entry_points : [];
  return checks.flatMap((entry) => {
    if (!plainObject(entry)) {
      return [];
    }
    let entryPath = entry.entry;
    if (typeof entry.path === 'string') {
      entryPath = entry.path;
    } else if (typeof entry.entry_path === 'string') {
      entryPath = entry.entry_path;
    }
    const mustLinkTo = Array.isArray(entry.must_link_to) ? entry.must_link_to : [entry.must_link_to];
    const targets = mustLinkTo
      .map((target) => normalizePathPattern(target))
      .filter(Boolean);
    if (typeof entryPath !== 'string' || entryPath.trim() === '' || targets.length === 0) {
      return [];
    }
    return [{
      path: normalizePathPattern(entryPath),
      must_link_to: targets,
      description: typeof entry.description === 'string' ? entry.description.trim() : '',
    }];
  }).filter((entry) => entry.path);
}

function contractForbiddenPhraseEntries(config) {
  const checks = Array.isArray(config.forbidden_phrases) ? config.forbidden_phrases : [];
  return checks.flatMap((entry) => {
    const phrase = typeof entry === 'string' ? entry : entry?.phrase;
    if (typeof phrase !== 'string' || phrase.trim() === '') {
      return [];
    }
    return [{
      phrase: phrase.trim(),
      description: typeof entry?.description === 'string' ? entry.description.trim() : '',
    }];
  });
}

function safeWorkspacePath(workspace, relativePath) {
  const normalized = normalizePathPattern(relativePath);
  if (!normalized || normalized.split('/').includes('..')) {
    return null;
  }
  const resolved = path.resolve(workspace, normalized);
  const workspaceRoot = path.resolve(workspace);
  if (resolved !== workspaceRoot && !resolved.startsWith(`${workspaceRoot}${path.sep}`)) {
    return null;
  }
  return resolved;
}

function workspaceFileList(workspace, config = {}) {
  const files = [];
  const excludedDirectories = new Set(['.git']);
  for (const prefix of ignoredWorkspacePathPrefixes(config)) {
    const segment = prefix.replace(/\/$/, '').split('/')[0];
    if (segment) {
      excludedDirectories.add(segment);
    }
  }
  function visit(directory, prefix = '') {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      if (entry.isDirectory() && excludedDirectories.has(entry.name)) {
        continue;
      }
      const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
      const fullPath = path.join(directory, entry.name);
      if (entry.isDirectory()) {
        visit(fullPath, relative);
      } else if (entry.isFile()) {
        files.push(normalizePathPattern(relative));
      }
    }
  }
  visit(workspace);
  return files;
}

function isWorkspaceTextFile(file) {
  return /\.(md|markdown|mdx|txt|text)$/i.test(file);
}

function scopedContractFiles(config, contract, workspace, workspaceFiles, pathChecks, globChecks, entryChecks) {
  const explicitScope = Array.isArray(contract.scope) ? contract.scope : [];
  const scopePatterns = [
    ...explicitScope.map(normalizePathPattern),
    ...writablePathPatterns(config),
    ...globChecks.map((entry) => entry.glob),
  ].filter(Boolean);
  const scopeMatchers = scopePatterns.map(globPatternToRegExp);
  const explicitFiles = new Set([
    ...pathChecks.map((entry) => entry.path),
    ...entryChecks.map((entry) => entry.path),
  ].filter(Boolean));

  return workspaceFiles.filter((file) => {
    if (!isWorkspaceTextFile(file)) {
      return false;
    }
    if (scopeMatchers.length === 0 && explicitFiles.size === 0) {
      return true;
    }
    if (explicitFiles.has(file)) {
      return true;
    }
    return scopeMatchers.some((matcher) => matcher.test(file));
  }).filter((file) => Boolean(safeWorkspacePath(workspace, file)));
}

function entryPointLinksTo(content, target, entryPath) {
  const normalized = normalizePathPattern(target);
  const relativeFromEntry = normalizePathPattern(path.posix.relative(path.posix.dirname(entryPath), normalized));
  const candidates = new Set([
    target,
    normalized,
    `./${normalized}`,
    relativeFromEntry,
    `./${relativeFromEntry}`,
    encodeURI(normalized),
    `./${encodeURI(normalized)}`,
    encodeURI(relativeFromEntry),
    `./${encodeURI(relativeFromEntry)}`,
  ].filter(Boolean));
  for (const candidate of candidates) {
    if (content.includes(candidate)) {
      return true;
    }
  }
  return false;
}

function evaluateEntryPoints(entries, workspace) {
  return entries.map((entry) => {
    const resolved = safeWorkspacePath(workspace, entry.path);
    const exists = Boolean(resolved && fs.existsSync(resolved) && fs.statSync(resolved).isFile());
    const content = exists ? fs.readFileSync(resolved, 'utf8') : '';
    const missing_targets = exists
      ? entry.must_link_to.filter((target) => !entryPointLinksTo(content, target, entry.path))
      : [...entry.must_link_to];
    return {
      ...entry,
      exists,
      missing_targets,
      success: exists && missing_targets.length === 0,
    };
  });
}

function evaluateForbiddenPhrases(entries, workspace, scopedFiles) {
  return entries.map((entry) => {
    const matching_files = [];
    for (const file of scopedFiles) {
      const resolved = safeWorkspacePath(workspace, file);
      if (!resolved || !fs.existsSync(resolved) || !fs.statSync(resolved).isFile()) {
        continue;
      }
      const content = fs.readFileSync(resolved, 'utf8');
      if (content.includes(entry.phrase)) {
        matching_files.push(file);
      }
    }
    return {
      ...entry,
      matching_files,
      success: matching_files.length === 0,
    };
  });
}

function evaluateWorkspaceContract(config, workspace) {
  const contract = workspaceContractConfig(config);
  const pathChecks = contractPathEntries(contract, 'paths_exist');
  const globChecks = contractGlobEntries(contract);
  const entryChecks = contractEntryPointEntries(contract);
  const forbiddenPhraseChecks = contractForbiddenPhraseEntries(contract);
  if (pathChecks.length === 0 && globChecks.length === 0 && entryChecks.length === 0 && forbiddenPhraseChecks.length === 0) {
    return withLifecycleGateResult('workspace_contract_checks', { enabled: false, success: true, paths_exist: [], glob_min_count: [], entry_points: [], forbidden_phrases: [] });
  }

  const pathsExist = pathChecks.map((entry) => {
    const resolved = safeWorkspacePath(workspace, entry.path);
    const exists = Boolean(resolved && fs.existsSync(resolved));
    return { ...entry, exists, success: exists };
  });
  const needsWorkspaceFiles = globChecks.length > 0 || forbiddenPhraseChecks.length > 0;
  const workspaceFiles = needsWorkspaceFiles ? workspaceFileList(workspace, config) : [];
  const globMinCount = globChecks.map((entry) => {
    const matcher = globPatternToRegExp(entry.glob);
    const matches = workspaceFiles.filter((file) => matcher.test(file));
    return {
      ...entry,
      count: matches.length,
      matches,
      success: matches.length >= entry.min,
    };
  });
  const entryPoints = evaluateEntryPoints(entryChecks, workspace);
  const forbiddenPhraseFiles = forbiddenPhraseChecks.length > 0
    ? scopedContractFiles(config, contract, workspace, workspaceFiles, pathChecks, globChecks, entryChecks)
    : [];
  const forbiddenPhrases = evaluateForbiddenPhrases(forbiddenPhraseChecks, workspace, forbiddenPhraseFiles);
  const failures = [
    ...pathsExist.filter((entry) => !entry.success).map((entry) => `missing path ${entry.path}`),
    ...globMinCount.filter((entry) => !entry.success).map((entry) => `glob ${entry.glob} matched ${entry.count}, expected at least ${entry.min}`),
    ...entryPoints.filter((entry) => !entry.success).map((entry) => `entry_points ${entry.path} missing links: ${entry.missing_targets.join(', ')}`),
    ...forbiddenPhrases.filter((entry) => !entry.success).map((entry) => `forbidden phrase ${JSON.stringify(entry.phrase)} found in ${entry.matching_files.join(', ')}`),
  ];
  const success = failures.length === 0;
  return withLifecycleGateResult('workspace_contract_checks', {
    enabled: true,
    success,
    paths_exist: pathsExist,
    glob_min_count: globMinCount,
    entry_points: entryPoints,
    forbidden_phrases: forbiddenPhrases,
    forbidden_phrase_files: forbiddenPhraseFiles,
    error: success ? '' : `workspace_contract_checks failed: ${failures.join('; ')}`,
  });
}

function differenceFiles(files, baseline) {
  const baselineSet = new Set(baseline.map(normalizePathPattern));
  return files
    .map((file) => normalizePathPattern(file))
    .filter((file) => file && !baselineSet.has(file));
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

function publicationBase(config, hooks = {}) {
  const adapter = lifecycleHooks(hooks);
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
      || adapter.env.GITHUB_BASE_REF
      || 'main',
  ) || 'main';
}

function publicationTemplates(config, values, hooks = {}) {
  const artifactExport = plainObject(config.artifact_export) ? config.artifact_export : {};
  const workspaceConfig = plainObject(config.runner_workspace) ? config.runner_workspace : {};
  return {
    branch: renderTemplate(
      workspaceConfig.branch || artifactExport.branch_template || 'agent-artifacts/{agent_slug}-{run_id}',
      values,
    ),
    commitMessage: renderTemplate(
      workspaceConfig.commit_message || artifactExport.commit_message_template || 'chore: persist runtime agent workspace changes',
      values,
    ),
    title: renderTemplate(
      artifactExport.pr_title_template || 'Persist runtime agent workspace changes',
      values,
    ),
    body: renderTemplate(
      artifactExport.pr_body_template || '## Result\n\nRuntime agent workspace changes are ready for review.\n',
      values,
    ),
    base: publicationBase(config, hooks),
  };
}

function copyFile(source, destination) {
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  fs.copyFileSync(source, destination);
}

function preserveWorkspaceFiles(workspace, files, hooks = {}) {
  const adapter = lifecycleHooks(hooks);
  const temp = fs.mkdtempSync(path.join(
    adapter.tmpdir,
    'homeboy-runner-workspace.',
  ));
  const entries = [];
  for (const file of files) {
    const source = path.join(workspace, file);
    const backup = path.join(temp, file);
    if (fs.existsSync(source) && fs.statSync(source).isFile()) {
      copyFile(source, backup);
      entries.push({ file, backup, deleted: false });
    } else {
      entries.push({ file, deleted: true });
    }
  }
  return { temp, entries };
}

function restoreWorkspaceFiles(workspace, preserved) {
  for (const entry of preserved.entries) {
    const target = path.join(workspace, entry.file);
    if (entry.deleted) {
      fs.rmSync(target, { force: true });
      continue;
    }
    copyFile(entry.backup, target);
  }
  fs.rmSync(preserved.temp, { recursive: true, force: true });
}

function resetPublicationBranch(workspace, branch, base, files, hooks = {}) {
  const preserved = preserveWorkspaceFiles(workspace, files, hooks);
  const fetch = git(
    workspace,
    ['fetch', 'origin', `+refs/heads/${base}:refs/remotes/origin/${base}`],
    { check: false, hooks },
  );
  const baseRef = fetch.status === 0 ? `origin/${base}` : base;
  git(workspace, ['reset', '--hard', 'HEAD'], { hooks });
  git(workspace, ['clean', '-fd'], { hooks });
  git(workspace, ['checkout', '-B', branch, baseRef], { hooks });
  restoreWorkspaceFiles(workspace, preserved);
}

function pushWorkspaceBranch(workspace, branch, hooks = {}) {
  const fetch = git(workspace, ['fetch', 'origin', `+refs/heads/${branch}:refs/remotes/origin/${branch}`], { check: false, hooks });
  const args = ['push', '-u', 'origin', `HEAD:${branch}`];
  if (fetch.status === 0) {
    args.splice(1, 0, `--force-with-lease=refs/heads/${branch}`);
  }
  git(workspace, args, { hooks });
}

function pushNewWorkspaceBranch(workspace, branch, hooks = {}) {
  git(workspace, ['push', '-u', 'origin', `HEAD:${branch}`], { hooks });
}

function pullRequestForBranch(workspace, branch, hooks = {}) {
  const result = gh(workspace, ['pr', 'view', branch, '--json', 'number,state,url', '--jq', '.'], { check: false, hooks });
  if (result.status !== 0) {
    return null;
  }
  try {
    return JSON.parse(result.stdout || '{}');
  } catch {
    return null;
  }
}

function createPullRequest(workspace, branch, templates, hooks = {}) {
  return gh(workspace, [
    'pr', 'create',
    '--head', branch,
    '--base', templates.base,
    '--title', templates.title,
    '--body', templates.body,
  ], { hooks }).stdout.trim();
}

function replacementBranchName(branch, hooks = {}) {
  const adapter = lifecycleHooks(hooks);
  const suffix = adapter.env.GITHUB_RUN_ID || adapter.now();
  return `${branch}-run-${suffix}`.replace(/[^A-Za-z0-9._/-]+/g, '-').replace(/^-+|-+$/g, '');
}

function publicationEvidenceRef(input = {}) {
  return {
    type: input.url ? 'pull_request' : 'branch',
    provider: 'github',
    repo: input.repo || '',
    head: input.head || input.branch || '',
    base: input.base || '',
    url: input.url || '',
    action: input.action || '',
    pr_number: input.pr_number ?? input.number ?? null,
    pr_state: input.pr_state || input.state || '',
    files: Array.isArray(input.files) ? input.files : [],
  };
}

function preparePublication(config, results, scenario, files, hooks = {}) {
  const adapter = lifecycleHooks(hooks);
  const metadata = scenario.metadata || {};
  const values = {
    agent_slug: config.agent_slug || 'runtime-agent',
    run_id: adapter.env.GITHUB_RUN_ID || metadata.run_id || metadata.job_id || 'run',
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
  const templates = publicationTemplates(config, values, hooks);
  const branch = templates.branch.replace(/[^A-Za-z0-9._/-]+/g, '-').replace(/^-+|-+$/g, '') || `agent-artifacts/${values.agent_slug}-${values.run_id}`;
  const repo = config.target_repo || adapter.env.GITHUB_REPOSITORY || '';
  const publication_evidence_ref = publicationEvidenceRef({
    repo,
    head: branch,
    base: templates.base,
    files,
  });

  return {
    changed: files.length > 0,
    dry_run: adapter.env.HOMEBOY_HOST_LIFECYCLE_DRY_RUN === '1' || Boolean(config.dry_run),
    values,
    templates,
    branch,
    base: templates.base,
    repo,
    files,
    publication_evidence_ref,
  };
}

function captureWorkspaceDelta(config, workspace, publication, hooks = {}) {
  resetPublicationBranch(workspace, publication.branch, publication.base, publication.files, hooks);
  const files = changedFiles(workspace, config, hooks);
  git(workspace, ['add', '--', ...files], { hooks });
  const staged = git(workspace, ['diff', '--cached', '--name-only'], { check: false, hooks }).stdout.trim().split('\n').filter(Boolean);
  return {
    changed: staged.length > 0,
    workspace,
    files,
    staged,
    publication_evidence_ref: publicationEvidenceRef({
      repo: publication.repo,
      head: publication.branch,
      base: publication.base,
      files: staged,
    }),
  };
}

function publishRevision(workspace, publication, delta, hooks = {}) {
  if (!delta.changed) {
    return {
      changed: false,
      head: publication.branch,
      files: [],
      publication_evidence_ref: publicationEvidenceRef({
        repo: publication.repo,
        head: publication.branch,
        base: publication.base,
        files: [],
      }),
    };
  }

  git(workspace, ['config', 'user.name', 'homeboy-agent-ci'], { hooks });
  git(workspace, ['config', 'user.email', '41898282+github-actions[bot]@users.noreply.github.com'], { hooks });
  git(workspace, ['commit', '-m', publication.templates.commitMessage], { hooks });
  pushWorkspaceBranch(workspace, publication.branch, hooks);

  return {
    changed: true,
    head: publication.branch,
    files: delta.staged,
    publication_evidence_ref: publicationEvidenceRef({
      repo: publication.repo,
      head: publication.branch,
      base: publication.base,
      files: delta.staged,
    }),
  };
}

function finalizationGateArgs(lifecycle = {}) {
  return [
    lifecycle.verification?.gate_result,
    lifecycle.drift?.gate_result,
    lifecycle.sideEffectPolicy?.gate_result,
    lifecycle.writablePaths?.gate_result,
    lifecycle.workspaceContract?.gate_result,
    ...(Array.isArray(lifecycle.gate_results) ? lifecycle.gate_results : []),
  ].filter((gate) => gate && gate.enabled !== false && gate.status === 'passed').map((gate) => {
    const name = String(gate.id || gate.name || gate.label || 'workspace_lifecycle').replace(/[^A-Za-z0-9_.-]+/g, '_');
    const detail = String(gate.message || gate.reason || '').replace(/\n/g, ' ').trim();
    return detail ? `${name}=passed:${detail}` : `${name}=passed`;
  });
}

function parseHomeboyJsonOutput(stdout) {
  const text = String(stdout || '').trim();
  if (!text) {
    return null;
  }
  const parsed = JSON.parse(text);
  return plainObject(parsed?.data) ? parsed.data : parsed;
}

function finalizeWorkspaceReview(config, workspace, publication, delta, lifecycle = {}, hooks = {}) {
  const gateArgs = finalizationGateArgs({
    ...lifecycle,
    gate_results: [
      ...(Array.isArray(lifecycle.gate_results) ? lifecycle.gate_results : []),
      ...(Array.isArray(config.finalization_gate_results) ? config.finalization_gate_results : []),
    ],
  });
  if (gateArgs.length === 0) {
    throw new Error('Homeboy agent-task finalize-pr requires at least one passed deterministic gate_result; no eligible lifecycle gates were available');
  }

  const adapter = lifecycleHooks(hooks);
  const homeboyBin = config.homeboy_bin || config.homeboyBin || adapter.env.HOMEBOY_BIN || 'homeboy';
  const metadata = lifecycle.scenario?.metadata || {};
  const runId = publication.values.run_id || metadata.run_id || metadata.job_id || 'run';
  const verificationCommands = commandList(config, 'verification_commands').map((entry) => entry.command);
  const args = [
    'agent-task', 'finalize-pr',
    '--run-id', runId,
    '--path', workspace,
    '--base', publication.base,
    '--head', publication.branch,
    '--title', publication.templates.title,
    '--commit-message', publication.templates.commitMessage,
    '--attempt-summary', lifecycle.gateSummary?.reason || 'green deterministic workspace lifecycle gates completed',
    '--ai-tool', config.ai_tool || config.aiTool || 'OpenCode',
    '--ai-model', config.ai_model || config.aiModel || 'not recorded',
    '--ci-expected', config.ci_expected || config.ciExpected || 'Homeboy CI after push',
    '--ai-used-for', config.ai_used_for || config.aiUsedFor || 'Drafted implementation and tests; Chris reviews and owns the change.',
  ];
  for (const gateArg of gateArgs) {
    args.push('--gate-result', gateArg);
  }
  for (const file of delta.staged) {
    args.push('--changed-file', file);
  }
  for (const command of verificationCommands) {
    args.push('--targeted-check-run', command);
  }
  for (const ref of config.source_refs || config.sourceRefs || []) {
    args.push('--source-ref', ref);
  }
  for (const ref of config.artifact_refs || config.artifactRefs || []) {
    args.push('--artifact-ref', ref);
  }
  for (const branch of config.protected_branches || config.protectedBranches || []) {
    args.push('--protected-branch', branch);
  }

  const result = adapter.run(homeboyBin, args, { cwd: workspace });
  if (result.status !== 0) {
    throw new Error((result.stderr || result.stdout || 'homeboy agent-task finalize-pr failed').trim());
  }
  const report = parseHomeboyJsonOutput(result.stdout) || {};
  const publicationProof = plainObject(report.publication_proof) ? report.publication_proof : {};
  const target = plainObject(publicationProof.target) ? publicationProof.target : {};
  const url = report.pr_url || publicationProof.adapter_ref || target.url || '';
  const action = report.pr_action || publicationProof.adapter_action || '';
  return {
    report,
    changed: Array.isArray(report.changed_files) ? report.changed_files.length > 0 : delta.changed,
    head: report.head || publication.branch,
    base: report.base || publication.base,
    url,
    action,
    pr_number: report.pr_number ?? null,
    pr_state: url ? 'OPEN' : '',
    files: Array.isArray(report.changed_files) ? report.changed_files : delta.staged,
    publication_intent: report.publication_intent || null,
    publication_proof: report.publication_proof || null,
  };
}

function updateReviewSummary(workspace, review, templates, hooks = {}) {
  if (!review?.number) {
    return { updated: false };
  }
  const result = gh(
    workspace,
    ['pr', 'edit', String(review.number), '--title', templates.title, '--body', templates.body],
    { check: false, hooks },
  );
  return { updated: result.status === 0, error: result.status === 0 ? '' : (result.stderr || result.stdout || '').trim() };
}

function ensureReviewRequest(workspace, branch, templates, hooks = {}) {
  const existing = pullRequestForBranch(workspace, branch, hooks);
  if (existing?.state === 'OPEN' && existing.url) {
    updateReviewSummary(workspace, { number: existing.number || branch }, templates, hooks);
    return { url: existing.url, action: 'updated', number: existing.number || null, state: 'OPEN', publication_evidence_ref: publicationEvidenceRef({ head: branch, base: templates.base, url: existing.url, action: 'updated', number: existing.number || null, state: 'OPEN' }) };
  }

  if (existing?.state === 'CLOSED' && existing.number) {
    const reopened = gh(workspace, ['pr', 'reopen', String(existing.number)], { check: false, hooks });
    if (reopened.status === 0) {
      updateReviewSummary(workspace, existing, templates, hooks);
      return { url: existing.url || '', action: 'reopened', head: branch, number: existing.number, state: 'OPEN', publication_evidence_ref: publicationEvidenceRef({ head: branch, base: templates.base, url: existing.url || '', action: 'reopened', number: existing.number, state: 'OPEN' }) };
    }

    const replacementBranch = replacementBranchName(branch, hooks);
    pushNewWorkspaceBranch(workspace, replacementBranch, hooks);
    const url = createPullRequest(workspace, replacementBranch, templates, hooks);
    return {
      url,
      action: 'created_after_closed_pr',
      head: replacementBranch,
      number: null,
      state: 'OPEN',
      closed_pr_number: existing.number,
      closed_pr_url: existing.url || '',
      reopen_error: (reopened.stderr || reopened.stdout || '').trim(),
      publication_evidence_ref: publicationEvidenceRef({ head: replacementBranch, base: templates.base, url, action: 'created_after_closed_pr', state: 'OPEN' }),
    };
  }

  const url = createPullRequest(workspace, branch, templates, hooks);
  return { url, action: 'created', head: branch, number: null, state: 'OPEN', publication_evidence_ref: publicationEvidenceRef({ head: branch, base: templates.base, url, action: 'created', state: 'OPEN' }) };
}

function ensurePullRequest(workspace, branch, templates, hooks = {}) {
  return ensureReviewRequest(workspace, branch, templates, hooks);
}

function publishWorkspace(config, results, scenario, workspace, files, hooks = {}, lifecycle = {}) {
  if (files.length === 0) {
    return { opened: false, changed: false };
  }

  const publication = preparePublication(config, results, scenario, files, hooks);
  if (publication.dry_run) {
    return { opened: false, changed: true, dry_run: true, head: publication.branch, files, publication_evidence_ref: publication.publication_evidence_ref };
  }

  const delta = captureWorkspaceDelta(config, workspace, publication, hooks);
  if (!delta.changed) {
    return { opened: false, changed: false, head: publication.branch, files: [], publication_evidence_ref: delta.publication_evidence_ref };
  }

  const finalization = finalizeWorkspaceReview(config, workspace, publication, delta, { ...lifecycle, scenario }, hooks);
  const url = finalization.url;
  const evidenceRef = publicationEvidenceRef({
    repo: publication.repo,
    head: finalization.head,
    base: publication.base,
    url,
    action: finalization.action,
    number: finalization.pr_number,
    state: finalization.pr_state,
    files: finalization.files,
  });


  return {
    opened: Boolean(url),
    changed: true,
    tool_name: 'host_runner_workspace_publication',
    source: 'host_runner_lifecycle',
    success: Boolean(url),
    repo: publication.repo,
    head: finalization.head,
    base: publication.base,
    url,
    action: finalization.action,
    pr_number: finalization.pr_number,
    pr_state: finalization.pr_state,
    files: finalization.files,
    publication_evidence_ref: evidenceRef,
    finalization: finalization.report,
    publication_intent: finalization.publication_intent,
    publication_proof: finalization.publication_proof,
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
  metadata.engine_data.runner_writable_path_policy = lifecycle.writablePaths;
  metadata.engine_data.runner_side_effect_policy = lifecycle.sideEffectPolicy;
  metadata.engine_data.runner_workspace_contract = lifecycle.workspaceContract;
  metadata.runner_workspace_capture = lifecycle.capture;
  metadata.runner_workspace_publication = lifecycle.publication;
  metadata.runner_writable_path_policy = lifecycle.writablePaths;
  metadata.runner_side_effect_policy = lifecycle.sideEffectPolicy;
  metadata.runner_workspace_contract = lifecycle.workspaceContract;
  metadata.verification_results = lifecycle.verification;
  metadata.drift_check_results = lifecycle.drift;
  scenario.metrics.verification_commands_succeeded = !lifecycle.verification.enabled || lifecycle.verification.success ? 1 : 0;
  scenario.metrics.drift_checks_succeeded = !lifecycle.drift.enabled || lifecycle.drift.success ? 1 : 0;
  scenario.metrics.writable_paths_satisfied = !lifecycle.writablePaths.enabled || lifecycle.writablePaths.success ? 1 : 0;
  scenario.metrics.side_effect_policy_satisfied = !lifecycle.sideEffectPolicy.enabled || lifecycle.sideEffectPolicy.success ? 1 : 0;
  scenario.metrics.workspace_contract_satisfied = !lifecycle.workspaceContract.enabled || lifecycle.workspaceContract.success ? 1 : 0;
  scenario.metrics.pr_opened = lifecycle.publication.opened ? 1 : 0;
  scenario.metrics.pr_opened_mean = scenario.metrics.pr_opened;
  scenario.metrics.file_written = lifecycle.capture.changed ? 1 : 0;
  scenario.metrics.file_written_mean = scenario.metrics.file_written;
  if (lifecycle.success && lifecycle.publication.opened) {
    metadata.success_status = 'pr_opened';
  }
  if (metadata.success_status) {
    metadata.engine_data.success_status = metadata.success_status;
    if (plainObject(metadata.engine_data.eval_artifact)) {
      metadata.engine_data.eval_artifact.run = plainObject(metadata.engine_data.eval_artifact.run)
        ? metadata.engine_data.eval_artifact.run
        : {};
      metadata.engine_data.eval_artifact.run.success_status = metadata.success_status;
    }
  }
  if (!lifecycle.success) {
    scenario.status = 'failed';
    scenario.summary = lifecycle.error;
    metadata.error_message = lifecycle.error;
  }
  results.status = lifecycle.success ? (results.status || 'completed') : 'failed';
}

function runDeterministicWorkspaceLifecycle(config, results, scenario, workspace, hooks = {}) {
  const agentFiles = changedFiles(workspace, config, hooks);
  const verification = runCommandChecks(config, workspace, 'verification_commands', hooks);
  if ((!verification.enabled || verification.success) && agentFiles.length > 0 && hasCommandChecks(config, 'drift_checks')) {
    git(workspace, ['add', '--', ...agentFiles], { hooks });
  }
  const drift = verification.enabled && !verification.success
    ? withLifecycleGateResult('drift_checks', { enabled: false, success: true, checks: [], skipped_reason: 'verification_commands_failed' })
    : runCommandChecks(config, workspace, 'drift_checks', hooks);
  const workspaceFiles = changedFiles(workspace, config, hooks);
  const verificationSideEffectFiles = differenceFiles(workspaceFiles, agentFiles);
  const sideEffectPolicy = evaluateSideEffectPolicy(config, verificationSideEffectFiles);
  const files = calculatePublishSet(agentFiles, sideEffectPolicy);
  const writablePaths = validateWritablePaths(config, files);
  const workspaceContract = evaluateWorkspaceContract(config, workspace);
  const capture = {
    enabled: true,
    changed: files.length > 0,
    files,
    agent_files: agentFiles,
    workspace,
    workspace_files: workspaceFiles,
    verification_side_effect_files: verificationSideEffectFiles,
  };
  let publication = { opened: false };
  let gateSummary = loopGateSummary([
    verification.gate_result,
    drift.gate_result,
    sideEffectPolicy.gate_result,
    writablePaths.gate_result,
    workspaceContract.gate_result,
  ]);
  let success = gateSummary.success;
  let error = gateSummary.error;

  if (!success) {
    if (verification.enabled && !verification.success) {
      error = verification.error || 'verification_commands failed';
    } else if (drift.enabled && !drift.success) {
      error = drift.error || 'drift_checks failed';
    } else if (sideEffectPolicy.enabled && !sideEffectPolicy.success) {
      error = sideEffectPolicy.error || 'side_effect_policy failed';
    } else if (workspaceContract.enabled && !workspaceContract.success) {
      error = workspaceContract.error || 'workspace_contract_checks failed';
    } else {
      error = writablePaths.error || 'writable_paths policy failed';
    }
  } else {
    try {
      publication = publishWorkspace(config, results, scenario, workspace, files, hooks, {
        verification,
        drift,
        sideEffectPolicy,
        writablePaths,
        workspaceContract,
        gateSummary,
        scenario,
      });
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

  gateSummary = loopGateSummary([
    verification.gate_result,
    drift.gate_result,
    sideEffectPolicy.gate_result,
    writablePaths.gate_result,
    workspaceContract.gate_result,
  ]);

  return { verification, drift, sideEffectPolicy, writablePaths, workspaceContract, gateSummary, capture, publication, success, error };
}

module.exports = {
  calculatePublishSet,
  captureWorkspaceDelta,
  changedFiles,
  evaluateSideEffectPolicy,
  evaluateWritablePaths: validateWritablePaths,
  evaluateWorkspaceContract,
  ensurePullRequest,
  ensureReviewRequest,
  finalizeWorkspaceReview,
  lifecycleHooks,
  preparePublication,
  publicationBase,
  publicationEvidenceRef,
  publicationTemplates,
  publishRevision,
  publishWorkspace,
  prepareRunnerCommand,
  recordLifecycle,
  replacementBranchName,
  runDeterministicWorkspaceLifecycle,
  runShellCommand,
  scenarioById,
  updateReviewSummary,
  validateWritablePaths,
};
