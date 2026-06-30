#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const boundaryTerms = /Data Machine|DataMachine|datamachine|data-machine|wp-site-generator|WPSG|site-generator|site generator/;
const productionExtensions = new Set(['.js', '.cjs', '.mjs', '.ts', '.tsx', '.yml', '.yaml']);

const scannedRoots = [
  '.github/scripts',
  '.github/workflows',
  'datamachine-agent-ci',
  'runtime-agent-ci',
  'wordpress/lib',
  'wordpress/scripts/agent',
  'agent-runtimes',
];

const scannedFiles = [
  '.github/workflows/runtime-agent-ci.yml',
  'wordpress/scripts/agent/run-host-runner-lifecycle.cjs',
  'runtime-agent-ci/lib/runtime-provider-resolver.cjs',
];

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
  const absolutePath = path.join(repoRoot, relativePath);
  const content = fs.readFileSync(absolutePath, 'utf8');
  if (!boundaryTerms.test(content)) {
    continue;
  }
  violations.push(relativePath);
}

assert.deepEqual(
  violations,
  [],
  `Generic Homeboy Extensions production code must not reference Data Machine or wp-site-generator terms. Violations: ${violations.join(', ')}`,
);

const wordpressIndex = fs.readFileSync(path.join(repoRoot, 'wordpress/index.js'), 'utf8');
assert.equal(
  wordpressIndex.includes("./lib/audit-wp-codebox-fanout"),
  false,
  'WP Codebox audit fanout must stay behind its direct module/CLI entrypoints and out of the WordPress package barrel.',
);

const auditFanout = await import(path.join(repoRoot, 'wordpress/lib/audit-wp-codebox-fanout.js'));
assert.equal(auditFanout.IMPLEMENTATION_SCOPE?.quarantine, 'wp-codebox-compatibility-entrypoint');
assert.equal(auditFanout.IMPLEMENTATION_SCOPE?.generic_surface, false);
assert.equal(auditFanout.IMPLEMENTATION_SCOPE?.runtime_adapter, 'wordpress/lib/audit-fanout-runtime-adapter.js');

const wordpressPackage = await import(path.join(repoRoot, 'wordpress/index.js'));
assert.equal(typeof wordpressPackage.default.auditFanoutRuntimeProviderInterface, 'function');
assert.equal(wordpressPackage.default.createAuditWpCodeboxFanoutPlan, undefined);
assert.equal(wordpressPackage.default.wpCodebox?.createAuditWpCodeboxFanoutPlan, undefined);

const auditFanoutRuntimeProvider = fs.readFileSync(path.join(repoRoot, 'wordpress/lib/audit-fanout-runtime-provider.js'), 'utf8');
assert.equal(
  /wp-codebox|Codebox|audit-wp-codebox-fanout/.test(auditFanoutRuntimeProvider),
  false,
  'Generic audit fanout runtime provider interface must not reference the WP Codebox implementation lane.',
);

process.env.HOMEBOY_WP_CODEBOX_CORE_MODULE ||= path.join(repoRoot, 'tests', 'fixtures', 'wp-codebox-core-runtime-contract.cjs');
const runtimePackage = require(path.join(repoRoot, 'agent-runtimes', 'wp-codebox'));
const wpCodeboxPackageJson = JSON.parse(fs.readFileSync(path.join(repoRoot, 'agent-runtimes', 'wp-codebox', 'package.json'), 'utf8'));

assert.equal(runtimePackage.wpCodeboxCliDescriptor().schema, 'wp-codebox/cli-descriptor/v1');

const stableWpCodeboxConsumerExports = [
  '.',
  './codebox-agent-task-executor',
  './codebox-artifact-contract',
  './codebox-run-agent-task-contract',
  './codebox-runtime-profile',
  './provider-credential-boundary',
  './provider-outcome-normalizer',
  './wp-codebox-adapter-contract',
  './wp-codebox-adapter-descriptor',
  './wp-codebox-runtime-contract-source',
  './wp-codebox-runtime-readiness',
];
const forbiddenRuntimeExportTerms = /datamachine|data machine|wp-site-generator|wpsg|site generator/i;
for (const exportName of stableWpCodeboxConsumerExports) {
  assert.ok(wpCodeboxPackageJson.exports[exportName], `${exportName} stays exported for runtime consumers`);
  assert.doesNotMatch(JSON.stringify(wpCodeboxPackageJson.exports[exportName]), forbiddenRuntimeExportTerms, `${exportName} export should stay on the WP Codebox runtime package contract`);
}

const wpCodeboxRuntimeFiles = walkProductionFiles('agent-runtimes/wp-codebox');
for (const relativePath of wpCodeboxRuntimeFiles) {
  const source = fs.readFileSync(path.join(repoRoot, relativePath), 'utf8');
  assert.equal(source.includes('codebox-legacy-result-adapter'), false, `${relativePath} must not depend on removed legacy result parsing`);
}

const auditFanoutReferencePattern = /audit-wp-codebox-fanout|homeboy-audit-wp-codebox-fanout|createAuditWpCodeboxFanoutPlan|executeAuditWpCodeboxFanout/;
const auditFanoutAllowedReferences = new Set([
  'wordpress/homeboy.json',
  'wordpress/lib/audit-wp-codebox-fanout.js',
  'wordpress/package.json',
  'wordpress/scripts/agent/homeboy-audit-wp-codebox-fanout.cjs',
]);
const auditFanoutReferenceFiles = [
  ...scannedRoots.flatMap(walkProductionFiles),
  'wordpress/homeboy.json',
  'wordpress/package.json',
]
  .filter((relativePath, index, all) => all.indexOf(relativePath) === index)
  .filter((relativePath) => auditFanoutReferencePattern.test(fs.readFileSync(path.join(repoRoot, relativePath), 'utf8')))
  .filter((relativePath) => !auditFanoutAllowedReferences.has(relativePath));

assert.deepEqual(
  auditFanoutReferenceFiles,
  [],
  `WP Codebox audit fanout references must stay in the quarantined implementation, CLI, and package metadata. Violations: ${auditFanoutReferenceFiles.join(', ')}`,
);

const genericSetupRuntime = fs.readFileSync(path.join(repoRoot, '.github/scripts/runtime-agent-full-run/setup-runtime.cjs'), 'utf8');
assert.equal(
  /WordPress|wordpress|wp-codebox|Codebox|requires_wordpress_dependencies|wordpress_dependencies/.test(genericSetupRuntime),
  false,
  'Generic runtime setup must delegate runtime-specific setup through manifest adapters instead of naming WordPress or WP Codebox concerns.',
);

console.log('architectural boundary smoke passed');
