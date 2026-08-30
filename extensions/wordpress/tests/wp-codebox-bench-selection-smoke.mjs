import assert from 'node:assert/strict';
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

console.log('wp-codebox bench selection smoke ok');
