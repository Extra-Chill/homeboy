'use strict';

/**
 * External dependencies
 */
const path = require('path');

const WORDPRESS_EXTENSION_ROOT = path.resolve(__dirname, '..');

const HELPER_PATHS = Object.freeze({
	requestProfiler: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'request-profiler.js'),
	restDbQueryProfiler: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'rest-db-query-profiler.js'),
	externalHttpGuardrail: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'external-http-guardrail.js'),
	timingCorrelator: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'timing-correlator.js'),
	wordpressRouteLatency: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'wordpress-route-latency.js'),
	bootstrapTimeline: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'wordpress-bootstrap-timeline.js'),
	pageProfiler: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'page-profiler.js'),
	adminPageScenarios: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'admin-page-scenarios.js'),
	blockQuality: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'block-quality.js'),
	materializedSiteQuality: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'materialized-site-quality.js'),
	editorCanvasProbes: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'editor-canvas-probes.js'),
	fidelityComparison: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'fidelity-comparison.js'),
	fixtureSetup: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'fixture-setup.js'),
	fuzzManifestContracts: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'fuzz-manifest-contracts.js'),
	wordpressFuzzManifestValidator: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'wordpress-fuzz-manifest-validator.js'),
	wpCodeboxRecipeHelper: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'wp-codebox-recipe-helper.js'),
	wpCodeboxArtifacts: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'wp-codebox-artifacts.js'),
	wpCodeboxBrowserCoverage: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'wp-codebox-browser-coverage.js'),
});

const HELPER_ENV = Object.freeze({
	wordpressFuzzManifestValidator: 'HOMEBOY_WORDPRESS_FUZZ_MANIFEST_VALIDATOR',
	wpCodeboxRecipeHelper: 'HOMEBOY_WP_CODEBOX_RECIPE_HELPER',
	wpCodeboxArtifacts: 'HOMEBOY_WP_CODEBOX_ARTIFACT_HELPER',
	wpCodeboxBrowserCoverage: 'HOMEBOY_WP_CODEBOX_BROWSER_COVERAGE_HELPER',
});

function getWordPressHelperManifest() {
	return {
		version: 1,
		revision: 2,
		extensionRoot: WORDPRESS_EXTENSION_ROOT,
		manifestPath: __filename,
		helpers: { ...HELPER_PATHS },
		helperEnv: { ...HELPER_ENV },
	};
}

module.exports = {
	getWordPressHelperManifest,
	WORDPRESS_HELPER_MANIFEST: getWordPressHelperManifest(),
};
