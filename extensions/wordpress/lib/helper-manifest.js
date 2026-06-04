'use strict';

/**
 * External dependencies
 */
const path = require('path');

const WORDPRESS_EXTENSION_ROOT = path.resolve(__dirname, '..');

const HELPER_PATHS = Object.freeze({
	requestProfiler: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'request-profiler.js'),
	timingCorrelator: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'timing-correlator.js'),
	bootstrapTimeline: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'wordpress-bootstrap-timeline.js'),
	blockQuality: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'block-quality.js'),
	editorCanvasProbes: path.join(WORDPRESS_EXTENSION_ROOT, 'lib', 'editor-canvas-probes.js'),
	woocommerceExpensiveShipping: path.join(WORDPRESS_EXTENSION_ROOT, 'scripts', 'bench', 'lib', 'woocommerce-expensive-shipping.php'),
});

function getWordPressHelperManifest() {
	return {
		version: 1,
		extensionRoot: WORDPRESS_EXTENSION_ROOT,
		helpers: { ...HELPER_PATHS },
	};
}

module.exports = {
	getWordPressHelperManifest,
	WORDPRESS_HELPER_MANIFEST: getWordPressHelperManifest(),
};
