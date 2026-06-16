'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const manifestPath = path.join(__dirname, '..', 'wordpress.json');
const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'));
const provider = manifest.agent_runtimes
  .find((runtime) => runtime.id === 'wp-codebox')
  .agent_task_executors.find((executor) => executor.id === 'wordpress.codebox-agent-task-executor');

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wordpress-relink-layout-'));

try {
  const extensionsRoot = path.join(root, 'extensions');
  const extensionPath = path.join(extensionsRoot, 'wordpress');
  fs.mkdirSync(extensionsRoot, { recursive: true });
  fs.symlinkSync(path.join(__dirname, '..'), extensionPath, 'dir');

const command = provider.command.replaceAll('{{extension_path}}', extensionPath);
  assert(
    !command.includes('../agent-runtimes/'),
    'relinked wordpress provider command must not require a sibling agent-runtimes install'
  );

  const [, scriptPath] = command.match(/^node\s+(.+)$/) || [];
  assert(scriptPath, 'provider command should be a node script command');
  assert.equal(
    path.normalize(scriptPath),
    path.join(extensionPath, 'scripts', 'agent', 'homeboy-codebox-agent-task-executor.cjs')
  );
  assert.equal(fs.existsSync(scriptPath), true, `provider command target should exist: ${scriptPath}`);
  assert.equal(
    fs.existsSync(path.join(extensionsRoot, 'agent-runtimes')),
    false,
    'smoke layout should not install sibling agent-runtimes'
  );
} finally {
  fs.rmSync(root, { recursive: true, force: true });
}

console.log('Agent runtime command path smoke passed');
