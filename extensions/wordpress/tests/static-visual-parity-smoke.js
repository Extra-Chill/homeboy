'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {
  buildStaticVisualParityRecipe,
  normalizeStaticVisualParityArtifacts,
  runStaticVisualParity,
} = require('../lib/static-visual-parity');

function withTempDirectory(callback) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'static-visual-parity-'));
  return Promise.resolve()
    .then(() => callback(root))
    .finally(() => fs.rmSync(root, { recursive: true, force: true }));
}

function writeFakeWpCodebox(filePath) {
  fs.writeFileSync(filePath, `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
const recipePath = args[args.indexOf('--recipe') + 1];
const artifactsDir = args[args.indexOf('--artifacts') + 1];
const recipe = JSON.parse(fs.readFileSync(recipePath, 'utf8'));
fs.writeFileSync(process.env.FAKE_WP_CODEBOX_RECIPE_CAPTURE, JSON.stringify(recipe, null, 2));
const visualDir = path.join(artifactsDir, 'files', 'browser', 'visual-compare');
fs.mkdirSync(visualDir, { recursive: true });
for (const file of ['source.png', 'candidate.png', 'diff.png']) {
  fs.writeFileSync(path.join(visualDir, file), file);
}
fs.writeFileSync(path.join(visualDir, 'visual-diff.json'), JSON.stringify({
  schema: 'wp-codebox/visual-compare/v1',
  status: 'identical',
  files: {
    sourceScreenshot: 'files/browser/visual-compare/source.png',
    candidateScreenshot: 'files/browser/visual-compare/candidate.png',
    diffScreenshot: 'files/browser/visual-compare/diff.png',
    visualDiff: 'files/browser/visual-compare/visual-diff.json'
  },
  comparison: {
    mismatchRatio: 0,
    mismatchPixels: 0,
    totalPixels: 100,
    dimensionMismatch: false,
    source: { width: 10, height: 10 },
    candidate: { width: 10, height: 10 },
    diff: { width: 10, height: 10 }
  }
}, null, 2));
process.stdout.write(JSON.stringify({ success: true, schema: 'wp-codebox/recipe-run/v1', artifacts: { directory: artifactsDir } }));
`);
  fs.chmodSync(filePath, 0o755);
}

assert.deepEqual(buildStaticVisualParityRecipe({
  sourceUrl: 'http://127.0.0.1:4173/index.html',
  candidateUrl: '/',
  sourceLabel: 'source',
  candidateLabel: 'candidate',
  viewport: { width: 800, height: 600 },
  artifactsDirectory: '/tmp/artifacts',
  blueprint: { steps: [{ step: 'runPHP', code: '<?php' }] },
  mounts: [{ source: '/repo', target: '/wordpress/wp-content/plugins/example', mode: 'readonly' }],
}).workflow.steps[0].args, [
  'source-url=http://127.0.0.1:4173/index.html',
  'candidate-url=/',
  'source-label=source',
  'candidate-label=candidate',
  'viewport=800x600',
  'full-page=true',
  'wait-for=domcontentloaded',
  'threshold=0.1',
  'include-aa=true',
]);

withTempDirectory(async (root) => {
  const artifactsDirectory = path.join(root, 'artifacts');
  const outputDirectory = path.join(root, 'output');
  const visualDir = path.join(artifactsDirectory, 'custom-browser-artifacts');
  fs.mkdirSync(visualDir, { recursive: true });
  fs.writeFileSync(path.join(visualDir, 'source.png'), 'source');
  fs.writeFileSync(path.join(visualDir, 'candidate.png'), 'candidate');
  fs.writeFileSync(path.join(visualDir, 'diff.png'), 'diff');
  fs.writeFileSync(path.join(visualDir, 'visual-diff.json'), JSON.stringify({
    schema: 'wp-codebox/visual-compare/v1',
    status: 'different',
    comparison: {
      mismatchRatio: 0.01,
      mismatchPixels: 10,
      totalPixels: 1000,
      dimensionMismatch: false,
      regions: [{ x: 1, y: 2, width: 5, height: 5, mismatchPixels: 3 }]
    },
    files: {
      sourceScreenshot: 'custom-browser-artifacts/source.png',
      candidateScreenshot: 'custom-browser-artifacts/candidate.png',
      diffScreenshot: 'custom-browser-artifacts/diff.png'
    }
  }));
  fs.mkdirSync(outputDirectory, { recursive: true });

  const visualDiff = await normalizeStaticVisualParityArtifacts({
    codeboxResult: {
      success: true,
      artifacts: { directory: artifactsDirectory },
      files: { visualDiff: 'custom-browser-artifacts/visual-diff.json' },
    },
    outputDirectory,
    maxMismatchRatio: 0.02,
  });
  assert.equal(visualDiff.pass, true);
  assert.equal(visualDiff.regions[0].mismatchRatio, 3 / 25);
  assert.equal(fs.existsSync(path.join(outputDirectory, 'imported.png')), true);
});

withTempDirectory(async (root) => {
  const sourceDirectory = path.join(root, 'source');
  const outputDirectory = path.join(root, 'output');
  const readinessFile = path.join(outputDirectory, 'ready.json');
  const fakeWpCodebox = path.join(root, 'wp-codebox.js');
  const recipeCapture = path.join(root, 'recipe-capture.json');
  fs.mkdirSync(sourceDirectory, { recursive: true });
  fs.writeFileSync(path.join(sourceDirectory, 'index.html'), '<!doctype html><title>Demo</title>');
  fs.mkdirSync(outputDirectory, { recursive: true });
  fs.writeFileSync(readinessFile, JSON.stringify({ theme: 'demo' }));
  writeFakeWpCodebox(fakeWpCodebox);

  const previous = process.env.FAKE_WP_CODEBOX_RECIPE_CAPTURE;
  process.env.FAKE_WP_CODEBOX_RECIPE_CAPTURE = recipeCapture;
  try {
    const result = await runStaticVisualParity({
      sourceDirectory,
      outputDirectory,
      artifactsDirectory: path.join(root, 'artifacts'),
      sourcePort: 4199,
      candidateUrl: '/',
      wpCodeboxBin: fakeWpCodebox,
      readinessFile,
      validateReadiness: (readiness) => assert.equal(readiness.theme, 'demo'),
      metadata: { site: 'demo' },
    });
    const recipe = JSON.parse(fs.readFileSync(recipeCapture, 'utf8'));
    assert.equal(recipe.workflow.steps[0].command, 'wordpress.visual-compare');
    assert.equal(result.summary.site, 'demo');
    assert.equal(result.visualDiff.pass, true);
    assert.equal(fs.existsSync(path.join(outputDirectory, 'summary.json')), true);
  } finally {
    if (previous === undefined) {
      delete process.env.FAKE_WP_CODEBOX_RECIPE_CAPTURE;
    } else {
      process.env.FAKE_WP_CODEBOX_RECIPE_CAPTURE = previous;
    }
  }
});

console.log('✓ static visual parity smoke test PASSED');
