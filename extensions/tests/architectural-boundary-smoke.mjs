#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const boundaryTerms = /Data Machine|DataMachine|datamachine|data-machine|wp-site-generator|WPSG|site-generator|site generator/;
const productionExtensions = new Set(['.js', '.cjs', '.mjs', '.ts', '.tsx']);

const scannedRoots = [
  'runtime-agent-ci',
  'wordpress/lib',
  'agent-runtimes/lib',
];

const scannedFiles = [
  '.github/workflows/runtime-agent-ci.yml',
  'wordpress/scripts/agent/run-host-runner-lifecycle.cjs',
  'agent-runtimes/lib/runtime-provider-resolver.cjs',
];

const allowedTransitionalAdapters = [
  /^datamachine-agent-ci\//,
  /^wordpress\/lib\/datamachine-agent-ci(?:-|$)/,
];

function isAllowedTransitionalAdapter(relativePath) {
  return allowedTransitionalAdapters.some((pattern) => pattern.test(relativePath));
}

function walkProductionFiles(relativeDir) {
  const absoluteDir = path.join(repoRoot, relativeDir);
  if (!fs.existsSync(absoluteDir)) {
    return [];
  }
  const files = [];
  for (const entry of fs.readdirSync(absoluteDir, { withFileTypes: true })) {
    const relativePath = path.join(relativeDir, entry.name).replace(/\\/g, '/');
    if (entry.isDirectory()) {
      files.push(...walkProductionFiles(relativePath));
    } else if (entry.isFile() && productionExtensions.has(path.extname(entry.name))) {
      files.push(relativePath);
    }
  }
  return files;
}

const files = new Set([
  ...scannedRoots.flatMap(walkProductionFiles),
  ...scannedFiles,
]);

const violations = [];
for (const relativePath of files) {
  if (isAllowedTransitionalAdapter(relativePath)) {
    continue;
  }
  const absolutePath = path.join(repoRoot, relativePath);
  const content = fs.readFileSync(absolutePath, 'utf8');
  if (boundaryTerms.test(content)) {
    violations.push(relativePath);
  }
}

assert.deepEqual(
  violations,
  [],
  `Generic Homeboy Extensions production code must not reference Data Machine or wp-site-generator terms. Quarantine them in datamachine-agent-ci or WP Codebox provider adapters. Violations: ${violations.join(', ')}`,
);

console.log('architectural boundary smoke passed');
