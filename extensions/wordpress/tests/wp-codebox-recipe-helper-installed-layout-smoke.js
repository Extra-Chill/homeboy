'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const sourceRoot = path.join(__dirname, '..');
const installRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wordpress-extension-install-'));
const installedExtension = path.join(installRoot, 'wordpress');

try {
  fs.cpSync(sourceRoot, installedExtension, {
    recursive: true,
    filter: (source) => !source.includes(`${path.sep}node_modules${path.sep}`) && !source.includes(`${path.sep}vendor${path.sep}`),
  });

  const helper = require(path.join(installedExtension, 'lib', 'wp-codebox-recipe-helper.js'));
  assert.equal(helper.wpCodeboxBin({ env: {}, bin: '/tmp/wp-codebox.js' }), '/tmp/wp-codebox.js');
  assert.deepEqual(helper.wpCodeboxCommand('/tmp/wp-codebox.js'), {
    command: process.execPath,
    args: ['/tmp/wp-codebox.js'],
  });
} finally {
  fs.rmSync(installRoot, { recursive: true, force: true });
}

console.log('wp-codebox recipe helper installed layout smoke passed');
