'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');

const {
	WORDPRESS_HELPER_MANIFEST,
	getWordPressHelperManifest,
} = require('../lib/helper-manifest');

const manifest = getWordPressHelperManifest();

assert.equal(manifest.version, 1);
assert.deepEqual(manifest, WORDPRESS_HELPER_MANIFEST);
assert.equal(manifest.extensionRoot, path.resolve(__dirname, '..'));

for (const [name, helperPath] of Object.entries(manifest.helpers)) {
	assert.ok(path.isAbsolute(helperPath), `${name} helper path is absolute`);
	assert.ok(fs.existsSync(helperPath), `${name} helper exists at ${helperPath}`);
}

assert.equal(
	manifest.helpers.requestProfiler,
	path.resolve(__dirname, '..', 'lib', 'request-profiler.js')
);
assert.equal(
	manifest.helpers.timingCorrelator,
	path.resolve(__dirname, '..', 'lib', 'timing-correlator.js')
);
assert.equal(
	manifest.helpers.bootstrapTimeline,
	path.resolve(__dirname, '..', 'lib', 'wordpress-bootstrap-timeline.js')
);

console.log('helper manifest smoke passed');
