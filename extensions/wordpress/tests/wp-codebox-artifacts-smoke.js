'use strict';

const assert = require('node:assert/strict');
const path = require('node:path');
const {
  resolveWpCodeboxArtifactPath,
  wpCodeboxArtifactByKey,
  wpCodeboxArtifactDirectory,
  wpCodeboxArtifactPath,
} = require('../lib/wp-codebox-artifacts');

const codeboxResult = {
  artifacts: {
    directory: '/tmp/codebox-artifacts',
    summary: { path: 'summary.json', kind: 'json' },
  },
  files: {
    visualDiff: 'files/browser/visual-compare/visual-diff.json',
  },
};

assert.equal(wpCodeboxArtifactDirectory(codeboxResult, '/tmp/fallback'), '/tmp/codebox-artifacts');
assert.equal(wpCodeboxArtifactDirectory({}, '/tmp/fallback'), '/tmp/fallback');
assert.equal(wpCodeboxArtifactPath({ path: 'artifact.json' }), 'artifact.json');
assert.equal(wpCodeboxArtifactPath('artifact.json'), 'artifact.json');
assert.deepEqual(wpCodeboxArtifactByKey(codeboxResult, 'summary'), { path: 'summary.json', kind: 'json' });
assert.equal(wpCodeboxArtifactByKey(codeboxResult, 'visualDiff'), 'files/browser/visual-compare/visual-diff.json');
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

console.log('✓ WP Codebox artifact helper smoke test PASSED');
