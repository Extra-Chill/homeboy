#!/usr/bin/env node
'use strict';

const path = require('node:path');
const { spawnSync } = require('node:child_process');

const target = path.resolve(__dirname, '../../../.github/scripts/runtime-agent-full-run/run-host-runner-lifecycle.cjs');

if (require.main === module) {
  const result = spawnSync(process.execPath, [target, ...process.argv.slice(2)], { stdio: 'inherit' });
  process.exit(result.status || 0);
}

module.exports = require('../../../runtime-agent-ci/lib/workspace-publication-lifecycle.cjs');
