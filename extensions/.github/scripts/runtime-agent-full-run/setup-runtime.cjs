#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { normalizeProviderPlugin, run } = require('./lib/common.cjs');
const { DEFAULT_RUNTIME_ID, resolveRuntimeProvider } = require('../../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');

try {
  const workspace = process.env.GITHUB_WORKSPACE || process.cwd();
  const runtime = resolveRuntimeProvider(process.env.RUNTIME || process.env.RUNTIME_PROVIDER || process.env.BACKEND || DEFAULT_RUNTIME_ID, { workspace });
  run('composer', ['install', '--no-interaction', '--no-progress', '--prefer-dist'], { cwd: path.join(workspace, '.ci/homeboy-extensions/wordpress') });
  run('npm', ['install'], { cwd: path.join(workspace, '.ci/homeboy-extensions/wordpress') });
  for (const command of [...runtime.setupCommands, ...runtime.buildCommands]) {
    run(command.command, command.args, { cwd: path.join(workspace, command.cwd) });
  }
  installCheckedOutPhpDependencies(workspace);
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
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
