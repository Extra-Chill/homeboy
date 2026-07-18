'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const source = path.resolve(__dirname, '..');
const snapshotRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wordpress-extension-'));
const snapshot = path.join(snapshotRoot, 'wordpress');

try {
	fs.cpSync(source, snapshot, { recursive: true });
	const { getWordPressHelperManifest } = require(path.join(snapshot, 'lib', 'helper-manifest.js'));
	const manifest = getWordPressHelperManifest();

	assert.equal(fs.realpathSync(manifest.extensionRoot), fs.realpathSync(snapshot));
	assert.doesNotThrow(
		() => require(manifest.helpers.wpCodeboxArtifacts),
		`Expected isolated WordPress helper "wpCodeboxArtifacts" to load from ${manifest.helpers.wpCodeboxArtifacts}`
	);
} finally {
	fs.rmSync(snapshotRoot, { recursive: true, force: true });
}

console.log('WordPress isolated WP Codebox artifact helper smoke test passed.');
