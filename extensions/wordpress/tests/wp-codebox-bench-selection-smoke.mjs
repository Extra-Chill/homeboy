import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { rigWorkloadInputs, selectedScenarioIds } from '../scripts/bench/wp-codebox-bench-selection.mjs';

assert.deepEqual(selectedScenarioIds(' boot-timing,read-heavy,boot-timing '), ['boot-timing', 'read-heavy']);

const root = path.join(path.sep, 'tmp', 'rig-workloads');
const inputs = rigWorkloadInputs(
  [path.join(root, 'BootTiming.php'), path.join(root, 'unselected-crash.php')].join(path.delimiter),
  ['boot-timing'],
  'markdown-database-integration',
);

assert.deepEqual(inputs.workloads, [{
  id: 'boot-timing',
  source: 'rig',
  overridesDiscovered: true,
  run: [{ type: 'php', file: '.homeboy/bench-rig/BootTiming.php' }],
}]);
assert.deepEqual(inputs.mounts, [{
  source: path.join(root, 'BootTiming.php'),
  target: '/wordpress/wp-content/plugins/markdown-database-integration/.homeboy/bench-rig/BootTiming.php',
  type: 'file',
  mode: 'readonly',
}]);

const componentRoot = path.join(path.sep, 'tmp', 'markdown-database-integration');
const componentInputs = rigWorkloadInputs(
  path.join(componentRoot, 'tests', 'bench', 'boot-timing.php'),
  ['boot-timing'],
  'markdown-database-integration',
  componentRoot,
);

assert.deepEqual(componentInputs.workloads, [{
  id: 'boot-timing',
  source: 'rig',
  overridesDiscovered: true,
  run: [{ type: 'php', file: 'tests/bench/boot-timing.php' }],
}]);
assert.deepEqual(componentInputs.mounts, [{
  source: path.join(componentRoot, 'tests', 'bench', 'boot-timing.php'),
  target: '/wordpress/wp-content/plugins/markdown-database-integration/tests/bench/boot-timing.php',
  type: 'file',
  mode: 'readonly',
}]);

const rigPackageRoot = path.join(path.sep, 'tmp', 'installed-rig-package');
const packageInputs = rigWorkloadInputs(
  [
    path.join(rigPackageRoot, 'tests', 'bench', 'boot-timing.php'),
    path.join(rigPackageRoot, 'tests', 'bench', 'read-heavy.php'),
  ].join(path.delimiter),
  [],
  'markdown-database-integration',
  componentRoot,
  rigPackageRoot,
);

assert.deepEqual(packageInputs.workloads, [
  {
    id: 'boot-timing',
    source: 'rig',
    overridesDiscovered: true,
    run: [{ type: 'php', file: '.homeboy/bench-rig/tests/bench/boot-timing.php' }],
  },
  {
    id: 'read-heavy',
    source: 'rig',
    overridesDiscovered: true,
    run: [{ type: 'php', file: '.homeboy/bench-rig/tests/bench/read-heavy.php' }],
  },
]);
assert.deepEqual(packageInputs.mounts, [{
  source: rigPackageRoot,
  target: '/wordpress/wp-content/plugins/markdown-database-integration/.homeboy/bench-rig',
  type: 'directory',
  mode: 'readonly',
}]);

const materializedRoot = mkdtempSync(path.join(tmpdir(), 'homeboy-bench-selection-'));
try {
  const actualRoot = path.join(materializedRoot, 'actual');
  const linkedRoot = path.join(materializedRoot, 'workspace');
  const workload = path.join(linkedRoot, 'tests', 'bench', 'boot-timing.php');
  mkdirSync(path.join(actualRoot, 'tests', 'bench'), { recursive: true });
  writeFileSync(path.join(actualRoot, 'tests', 'bench', 'boot-timing.php'), '<?php');
  symlinkSync(actualRoot, linkedRoot);

  const symlinkInputs = rigWorkloadInputs(
    workload,
    ['boot-timing'],
    'markdown-database-integration',
    realpathSync(linkedRoot),
  );

  assert.equal(symlinkInputs.workloads[0].run[0].file, 'tests/bench/boot-timing.php');
  assert.equal(
    symlinkInputs.mounts[0].target,
    '/wordpress/wp-content/plugins/markdown-database-integration/tests/bench/boot-timing.php',
  );
} finally {
  rmSync(materializedRoot, { recursive: true, force: true });
}

console.log('wp-codebox bench selection smoke ok');
