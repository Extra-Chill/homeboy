#!/usr/bin/env node
'use strict';

const path = require('node:path');
const { run } = require('./lib/common.cjs');
const { DEFAULT_RUNTIME_ID, resolveRuntimeProvider } = require('../../../runtime-agent-ci/lib/runtime-provider-resolver.cjs');

function main() {
  const workspace = process.env.GITHUB_WORKSPACE || process.cwd();
  const runtime = resolveRuntimeProvider(process.env.RUNTIME || DEFAULT_RUNTIME_ID, { workspace });
  runRuntimeSetup(runtime, { phase: 'before_commands', workspace, env: process.env, run });
  for (const command of [...runtime.setupCommands, ...runtime.buildCommands]) {
    run(command.command, command.args, { cwd: path.join(workspace, command.cwd) });
  }
  runRuntimeSetup(runtime, { phase: 'after_commands', workspace, env: process.env, run });
}

try {
  if (require.main === module) {
    main();
  }
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(1);
}

function runRuntimeSetup(runtime, context) {
  const adapter = runtimeSetupAdapter(runtime);
  if (!adapter) {
    return;
  }
  adapter({ runtime, ...context });
}

function runtimeSetupAdapter(runtime) {
  const adapter = runtime?.manifest?.ci_materialization?.setup_adapter;
  if (!adapter || typeof adapter !== 'object' || Array.isArray(adapter) || !adapter.module) {
    return null;
  }
  const repoRoot = path.resolve(__dirname, '..', '..', '..');
  const adapterPath = path.resolve(repoRoot, adapter.module);
  const loaded = require(adapterPath);
  const exportName = adapter.export || 'setupRuntime';
  if (typeof loaded[exportName] !== 'function') {
    throw new Error(`Runtime ${runtime.id} setup adapter ${adapter.module} does not export ${exportName}`);
  }
  return loaded[exportName];
}

module.exports = {
  main,
  runRuntimeSetup,
  runtimeSetupAdapter,
};
