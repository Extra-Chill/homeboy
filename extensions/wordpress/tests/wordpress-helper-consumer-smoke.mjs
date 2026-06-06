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

try {
	const extensionRoot = path.join(fixture, 'wordpress');
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
		helpers: { namedHelper: ${JSON.stringify(namedHelper)} },
	}),
};\n`,
		'utf8'
	);

	const manifestHandle = loadWordPressHelperManifest({ manifestPath });
	assert.equal(manifestHandle.found, true);
	assert.equal(manifestHandle.path, manifestPath);
	assert.equal(manifestHandle.manifest.helpers.namedHelper, namedHelper);

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
	fs.rmSync(fixture, { recursive: true, force: true });
}
