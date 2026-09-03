'use strict';

const assert = require('node:assert/strict');
const path = require('node:path');
const {
  resolveWpCodeboxArtifactPath,
  resolveWpCodeboxManifestArtifactPath,
  wpCodeboxArtifactByKey,
  wpCodeboxArtifactDirectory,
  wpCodeboxArtifactManifestV1,
  wpCodeboxArtifactPath,
  wpCodeboxBrowserArtifacts,
} = require('../lib/wp-codebox-artifacts');

const codeboxResult = {
  artifacts: {
    directory: '/tmp/codebox-artifacts',
    files: [
      { name: 'summary', path: 'summary.json', kind: 'json' },
      { name: 'visualDiff', path: 'files/browser/visual-compare/visual-diff.json' },
    ],
  },
};

assert.equal(wpCodeboxArtifactDirectory(codeboxResult, '/tmp/fallback'), '/tmp/codebox-artifacts');
assert.equal(wpCodeboxArtifactDirectory({}, '/tmp/fallback'), '/tmp/fallback');
assert.equal(wpCodeboxArtifactPath({ path: 'artifact.json' }), 'artifact.json');
assert.equal(wpCodeboxArtifactPath('artifact.json'), 'artifact.json');
assert.deepEqual(wpCodeboxArtifactByKey(codeboxResult, 'summary'), { name: 'summary', path: 'summary.json', kind: 'json' });
assert.deepEqual(wpCodeboxArtifactByKey(codeboxResult, 'visualDiff'), { name: 'visualDiff', path: 'files/browser/visual-compare/visual-diff.json' });
assert.equal(resolveWpCodeboxArtifactPath({
  codeboxResult,
  key: 'visualDiff',
}), path.join('/tmp/codebox-artifacts', 'files/browser/visual-compare/visual-diff.json'));
assert.equal(resolveWpCodeboxArtifactPath({
  codeboxResult: {},
  artifactsDirectory: '/tmp/alternate-artifacts',
  artifact: { path: 'browser/source.png' },
}), path.join('/tmp/alternate-artifacts', 'browser/source.png'));
assert.equal(resolveWpCodeboxArtifactPath({
  artifact: '/tmp/already-absolute.png',
}), '/tmp/already-absolute.png');
assert.throws(() => resolveWpCodeboxArtifactPath({ artifact: 'relative.json' }), /without artifact directory/);
assert.throws(() => resolveWpCodeboxArtifactPath({ codeboxResult, key: 'missing' }), /Unable to resolve WP Codebox artifact: missing/);

const browserOutput = { artifacts: { directory: '/tmp/wp-codebox-artifacts' } };
assert.equal(
  resolveWpCodeboxManifestArtifactPath(browserOutput, 'files/browser/summary.json'),
  path.join('/tmp/wp-codebox-artifacts', 'files/browser/summary.json')
);
assert.deepEqual(wpCodeboxBrowserArtifacts(browserOutput, ['summary.json', 'network.jsonl']), {
  directory: path.join('/tmp/wp-codebox-artifacts', 'files/browser'),
  'summary.json': path.join('/tmp/wp-codebox-artifacts', 'files/browser/summary.json'),
  'network.jsonl': path.join('/tmp/wp-codebox-artifacts', 'files/browser/network.jsonl'),
});

const manifestOutput = {
  artifacts: {
    directory: '/tmp/wp-codebox-artifacts',
    files: [
      { path: 'browser/summary.actual.json', relativePath: 'files/browser/summary.json' },
      { path: '/external/browser/network.actual.json', relativePath: 'files/browser/network.jsonl' },
    ],
  },
};
assert.deepEqual(wpCodeboxArtifactManifestV1(manifestOutput), {
  version: 1,
  directory: '/tmp/wp-codebox-artifacts',
  files: manifestOutput.artifacts.files,
});
assert.equal(
  resolveWpCodeboxManifestArtifactPath(manifestOutput, 'files/browser/summary.json'),
  path.join('/tmp/wp-codebox-artifacts', 'browser/summary.actual.json')
);
assert.equal(
  resolveWpCodeboxManifestArtifactPath(manifestOutput, 'files/browser/network.jsonl'),
  '/external/browser/network.actual.json'
);
assert.equal(resolveWpCodeboxManifestArtifactPath({}, 'files/browser/summary.json'), '');

console.log('✓ WP Codebox artifact helper smoke test PASSED');
