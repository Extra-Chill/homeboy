'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..', '..');
const scannedFiles = [
  'wordpress/wordpress.json',
  'wordpress/lib/codebox-agent-task-executor.js',
  'wordpress/scripts/agent/homeboy-codebox-agent-task-executor.cjs',
  'wordpress/scripts/agent/homeboy-wp-codebox-task-runner.cjs',
  'agent-runtimes/wp-codebox/lib/codebox-agent-task-executor.js',
  'agent-runtimes/wp-codebox/scripts/agent/homeboy-codebox-agent-task-executor.cjs',
];

const runtimeName = ['data', 'machine'].join('');
const runtimeSnake = ['data', 'machine'].join('_');
const forbiddenPatterns = [
  `${runtimeSnake}_bundle`,
  `${runtimeSnake}_bundle_execution`,
  `HOMEBOY_${runtimeSnake.toUpperCase()}_AGENT_CONFIG`,
  `${runtimeSnake}_path`,
  `${runtimeSnake}_code_path`,
  `${runtimeName}-agent-workload`,
  `${runtimeName}/import-agent`,
  `${runtimeName}/run-flow`,
  `${runtimeName}/execute-workflow`,
  `${runtimeName}/drain-job`,
  `${runtimeName}_resolved_tools`,
  `${runtimeName}_merge_engine_data`,
  `${runtimeName}_directives_enabled`,
];
const forbiddenClassPattern = new RegExp(`${runtimeName.charAt(0).toUpperCase()}${runtimeName.slice(1, 4)}${runtimeName.charAt(4).toUpperCase()}${runtimeName.slice(5)}\\\\Core`);

const failures = [];
for (const relativePath of scannedFiles) {
  const absolutePath = path.join(root, relativePath);
  const source = fs.readFileSync(absolutePath, 'utf8');
  for (const pattern of forbiddenPatterns) {
    if (source.includes(pattern)) {
      failures.push(`${relativePath}: ${pattern}`);
    }
  }
  if (forbiddenClassPattern.test(source)) {
    failures.push(`${relativePath}: runtime core namespace import`);
  }
}

assert.deepEqual(failures, []);
console.log('Codebox agent-task boundary smoke passed');
