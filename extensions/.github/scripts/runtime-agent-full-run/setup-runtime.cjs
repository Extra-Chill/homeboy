#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { normalizeProviderPlugin, run } = require('./lib/common.cjs');
const { DEFAULT_RUNTIME_ID, resolveRuntimeProvider } = require('../../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');

function main() {
  const workspace = process.env.GITHUB_WORKSPACE || process.cwd();
  const runtime = resolveRuntimeProvider(process.env.RUNTIME || process.env.RUNTIME_PROVIDER || process.env.BACKEND || DEFAULT_RUNTIME_ID, { workspace });
  if (requiresWordPressDependencies(runtime, process.env)) {
    run('composer', ['install', '--no-interaction', '--no-progress', '--prefer-dist'], { cwd: path.join(workspace, '.ci/homeboy-extensions/wordpress') });
    run('npm', ['install'], { cwd: path.join(workspace, '.ci/homeboy-extensions/wordpress') });
  }
  for (const command of [...runtime.setupCommands, ...runtime.buildCommands]) {
    run(command.command, command.args, { cwd: path.join(workspace, command.cwd) });
  }
  if (requiresWordPressDependencies(runtime, process.env)) {
    installCheckedOutPhpDependencies(workspace);
  }
}

try {
  if (require.main === module) {
    main();
  }
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}

function requiresWordPressDependencies(runtime, env = process.env) {
  if (runtime?.manifest?.ci_materialization?.requires_wordpress_dependencies === true) {
    return true;
  }
  const runtimeProfileId = env.PROFILE || env.RUNTIME_PROFILE || '';
  if (!runtimeProfileId) {
    return false;
  }
  let profiles;
  try {
    profiles = env.RUNTIME_PROFILES ? JSON.parse(env.RUNTIME_PROFILES) : {};
  } catch {
    return false;
  }
  const profile = profiles && typeof profiles === 'object' && !Array.isArray(profiles) ? profiles[runtimeProfileId] : null;
  return Boolean(
    profile &&
    typeof profile === 'object' &&
    !Array.isArray(profile) &&
    (profile.requires_wordpress_dependencies === true || profile.wordpress_dependencies === true)
  );
}

function installCheckedOutPhpDependencies(workspace) {
  const ciDir = path.join(workspace, '.ci');
  if (!fs.existsSync(ciDir)) {
    return;
  }
  for (const entry of fs.readdirSync(ciDir, { withFileTypes: true })) {
    if (!entry.isDirectory()) {
      continue;
    }
    const componentDir = path.join(ciDir, entry.name);
    if (componentDir === path.join(workspace, '.ci/homeboy-extensions')) {
      continue;
    }
    if (fs.existsSync(path.join(componentDir, 'composer.json'))) {
      run('composer', ['install', '--no-interaction', '--no-progress', '--prefer-dist'], { cwd: componentDir });
    }
  }

  const providerPlugin = normalizeProviderPlugin(process.env.PROVIDER_PLUGIN || '{}', process.env.PROVIDER || '', false);
  if (!providerPlugin.repo) {
    return;
  }
  const providerPluginDir = path.join(ciDir, providerPlugin.repo.split('/')[1], providerPlugin.path || '.');
  if (fs.existsSync(path.join(providerPluginDir, 'composer.json'))) {
    run('composer', ['install', '--no-interaction', '--no-progress', '--prefer-dist'], { cwd: providerPluginDir });
  }
}

module.exports = {
  installCheckedOutPhpDependencies,
  main,
  requiresWordPressDependencies,
};
