#!/usr/bin/env node
'use strict';

const { parseJsonInput, writeGithubOutput } = require('./lib/common.cjs');

function main() {
  writeGithubOutput(projectCallbackData(process.env));
}

function projectCallbackData(env) {
  const parsed = parseJsonInput('callback_data', env.CALLBACK_DATA || '{}', '', {});
  const callbackData = parsed === null || parsed === false ? {} : parsed;
  if (!callbackData || Array.isArray(callbackData) || typeof callbackData !== 'object') {
    throw new Error('Invalid callback_data: expected JSON object.');
  }
  return { callback_data_json: JSON.stringify(callbackData) };
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}

module.exports = { projectCallbackData };
