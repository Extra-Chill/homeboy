import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import helperConsumer from '../lib/wordpress-helper-consumer.js';

const {
	loadWordPressHelper,
	loadWordPressHelperManifest,
	loadWordPressLibHelper,
	wordpressHelperPath,
	wordpressLibHelperPath,
} = helperConsumer;

const fixture = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-helper-consumer-'));
const env = { ...process.env };

try {
	delete process.env.HOMEBOY_WORDPRESS_HELPER_MANIFEST;
	delete process.env.HOMEBOY_WORDPRESS_EXTENSION_ROOT;
	delete process.env.HOMEBOY_WP_CODEBOX_ARTIFACT_HELPER;
	process.env.HOMEBOY_CONFIG_ROOT = fixture;

	const uninstalled = loadWordPressHelper('wpCodeboxArtifacts');
	assert.equal(uninstalled.found, false);
	assert.equal(uninstalled.source, 'installed');
	assert.match(uninstalled.reason, /homeboy extension install https:\/\/github\.com\/Extra-Chill\/homeboy --id wordpress/);
	assert.match(uninstalled.reason, /HOMEBOY_WORDPRESS_HELPER_MANIFEST/);
	assert.match(uninstalled.reason, /HOMEBOY_WP_CODEBOX_ARTIFACT_HELPER/);
	const extensionRoot = path.join(fixture, 'extensions', 'wordpress');
	const libDir = path.join(extensionRoot, 'lib');
	fs.mkdirSync(libDir, { recursive: true });

	const namedHelper = path.join(libDir, 'named-helper.js');
	const libHelper = path.join(libDir, 'lib-helper.js');
	const manifestPath = path.join(libDir, 'helper-manifest.js');

	fs.writeFileSync(namedHelper, "module.exports = { answer: 42 };\n", 'utf8');
	fs.writeFileSync(libHelper, "module.exports = { label: 'from-lib' };\n", 'utf8');
	fs.writeFileSync(
		manifestPath,
		`module.exports = {
	getWordPressHelperManifest: () => ({
		version: 1,
		extensionRoot: ${JSON.stringify(extensionRoot)},
		helpers: {
			namedHelper: ${JSON.stringify(namedHelper)},
			missingFile: ${JSON.stringify(path.join(libDir, 'missing.js'))},
		},
	}),
};\n`,
		'utf8'
	);

	const manifestHandle = loadWordPressHelperManifest({ manifestPath });
	assert.equal(manifestHandle.found, true);
	assert.equal(manifestHandle.path, manifestPath);
	assert.equal(manifestHandle.manifest.helpers.namedHelper, namedHelper);
	assert.equal(manifestHandle.source, 'explicit');
	assert.equal(loadWordPressHelperManifest().path, manifestPath);
	assert.equal(loadWordPressHelper('namedHelper').source, 'installed');

	const staleHandle = loadWordPressHelper('wpCodeboxArtifacts');
	assert.equal(staleHandle.found, false);
	assert.match(staleHandle.reason, /stale/);
	assert.match(staleHandle.reason, /--replace/);
	assert.equal(loadWordPressHelper('lib-helper.js').found, false, 'named lookup never infers a lib filename');
	const missingFile = loadWordPressHelper('missingFile');
	assert.equal(missingFile.found, false);
	assert.match(missingFile.reason, /does not exist.*missing\.js/);

	process.env.HOMEBOY_WORDPRESS_EXTENSION_ROOT = extensionRoot;
	assert.equal(loadWordPressHelper('namedHelper').source, 'HOMEBOY_WORDPRESS_EXTENSION_ROOT');
	process.env.HOMEBOY_WORDPRESS_HELPER_MANIFEST = path.join(fixture, 'invalid-override.js');
	assert.equal(loadWordPressHelper('namedHelper').found, false);
	assert.equal(loadWordPressHelper('namedHelper').source, 'HOMEBOY_WORDPRESS_HELPER_MANIFEST');
	assert.equal(loadWordPressHelper('namedHelper', { manifestPath }).found, true);

	process.env.HOMEBOY_WP_CODEBOX_ARTIFACT_HELPER = namedHelper;
	const override = loadWordPressHelper('wpCodeboxArtifacts');
	assert.equal(override.module.answer, 42);
	assert.equal(override.source, 'HOMEBOY_WP_CODEBOX_ARTIFACT_HELPER');
	assert.equal(loadWordPressHelper('wpCodeboxArtifacts', { override: libHelper }).module.label, 'from-lib');
	assert.equal(loadWordPressHelper('otherName', { envVar: 'HOMEBOY_WP_CODEBOX_ARTIFACT_HELPER' }).module.answer, 42);

	assert.equal(wordpressHelperPath('namedHelper', { manifestPath }), namedHelper);
	assert.equal(wordpressLibHelperPath('lib-helper.js', { manifestPath }), libHelper);

	const namedHandle = loadWordPressHelper('namedHelper', { manifestPath });
	assert.equal(namedHandle.found, true);
	assert.equal(namedHandle.path, namedHelper);
	assert.equal(namedHandle.module.answer, 42);

	const libHandle = loadWordPressLibHelper('lib-helper.js', { manifestPath });
	assert.equal(libHandle.found, true);
	assert.equal(libHandle.path, libHelper);
	assert.equal(libHandle.module.label, 'from-lib');

	const missingHandle = loadWordPressHelper('missingHelper', { manifestPath });
	assert.equal(missingHandle.found, false);
	assert.match(missingHandle.reason, /could not be resolved/);

	assert.throws(
		() => loadWordPressHelper('missingHelper', { manifestPath, required: true }),
		/could not be resolved/
	);

	const missingManifest = loadWordPressHelperManifest({ manifestPath: path.join(fixture, 'missing.js') });
	assert.equal(missingManifest.found, false);
	assert.match(missingManifest.reason, /does not exist/);

	console.log('WordPress helper consumer smoke passed.');
} finally {
	for (const key of ['HOMEBOY_CONFIG_ROOT', 'HOMEBOY_WORDPRESS_HELPER_MANIFEST', 'HOMEBOY_WORDPRESS_EXTENSION_ROOT', 'HOMEBOY_WP_CODEBOX_ARTIFACT_HELPER']) {
		if (env[key] === undefined) {
			delete process.env[key];
		} else {
			process.env[key] = env[key];
		}
	}
	fs.rmSync(fixture, { recursive: true, force: true });
}
