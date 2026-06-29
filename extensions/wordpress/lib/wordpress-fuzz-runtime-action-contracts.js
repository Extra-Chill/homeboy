'use strict';

const WP_CODEBOX_RUNTIME_ACTION_TYPES = Object.freeze([
	'action_auth',
	'admin_page',
	'admin_action',
	'admin_post',
	'ajax_action',
	'browser',
	'browser_probe',
	'db_operation',
	'crud_operation',
	'editor_open',
	'nonce',
	'page',
	'php',
	'rest_request',
	'session',
	'wp_cli',
]);

const WP_CODEBOX_RUNTIME_ACTION_CONTRACT_FIELDS = Object.freeze({
	rest_request: Object.freeze(['actions.rest_request', 'runtime_actions.rest_request', 'wordpress_runtime_actions.rest_request']),
	crud_operation: Object.freeze(['actions.crud_operation', 'runtime_actions.crud_operation', 'wordpress_runtime_actions.crud_operation']),
	db_operation: Object.freeze(['actions.db_operation', 'runtime_actions.db_operation', 'wordpress_runtime_actions.db_operation', 'schemas.wordpressDb.operation']),
	db_query: Object.freeze(['actions.db_query', 'runtime_actions.db_query', 'wordpress_runtime_actions.db_query', 'schemas.wordpressDb.operation']),
	admin_page_load: Object.freeze(['actions.admin_page_load', 'runtime_actions.admin_page_load', 'wordpress_runtime_actions.admin_page_load']),
	admin_action: Object.freeze(['actions.admin_action', 'runtime_actions.admin_action', 'wordpress_runtime_actions.admin_action', 'schemas.wordpressRuntime.adminAction']),
	ajax_action: Object.freeze(['actions.ajax_action', 'runtime_actions.ajax_action', 'wordpress_runtime_actions.ajax_action', 'schemas.wordpressRuntime.ajaxAction']),
	admin_post: Object.freeze(['actions.admin_post', 'runtime_actions.admin_post', 'wordpress_runtime_actions.admin_post', 'schemas.wordpressRuntime.adminPost']),
	action_auth: Object.freeze(['actions.action_auth', 'runtime_actions.action_auth', 'wordpress_runtime_actions.action_auth', 'schemas.wordpressRuntime.actionAuth']),
	nonce: Object.freeze(['actions.nonce', 'runtime_actions.nonce', 'wordpress_runtime_actions.nonce', 'schemas.wordpressRuntime.nonce']),
	session: Object.freeze(['actions.session', 'runtime_actions.session', 'wordpress_runtime_actions.session', 'schemas.wordpressRuntime.session']),
});

function wpCodeboxRuntimeActionTarget(type) {
	return {
		kind: 'runtime-action',
		id: `runtime-action:${type}`,
		entrypoint: type,
	};
}

module.exports = {
	WP_CODEBOX_RUNTIME_ACTION_CONTRACT_FIELDS,
	WP_CODEBOX_RUNTIME_ACTION_TYPES,
	wpCodeboxRuntimeActionTarget,
};
