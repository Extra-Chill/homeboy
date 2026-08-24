'use strict';

const runtimeSelection = require('../../../wordpress/lib/wp-codebox-runtime-selection');
const { minimum_version: REQUIRED_WP_CODEBOX_VERSION } = require('../wp-codebox.json');

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
