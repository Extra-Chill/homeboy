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
assert.equal(manifest.revision, 2);
assert.equal(manifest.manifestPath, require.resolve('../lib/helper-manifest'));
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
	manifest.helpers.restDbQueryProfiler,
	path.resolve(__dirname, '..', 'lib', 'rest-db-query-profiler.js')
);
assert.equal(
	manifest.helpers.externalHttpGuardrail,
	path.resolve(__dirname, '..', 'lib', 'external-http-guardrail.js')
);
assert.equal(
	manifest.helpers.timingCorrelator,
	path.resolve(__dirname, '..', 'lib', 'timing-correlator.js')
);
assert.equal(
	manifest.helpers.bootstrapTimeline,
	path.resolve(__dirname, '..', 'lib', 'wordpress-bootstrap-timeline.js')
);
assert.equal(
	manifest.helpers.pageProfiler,
	path.resolve(__dirname, '..', 'lib', 'page-profiler.js')
);
assert.equal(
	manifest.helpers.adminPageScenarios,
	path.resolve(__dirname, '..', 'lib', 'admin-page-scenarios.js')
);
assert.equal(
	manifest.helpers.blockQuality,
	path.resolve(__dirname, '..', 'lib', 'block-quality.js')
);
assert.equal(
	manifest.helpers.materializedSiteQuality,
	path.resolve(__dirname, '..', 'lib', 'materialized-site-quality.js')
);
assert.equal(
	manifest.helpers.editorCanvasProbes,
	path.resolve(__dirname, '..', 'lib', 'editor-canvas-probes.js')
);
assert.equal(
	manifest.helpers.fuzzManifestContracts,
	path.resolve(__dirname, '..', 'lib', 'fuzz-manifest-contracts.js')
);
for (const [name, filename, envVar] of [
	['wordpressFuzzManifestValidator', 'wordpress-fuzz-manifest-validator.js', 'HOMEBOY_WORDPRESS_FUZZ_MANIFEST_VALIDATOR'],
	['wpCodeboxRecipeHelper', 'wp-codebox-recipe-helper.js', 'HOMEBOY_WP_CODEBOX_RECIPE_HELPER'],
	['wpCodeboxArtifacts', 'wp-codebox-artifacts.js', 'HOMEBOY_WP_CODEBOX_ARTIFACT_HELPER'],
	['wpCodeboxBrowserCoverage', 'wp-codebox-browser-coverage.js', 'HOMEBOY_WP_CODEBOX_BROWSER_COVERAGE_HELPER'],
]) {
	assert.equal(manifest.helpers[name], path.resolve(__dirname, '..', 'lib', filename));
	assert.equal(manifest.helperEnv[name], envVar);
	assert.doesNotThrow(() => require(manifest.helpers[name]));
}
console.log('helper manifest smoke passed');
