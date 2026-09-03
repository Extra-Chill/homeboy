'use strict';

const fs = require('node:fs');
const path = require('node:path');

const runtimeSelectionCandidates = [
	path.resolve(__dirname, '../../../extensions/wordpress/lib/wp-codebox-runtime-selection.js'),
	path.resolve(__dirname, '../../../wordpress/lib/wp-codebox-runtime-selection.js'),
];
const runtimeSelectionPath = runtimeSelectionCandidates.find((candidate) => fs.existsSync(candidate));

if (!runtimeSelectionPath) {
	throw new Error(
		`WP Codebox runtime selection requires the installed WordPress extension. Probed:\n${runtimeSelectionCandidates.map((candidate) => `  - ${candidate}`).join('\n')}\nRun homeboy extension install wordpress.`
	);
}

const runtimeSelection = require(runtimeSelectionPath);
const extensionManifestCandidates = [
	path.resolve(__dirname, '../../../extensions/wordpress/wordpress.json'),
	path.resolve(__dirname, '../../../wordpress/wordpress.json'),
];
const extensionManifestPath = extensionManifestCandidates.find((candidate) => fs.existsSync(candidate));
if (!extensionManifestPath) {
	throw new Error(`WP Codebox runtime selection requires the WordPress extension manifest. Probed:\n${extensionManifestCandidates.map((candidate) => `  - ${candidate}`).join('\n')}`);
}
const { minimum_version: REQUIRED_WP_CODEBOX_VERSION } = require(extensionManifestPath).wp_codebox;

const withRequiredVersion = (options = {}) => ({
	...options,
	requiredVersion: options.requiredVersion || REQUIRED_WP_CODEBOX_VERSION,
});

module.exports = {
	...runtimeSelection,
	REQUIRED_WP_CODEBOX_VERSION,
	preflightWpCodeboxCommand: (command, options) =>
		runtimeSelection.preflightWpCodeboxCommand(command, withRequiredVersion(options)),
	preflightWpCodeboxRuntime: (options) =>
		runtimeSelection.preflightWpCodeboxRuntime(withRequiredVersion(options)),
};
