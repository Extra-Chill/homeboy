'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');
const {
  normalizeVisualCompareResult,
  runWpCodeboxVisualCompare,
  visualCompareArgs,
} = require('../lib/wp-codebox-visual-compare');

function withTempDirectory(callback) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'wp-codebox-visual-compare-'));
  try {
    callback(root);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

function writeFakeWpCodebox(filePath, output) {
  fs.writeFileSync(filePath, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
if (args.includes('--version')) { process.stdout.write('0.21.0'); process.exit(0); }
if (args.slice(-3).join(' ') === 'runtime descriptor --json') { process.stdout.write(JSON.stringify({ schema: 'wp-codebox/runtime-descriptor/v1', readiness: { status: 'available', browserRuntime: { status: 'ready' } }, contractManifest: { schemas: { runtimeBoundary: { browserContainedSiteOpen: 'wp-codebox/browser-contained-site-open/v1' } } } })); process.exit(0); }
fs.writeFileSync(process.env.FAKE_WP_CODEBOX_ARGS_PATH, JSON.stringify(args));
if (args[0] !== 'run' || !args.includes('--command') || !args.includes('wordpress.visual-compare') || !args.includes('--json')) {
  console.error('unexpected wp-codebox args: ' + JSON.stringify(args));
  process.exit(2);
}
process.stdout.write(${JSON.stringify(`${JSON.stringify(output, null, 2)}\n`)});
`);
  fs.chmodSync(filePath, 0o755);
}

withTempDirectory((root) => {
  const artifactsDirectory = path.join(root, 'visual-artifacts');
  const argsPath = path.join(root, 'wp-codebox-args.json');
  const fakeWpCodebox = path.join(root, 'wp-codebox.js');
  const output = {
    schema: 'wp-codebox/visual-compare/v1',
    command: 'wordpress.visual-compare',
    status: 'identical',
    files: {
      sourceScreenshot: 'files/browser/visual-compare/source.png',
      candidateScreenshot: 'files/browser/visual-compare/candidate.png',
      diffScreenshot: 'files/browser/visual-compare/diff.png',
      visualDiff: 'files/browser/visual-compare/visual-diff.json',
      summary: 'files/browser/visual-compare/summary.json',
    },
    comparison: {
      mismatchRatio: 0,
      mismatchPixels: 0,
      totalPixels: 1234,
      dimensionMismatch: false,
    },
  };
  writeFakeWpCodebox(fakeWpCodebox, output);

  const previousArgsPath = process.env.FAKE_WP_CODEBOX_ARGS_PATH;
  process.env.FAKE_WP_CODEBOX_ARGS_PATH = argsPath;
  try {
    const parsed = runWpCodeboxVisualCompare({
      sourceScreenshot: '/tmp/source.png',
      candidateScreenshot: '/tmp/candidate.png',
      sourceLabel: 'baseline',
      candidateLabel: 'candidate',
      threshold: 0.1,
      artifactsDirectory,
    }, fakeWpCodebox);
    assert.deepEqual(JSON.parse(fs.readFileSync(argsPath, 'utf8')), [
      'run',
      '--command',
      'wordpress.visual-compare',
      '--arg',
      'source-screenshot=/tmp/source.png',
      '--arg',
      'candidate-screenshot=/tmp/candidate.png',
      '--arg',
      'source-label=baseline',
      '--arg',
      'candidate-label=candidate',
      '--arg',
      'threshold=0.1',
      '--artifacts',
      artifactsDirectory,
      '--json',
    ]);
    assert.equal(parsed.status, 'identical');
    assert.equal(parsed.metrics.visual_mismatch_ratio, 0);
    assert.equal(parsed.metrics.visual_dimension_mismatch, false);
    assert.equal(parsed.artifacts.visual_diff_screenshot.path, 'files/browser/visual-compare/diff.png');
  } finally {
    if (previousArgsPath === undefined) {
      delete process.env.FAKE_WP_CODEBOX_ARGS_PATH;
    } else {
      process.env.FAKE_WP_CODEBOX_ARGS_PATH = previousArgsPath;
    }
  }
});

assert.deepEqual(visualCompareArgs({
  sourceUrl: '/source',
  candidateUrl: '/candidate',
  explainSelectors: ['.button'],
  artifactsDirectory: '/tmp/artifacts',
}), [
  'run',
  '--command',
  'wordpress.visual-compare',
  '--arg',
  'source-url=/source',
  '--arg',
  'candidate-url=/candidate',
  '--arg',
  'explain-selector=.button',
  '--artifacts',
  '/tmp/artifacts',
  '--json',
]);

assert.equal(normalizeVisualCompareResult({
  artifact: {
    files: { diffScreenshot: 'diff.png' },
    summary: {
      visualCompare: {
        status: 'different',
        mismatchRatio: 0.2,
        mismatchPixels: 42,
        totalPixels: 210,
        dimensionMismatch: false,
      },
    },
  },
}, '/tmp/artifacts').metrics.visual_mismatch_pixels, 42);

withTempDirectory((root) => {
  const inputPath = path.join(root, 'input.json');
  const argsPath = path.join(root, 'wp-codebox-args.json');
  const fakeWpCodebox = path.join(root, 'wp-codebox');
  writeFakeWpCodebox(fakeWpCodebox, {
    schema: 'wp-codebox/visual-compare/v1',
    status: 'similar',
    files: { visualDiff: 'files/browser/visual-compare/visual-diff.json' },
    comparison: { mismatchRatio: 0.01, mismatchPixels: 10, totalPixels: 1000, dimensionMismatch: false },
  });
  fs.writeFileSync(inputPath, `${JSON.stringify({ sourceScreenshot: 'a.png', candidateScreenshot: 'b.png', artifactsDirectory: path.join(root, 'artifacts') })}\n`);
  const result = spawnSync(process.execPath, [path.join(__dirname, '..', 'lib', 'wp-codebox-visual-compare.js'), inputPath, fakeWpCodebox], {
    encoding: 'utf8',
    env: { ...process.env, FAKE_WP_CODEBOX_ARGS_PATH: argsPath },
  });
  assert.equal(result.status, 0, result.stderr);
  const parsed = JSON.parse(result.stdout);
  assert.equal(parsed.metrics.visual_mismatch_ratio, 0.01);
});

console.log('✓ WP Codebox visual compare smoke test PASSED');
