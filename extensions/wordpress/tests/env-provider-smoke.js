'use strict';

const assert = require('node:assert');
const childProcess = require('node:child_process');
const path = require('node:path');

const extensionRoot = path.resolve(__dirname, '..');
const provider = path.join(extensionRoot, 'scripts', 'env-provider.js');
const output = childProcess.execFileSync(process.execPath, [provider], {
	cwd: extensionRoot,
	encoding: 'utf8',
});
const env = JSON.parse(output);

assert.deepEqual(Object.keys(env), ['HOMEBOY_WORDPRESS_HELPER_MANIFEST']);
assert.equal(
	env.HOMEBOY_WORDPRESS_HELPER_MANIFEST,
	path.join(extensionRoot, 'lib', 'helper-manifest.js')
);

console.log('env provider smoke passed');
