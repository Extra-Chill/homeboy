#!/usr/bin/env node
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const {
  assertHeadlessDeterministicLoopFixture,
  runHeadlessDeterministicLoopFixture,
} = require(path.join(repoRoot, 'runtime-agent-ci/tests/fixtures/headless-deterministic-loop-fixture.cjs'));

assertHeadlessDeterministicLoopFixture(runHeadlessDeterministicLoopFixture());

console.log('headless deterministic loop fixture passed');
