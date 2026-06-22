#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(repoRoot, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const runtimePackage = require(path.join(repoRoot, 'agent-runtimes', 'wp-codebox'));
const packageJson = JSON.parse(fs.readFileSync(path.join(repoRoot, 'agent-runtimes', 'wp-codebox', 'package.json'), 'utf8'));

assert.equal(runtimePackage.DEPRECATED_CODEBOX_LEGACY_RESULT_ADAPTER.status, 'deprecated');
assert.equal(runtimePackage.wpCodeboxCliDescriptor().schema, 'wp-codebox/cli-descriptor/v1');

const stableConsumerExports = [
  '.',
  './codebox-agent-task-executor',
  './codebox-run-agent-task-contract',
  './wp-codebox-adapter-contract',
  './wp-codebox-adapter-descriptor',
];
const forbidden = /datamachine|data machine|wp-site-generator|wpsg|site generator/i;
for (const exportName of stableConsumerExports) {
  assert.doesNotMatch(JSON.stringify(packageJson.exports[exportName]), forbidden, `${exportName} export should stay on the WP Codebox runtime package contract`);
}

const allowedLegacyImporters = new Set([
  'agent-runtimes/wp-codebox/index.js',
  'agent-runtimes/wp-codebox/lib/codebox-artifact-contract.js',
  'agent-runtimes/wp-codebox/lib/codebox-run-agent-task-contract.js',
  'agent-runtimes/wp-codebox/lib/codebox-legacy-result-adapter.js',
]);
const runtimeFiles = walk(path.join(repoRoot, 'agent-runtimes', 'wp-codebox'))
  .filter((filePath) => filePath.endsWith('.js'));
for (const filePath of runtimeFiles) {
  const relativePath = path.relative(repoRoot, filePath);
  const source = fs.readFileSync(filePath, 'utf8');
  if (source.includes('codebox-legacy-result-adapter')) {
    assert.equal(allowedLegacyImporters.has(relativePath), true, `${relativePath} must not depend on deprecated legacy result parsing`);
  }
}

function walk(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const entryPath = path.join(directory, entry.name);
    return entry.isDirectory() ? walk(entryPath) : [entryPath];
  });
}

console.log('wp-codebox adapter boundary smoke passed');
