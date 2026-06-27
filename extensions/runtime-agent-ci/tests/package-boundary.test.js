'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const packageJson = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'package.json'), 'utf8'));

assert.equal(packageJson.bin['homeboy-run-agent-loop'], undefined, 'run-agent-loop stays an internal workflow helper, not a public package bin');
assert.equal(packageJson.bin['homeboy-artifact-fanout'], undefined, 'artifact fanout stays an internal workflow helper, not a public package bin');

console.log('runtime-agent-ci package boundary test passed');
