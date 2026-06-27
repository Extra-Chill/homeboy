'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(__dirname, '..', '..', '..', 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');

const {
  wpCodeboxBinaryDiagnostic,
  wpCodeboxProviderPluginPathsFromEnv,
  wpCodeboxResolveCommand,
} = require('..');

const runtimeRoot = path.join(__dirname, '..');
const runnerSource = fs.readFileSync(path.join(runtimeRoot, 'scripts', 'agent', 'homeboy-wp-codebox-task-runner.cjs'), 'utf8');

assert.doesNotMatch(runnerSource, /HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS|WP_CODEBOX_PROVIDER_PLUGIN_PATHS/);
assert.doesNotMatch(runnerSource, /packages\/cli\/dist|HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT|HOMEBOY_WP_CODEBOX_INSTALL_DIR/);

assert.deepEqual(wpCodeboxProviderPluginPathsFromEnv({
  WP_CODEBOX_PROVIDER_PLUGIN_PATHS: JSON.stringify(['/tmp/provider-a', '/tmp/provider-b']),
}), ['/tmp/provider-a', '/tmp/provider-b']);
assert.deepEqual(wpCodeboxProviderPluginPathsFromEnv({
  HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATH: ['/tmp/provider-a', '/tmp/provider-b'].join(path.delimiter),
  WP_CODEBOX_PROVIDER_PLUGIN_PATHS: JSON.stringify(['/tmp/provider-c']),
}), ['/tmp/provider-a', '/tmp/provider-b']);

assert.equal(wpCodeboxBinaryDiagnostic('').class, 'wp-codebox.config.missing_binary');
assert.equal(wpCodeboxBinaryDiagnostic('wp-codebox'), null);
assert.deepEqual(wpCodeboxResolveCommand('/tmp/wp-codebox.cjs', ['run-agent-task']).args, ['/tmp/wp-codebox.cjs', 'run-agent-task']);

process.stdout.write('WP Codebox adapter boundary passed\n');
