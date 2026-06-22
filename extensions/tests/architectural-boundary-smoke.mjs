#!/usr/bin/env node
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const boundaryTerms = /Data Machine|DataMachine|datamachine|data-machine|wp-site-generator|WPSG|site-generator|site generator/;
const productionExtensions = new Set(['.js', '.cjs', '.mjs', '.ts', '.tsx', '.yml', '.yaml']);
const quarantineManifestPath = 'tests/architectural-boundary-quarantine.json';
const quarantineManifest = JSON.parse(
  fs.readFileSync(path.join(repoRoot, quarantineManifestPath), 'utf8')
);

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
const quarantinedFiles = new Set(Object.keys(quarantineManifest));

const manifestViolations = [];
for (const [relativePath, justification] of Object.entries(quarantineManifest)) {
  if (typeof justification !== 'string' || justification.trim() === '') {
    manifestViolations.push(`${relativePath} is missing a file-level justification`);
  }
  if (!fs.existsSync(path.join(repoRoot, relativePath))) {
    manifestViolations.push(`${relativePath} does not exist`);
  }
}

const violations = [];
const quarantinedTermFiles = [];
for (const relativePath of files) {
  const absolutePath = path.join(repoRoot, relativePath);
  const content = fs.readFileSync(absolutePath, 'utf8');
  if (!boundaryTerms.test(content)) {
    continue;
  }
  if (quarantinedFiles.has(relativePath)) {
    quarantinedTermFiles.push(relativePath);
    continue;
  }
  violations.push(relativePath);
}

const staleQuarantineEntries = [...quarantinedFiles].filter((relativePath) => {
  const absolutePath = path.join(repoRoot, relativePath);
  return fs.existsSync(absolutePath) && !boundaryTerms.test(fs.readFileSync(absolutePath, 'utf8'));
});

assert.deepEqual(
  manifestViolations,
  [],
  `Boundary quarantine manifest entries must name existing files and include file-level justifications. Violations: ${manifestViolations.join(', ')}`,
);

assert.deepEqual(
  violations,
  [],
  `Generic Homeboy Extensions production code must not reference Data Machine or wp-site-generator terms outside ${quarantineManifestPath}. Violations: ${violations.join(', ')}`,
);

assert.deepEqual(
  staleQuarantineEntries,
  [],
  `Boundary quarantine entries must be removed when the file no longer contains Data Machine or wp-site-generator terms. Stale entries: ${staleQuarantineEntries.join(', ')}`,
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

const quarantineCounts = quarantinedTermFiles.reduce((counts, relativePath) => {
  const parts = relativePath.split('/');
  const root = parts[0] === '.github'
    ? parts.slice(0, parts[1] === 'scripts' ? 3 : 2).join('/')
    : parts[0];
  counts[root] = (counts[root] || 0) + 1;
  return counts;
}, {});

console.log('architectural boundary quarantine report:', JSON.stringify({
  quarantined_files: quarantinedFiles.size,
  quarantined_files_with_boundary_terms: quarantinedTermFiles.length,
  counts_by_root: quarantineCounts,
  files: quarantinedTermFiles.sort(),
}, null, 2));

console.log('architectural boundary smoke passed');
