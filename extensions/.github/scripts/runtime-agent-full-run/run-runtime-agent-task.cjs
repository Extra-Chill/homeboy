#!/usr/bin/env node
'use strict';

const path = require('node:path');
const { spawnSync } = require('node:child_process');

const target = path.resolve(__dirname, '../../../runtime-agent-ci/scripts/run-headless-loop.cjs');
const args = process.argv.slice(2);
const forwardedArgs = args.length > 0 && !args[0].startsWith('--')
  ? ['--spec', args[0], ...args.slice(1)]
  : args;
const result = spawnSync(process.execPath, [target, ...forwardedArgs], { stdio: 'inherit' });

if (result.error) {
  throw result.error;
}

process.exit(result.status || 0);
