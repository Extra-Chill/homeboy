'use strict';

const WP_CODEBOX_RUNTIME_ACTION_TYPES = Object.freeze([
	'admin_page',
	'browser',
	'browser_probe',
	'crud_operation',
	'editor_open',
	'page',
	'php',
	'rest_request',
	'wp_cli',
]);

function wpCodeboxRuntimeActionTarget(type) {
	return {
		kind: 'runtime-action',
		id: `runtime-action:${type}`,
		entrypoint: type,
	};
}

module.exports = {
	WP_CODEBOX_RUNTIME_ACTION_TYPES,
	wpCodeboxRuntimeActionTarget,
};
