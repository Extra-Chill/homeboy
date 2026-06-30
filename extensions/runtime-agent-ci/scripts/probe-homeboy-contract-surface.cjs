#!/usr/bin/env node
'use strict';

const { probeHomeboyContractSurface } = require('../lib/homeboy-contract-surface-probe.cjs');

const result = probeHomeboyContractSurface();
process.stdout.write(`${result.message}\n`);

if (result.status === 'failed') {
  process.exitCode = 1;
}
