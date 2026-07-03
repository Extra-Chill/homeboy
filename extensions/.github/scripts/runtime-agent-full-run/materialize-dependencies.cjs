#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const { run } = require('./lib/common.cjs');
const {
  assertSafeDependencyTargetPath,
  dependencyEntries,
  resolveDependencyTarget,
  resolvePlan,
  runtimeDependencyEntries,
} = require('../../../runtime-agent-ci/lib/materialize-dependencies.cjs');

function main() {
  const printPlan = process.argv.includes('--print-plan');
  const entries = dependencyEntries(process.env);
  const plan = resolvePlan(entries, printPlan, { workspace: process.env.GITHUB_WORKSPACE || process.cwd() });
  if (printPlan) {
    process.stdout.write(`${JSON.stringify(plan, null, 2)}\n`);
    return;
  }
  for (const item of plan) {
    assertSafeDependencyTargetPath(item.targetPath, process.env.GITHUB_WORKSPACE || process.cwd());
    fs.rmSync(item.targetPath, { recursive: true, force: true });
    process.stdout.write(`Checking out validation dependency ${item.repo}@${item.ref} into ${item.target}\n`);
    run('gh', ['repo', 'clone', item.repo, item.targetPath, '--', '--depth=1']);
    run('git', ['-C', item.targetPath, 'fetch', '--depth=1', 'origin', item.ref]);
    run('git', ['-C', item.targetPath, 'checkout', '--quiet', 'FETCH_HEAD']);
  }
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}

module.exports = { assertSafeDependencyTargetPath, dependencyEntries, resolveDependencyTarget, resolvePlan, runtimeDependencyEntries };
