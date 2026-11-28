'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const source = path.resolve(__dirname, '..');
const snapshotRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wordpress-extension-'));
const snapshot = path.join(snapshotRoot, 'extensions', 'wordpress');

try {
	fs.cpSync(path.join(source, 'lib'), path.join(snapshot, 'lib'), { recursive: true });
	fs.copyFileSync(path.join(source, 'wordpress.json'), path.join(snapshot, 'wordpress.json'));
	const { loadWordPressHelperManifest, loadWordPressHelper } = require('../lib/wordpress-helper-consumer');
	process.env.HOMEBOY_CONFIG_ROOT = snapshotRoot;
	delete process.env.HOMEBOY_WORDPRESS_HELPER_MANIFEST;
	delete process.env.HOMEBOY_WORDPRESS_EXTENSION_ROOT;
	const { manifest } = loadWordPressHelperManifest();

	assert.equal(fs.realpathSync(manifest.extensionRoot), fs.realpathSync(snapshot));
	for (const [name, envVar] of Object.entries(manifest.helperEnv)) {
		delete process.env[envVar];
		const handle = loadWordPressHelper(name, { required: true });
		assert.equal(handle.source, 'installed');
		assert.equal(handle.path, manifest.helpers[name]);
		assert.ok(handle.module);
	}
} finally {
	fs.rmSync(snapshotRoot, { recursive: true, force: true });
}

console.log('WordPress isolated named helper smoke test passed.');
