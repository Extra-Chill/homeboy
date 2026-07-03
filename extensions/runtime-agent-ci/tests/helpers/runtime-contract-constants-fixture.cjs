'use strict';

const path = require('node:path');

process.env.HOMEBOY_RUNTIME_CONTRACT_CONSTANTS_FIXTURE ||= path.join(
  __dirname,
  '..',
  'fixtures',
  'homeboy-runtime-contract-constants.generated.json'
);
