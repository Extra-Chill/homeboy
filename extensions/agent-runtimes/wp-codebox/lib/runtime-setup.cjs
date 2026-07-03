'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { normalizeProviderPlugin } = require('../../../runtime-agent-ci/lib/full-run-inputs.cjs');
const { resolveDependencyTarget, runtimeDependencyEntries } = require('../../../runtime-agent-ci/lib/materialize-dependencies.cjs');

function setupRuntime({ phase, workspace, env = process.env, run }) {
  // After the runtime's build commands run, the wp-codebox CLI exists in the
  // checkout (packages/cli/dist/index.js, made executable by the build) but is
  // not on PATH and HOMEBOY_WP_CODEBOX_BIN is unset, so the downstream
  // "Build runner config" step reports the bin as missing. Point the env var at
  // the built CLI so subsequent steps resolve runtime.paths.runtime_bin. Runs
  // for the wp-codebox runtime regardless of the WordPress-dependency gate.
  if (phase === 'after_commands') {
    exportRuntimeBin(workspace, env);
  }
  if (!requiresWordPressDependencies(env)) {
    return;
  }
  if (phase === 'before_commands') {
    run('composer', ['install', '--no-interaction', '--no-progress', '--prefer-dist'], { cwd: path.join(workspace, '.ci/homeboy-extensions/wordpress') });
    run('npm', ['install'], { cwd: path.join(workspace, '.ci/homeboy-extensions/wordpress') });
    return;
  }
  if (phase === 'after_commands') {
    installCheckedOutPhpDependencies({ workspace, env, run });
  }
}

function installCheckedOutPhpDependencies({ workspace, env = process.env, run }) {
  for (const dependencyDir of checkedOutPhpDependencyDirs({ workspace, env })) {
    run('composer', ['install', '--no-interaction', '--no-progress', '--prefer-dist'], { cwd: dependencyDir });
  }
}

function checkedOutPhpDependencyDirs({ workspace, env = process.env }) {
  const dirs = [];
  for (const entry of runtimeDependencyEntries(env.RUNTIME_DEPENDENCIES || '')) {
    const dir = checkedOutDependencyDir(entry, workspace);
    if (dir && fs.existsSync(path.join(dir, 'composer.json'))) {
      dirs.push(dir);
    }
  }

  const providerPlugin = normalizeProviderPlugin(env.PROVIDER_PLUGIN || '{}', env.PROVIDER || '', false);
  if (providerPlugin.repo) {
    const providerPluginDir = safeDependencySubdir(path.join('.ci', providerPlugin.repo.split('/')[1]), providerPlugin.path || '.', workspace);
    if (fs.existsSync(path.join(providerPluginDir, 'composer.json'))) {
      dirs.push(providerPluginDir);
    }
  }

  return [...new Set(dirs)];
}

function checkedOutDependencyDir(rawEntry, workspace) {
  if (rawEntry && !Array.isArray(rawEntry) && typeof rawEntry === 'object') {
    if (!rawEntry.repo) {
      return '';
    }
    return safeDependencySubdir(rawEntry.target || path.join('.ci', rawEntry.repo.split('/')[1]), rawEntry.path || '.', workspace);
  }
  const entry = String(rawEntry || '').trim();
  if (!entry) {
    return '';
  }
  const repo = entry.includes('@') ? entry.slice(0, entry.lastIndexOf('@')) : entry;
  return resolveDependencyTarget(path.join('.ci', repo.split('/')[1]), workspace).targetPath;
}

function safeDependencySubdir(target, subdir, workspace) {
  const targetPath = resolveDependencyTarget(target, workspace).targetPath;
  const resolved = path.resolve(targetPath, subdir);
  const relative = path.relative(targetPath, resolved);
  if (relative.startsWith('..') || path.isAbsolute(relative)) {
    throw new Error(`Dependency subdirectory must resolve inside dependency checkout: ${subdir}`);
  }
  return resolved;
}

function exportRuntimeBin(workspace, env = process.env) {
  exportRuntimePath(
    workspace,
    env,
    ['HOMEBOY_WP_CODEBOX_BIN', 'WP_CODEBOX_BIN'],
    '.ci/wp-codebox/packages/cli/dist/index.js',
    'HOMEBOY_WP_CODEBOX_BIN'
  );
  // The runtime contract source (wp-codebox-runtime-contract-source.js) requires
  // @automattic/wp-codebox-core/contracts, which is the runtime-core workspace
  // package — not resolvable from the homeboy-extensions checkout. Point at the
  // built contracts module so "Build runner config" can load the canonical
  // runtime contract manifest.
  exportRuntimePath(
    workspace,
    env,
    ['HOMEBOY_WP_CODEBOX_CORE_MODULE', 'WP_CODEBOX_CORE_MODULE'],
    '.ci/wp-codebox/packages/runtime-core/dist/contracts.js',
    'HOMEBOY_WP_CODEBOX_CORE_MODULE'
  );
}

function exportRuntimePath(workspace, env, existingEnvNames, relativePath, exportName) {
  const resolvedPath = path.join(workspace, relativePath);
  // A checked-out runtime build is the deterministic contract for this job. Keep
  // an explicit value only when it already points inside the materialized
  // workspace; stale runner-global env must not outrank the fresh checkout.
  if (existingEnvNames.some((name) => envPathIsInsideWorkspace(env[name], workspace))) {
    return;
  }
  if (!fs.existsSync(resolvedPath)) {
    // Build did not produce the artifact; let the downstream check surface a
    // clear, accurate error rather than a wrong path.
    return;
  }
  const githubEnv = env.GITHUB_ENV;
  if (githubEnv) {
    fs.appendFileSync(githubEnv, `${exportName}=${resolvedPath}\n`);
  }
}

function envPathIsInsideWorkspace(value, workspace) {
  if (!value) {
    return false;
  }
  const resolvedValue = path.resolve(String(value));
  const resolvedWorkspace = path.resolve(workspace);
  return resolvedValue === resolvedWorkspace || resolvedValue.startsWith(`${resolvedWorkspace}${path.sep}`);
}

function requiresWordPressDependencies(env = process.env) {
  const runtimeProfileId = env.PROFILE || env.RUNTIME_PROFILE || '';
  if (!runtimeProfileId) {
    return true;
  }
  let profiles;
  try {
    profiles = env.RUNTIME_PROFILES ? JSON.parse(env.RUNTIME_PROFILES) : {};
  } catch {
    return true;
  }
  const profile = profiles && typeof profiles === 'object' && !Array.isArray(profiles) ? profiles[runtimeProfileId] : null;
  if (!profile || typeof profile !== 'object' || Array.isArray(profile)) {
    return true;
  }
  return profile.requires_wordpress_dependencies !== false && profile.wordpress_dependencies !== false;
}

module.exports = {
  checkedOutPhpDependencyDirs,
  checkedOutDependencyDir,
  envPathIsInsideWorkspace,
  installCheckedOutPhpDependencies,
  requiresWordPressDependencies,
  safeDependencySubdir,
  setupRuntime,
};
