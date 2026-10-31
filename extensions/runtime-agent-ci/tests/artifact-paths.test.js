'use strict';

require('./helpers/runtime-contract-constants-fixture.cjs');

const assert = require('node:assert/strict');
const path = require('node:path');

const {
  ARTIFACT_MANIFEST_FILE,
  artifactManifestForFiles,
  runtimeAgentArtifactPaths,
} = require('../lib/artifact-paths.cjs');

const canonicalRunDir = path.join('/tmp', 'homeboy-runtime-agent-run');
assert.equal(
  runtimeAgentArtifactPaths({ artifact_paths: { run_dir: canonicalRunDir } }).artifact_manifest,
  path.join(canonicalRunDir, ARTIFACT_MANIFEST_FILE)
);

const canonicalOptions = runtimeAgentArtifactPaths({
  run_dir: canonicalRunDir,
  artifact_manifest_file: '/tmp/canonical-manifest.json',
});
assert.equal(canonicalOptions.run_dir, canonicalRunDir);
assert.equal(canonicalOptions.artifact_manifest, '/tmp/canonical-manifest.json');

const projectedPaths = runtimeAgentArtifactPaths({ artifact_paths: { run_dir: canonicalRunDir } });
const projectedManifest = artifactManifestForFiles(projectedPaths, [
  { path: projectedPaths.outcome },
  { path: projectedPaths.results, semantic_key: 'explicit-results' },
]);
assert.deepEqual(projectedManifest.artifacts.map((artifact) => artifact.semantic_key), ['outcome', 'explicit-results']);

process.stdout.write('Runtime agent artifact path contract checks passed\n');
