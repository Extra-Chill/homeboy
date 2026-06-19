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
	manifest.helpers.woocommerceExpensiveShipping,
	path.resolve(__dirname, '..', 'scripts', 'bench', 'lib', 'woocommerce-expensive-shipping.php')
);

console.log('helper manifest smoke passed');
