#!/usr/bin/env node
'use strict';

const path = require('node:path');

process.env.HOMEBOY_RUNTIME_CONTRACT_CONSTANTS_FIXTURE = process.env.HOMEBOY_RUNTIME_CONTRACT_CONSTANTS_FIXTURE || path.join(
  __dirname,
  '..',
  'tests',
  'fixtures',
  'homeboy-runtime-contract-constants.generated.json'
);

const { probeHomeboyContractSurface } = require('../lib/homeboy-contract-surface-probe.cjs');

const result = probeHomeboyContractSurface();
process.stdout.write(`${result.message}\n`);

if (result.status === 'failed') {
  process.exitCode = 1;
}
