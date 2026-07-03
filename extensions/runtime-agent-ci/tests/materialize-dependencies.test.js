'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  assertSafeDependencyTargetPath,
  normalizeDependencyEntry,
  resolveDependencyTarget,
  resolvePlan,
  runtimeDependencyEntries,
} = require('../lib/materialize-dependencies.cjs');

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-materialize-deps-'));
try {
  assert.deepEqual(runtimeDependencyEntries('Extra-Chill/runtime-a@main, Extra-Chill/runtime-b'), [
    'Extra-Chill/runtime-a@main',
    'Extra-Chill/runtime-b',
  ]);
  assert.deepEqual(runtimeDependencyEntries('[{"repo":"Extra-Chill/runtime-a","ref":"main","target":".ci/runtime-a"}]'), [{
    repo: 'Extra-Chill/runtime-a',
    ref: 'main',
    target: '.ci/runtime-a',
  }]);

  assert.deepEqual(normalizeDependencyEntry('Extra-Chill/runtime-a@trunk'), {
    repo: 'Extra-Chill/runtime-a',
    ref: 'trunk',
    target: '',
  });
  assert.throws(() => normalizeDependencyEntry('Extra-Chill/runtime-a@'), /non-empty ref/);

  assert.deepEqual(resolveDependencyTarget('.ci/runtime-a', workspace), {
    target: path.normalize('.ci/runtime-a'),
    targetPath: path.join(workspace, '.ci/runtime-a'),
  });
  assert.throws(() => resolveDependencyTarget('runtime-a', workspace), /under \.ci\//);
  assert.throws(() => resolveDependencyTarget('.ci/../runtime-a', workspace), /parent-directory/);

  assert.deepEqual(resolvePlan([
    { repo: 'Extra-Chill/runtime-a', ref: 'main', target: '.ci/runtime-a' },
    'Extra-Chill/runtime-a@main',
    'Extra-Chill/runtime-b@trunk',
  ], true, { workspace }), [
    { repo: 'Extra-Chill/runtime-a', ref: 'main', target: path.normalize('.ci/runtime-a'), targetPath: path.join(workspace, '.ci/runtime-a') },
    { repo: 'Extra-Chill/runtime-b', ref: 'trunk', target: path.normalize('.ci/runtime-b'), targetPath: path.join(workspace, '.ci/runtime-b') },
  ]);

  assertSafeDependencyTargetPath(path.join(workspace, '.ci/runtime-a'), workspace);
  assert.throws(
    () => assertSafeDependencyTargetPath(path.join(workspace, 'outside-runtime'), workspace),
    /parent must resolve under \.ci\//
  );
} finally {
  fs.rmSync(workspace, { recursive: true, force: true });
}

process.stdout.write('Runtime dependency materialization helper passed\n');
