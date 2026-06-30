'use strict';

const WP_CODEBOX_RUNTIME_ACTION_TYPES = Object.freeze([
	'action_auth',
	'admin_page',
	'admin_action',
	'admin_post',
	'ajax_action',
	'browser',
	'browser_corpus',
	'browser_probe',
	'cache_observation',
	'cron_event',
	'db_operation',
	'crud_operation',
	'editor_open',
	'hook_run',
	'nonce',
	'page',
	'php',
	'query_observation',
	'resource_crud',
	'rest_request',
	'rest_fixture',
	'session',
	'write_observation',
	'wp_cli',
]);

const WP_CODEBOX_RUNTIME_ACTION_CONTRACT_FIELDS = Object.freeze({
	rest_request: Object.freeze(['actions.rest_request', 'runtime_actions.rest_request', 'wordpress_runtime_actions.rest_request']),
	crud_operation: Object.freeze(['actions.crud_operation', 'runtime_actions.crud_operation', 'wordpress_runtime_actions.crud_operation']),
	db_operation: Object.freeze(['actions.db_operation', 'runtime_actions.db_operation', 'wordpress_runtime_actions.db_operation', 'schemas.wordpressDb.operation']),
	db_query: Object.freeze(['actions.db_query', 'runtime_actions.db_query', 'wordpress_runtime_actions.db_query', 'schemas.wordpressDb.operation']),
	query_observation: Object.freeze(['actions.query_observation', 'runtime_actions.query_observation', 'wordpress_runtime_actions.query_observation', 'schemas.wordpressObservability.queryObservation', 'schemas.wordpressRuntime.queryObservation']),
	cache_observation: Object.freeze(['actions.cache_observation', 'runtime_actions.cache_observation', 'wordpress_runtime_actions.cache_observation', 'schemas.wordpressObservability.cacheObservation', 'schemas.wordpressRuntime.cacheObservation']),
	write_observation: Object.freeze(['actions.write_observation', 'runtime_actions.write_observation', 'wordpress_runtime_actions.write_observation', 'schemas.wordpressObservability.writeObservation', 'schemas.wordpressRuntime.writeObservation']),
	resource_crud: Object.freeze(['actions.resource_crud', 'runtime_actions.resource_crud', 'wordpress_runtime_actions.resource_crud', 'schemas.wordpressRuntime.resourceCrud', 'schemas.wordpressRuntime.resourceCRUD']),
	admin_page_load: Object.freeze(['actions.admin_page_load', 'runtime_actions.admin_page_load', 'wordpress_runtime_actions.admin_page_load']),
	admin_action: Object.freeze(['actions.admin_action', 'runtime_actions.admin_action', 'wordpress_runtime_actions.admin_action', 'schemas.wordpressRuntime.adminAction']),
	ajax_action: Object.freeze(['actions.ajax_action', 'runtime_actions.ajax_action', 'wordpress_runtime_actions.ajax_action', 'schemas.wordpressRuntime.ajaxAction']),
	admin_post: Object.freeze(['actions.admin_post', 'runtime_actions.admin_post', 'wordpress_runtime_actions.admin_post', 'schemas.wordpressRuntime.adminPost']),
	hook_run: Object.freeze(['actions.hook_run', 'runtime_actions.hook_run', 'wordpress_runtime_actions.hook_run', 'schemas.wordpressRuntime.hookRun', 'schemas.wordpressRuntime.hook']),
	cron_event: Object.freeze(['actions.cron_event', 'runtime_actions.cron_event', 'wordpress_runtime_actions.cron_event', 'schemas.wordpressRuntime.cronEvent', 'schemas.wordpressRuntime.cron']),
	wp_cli: Object.freeze(['actions.wp_cli', 'runtime_actions.wp_cli', 'wordpress_runtime_actions.wp_cli', 'schemas.wordpressRuntime.wpCli', 'schemas.wordpressRuntime.wpCLI']),
	rest_fixture: Object.freeze(['actions.rest_fixture', 'runtime_actions.rest_fixture', 'wordpress_runtime_actions.rest_fixture', 'schemas.wordpressRuntime.restFixture', 'schemas.wordpressRuntime.restFixtureGeneration']),
	browser_corpus: Object.freeze(['actions.browser_corpus', 'runtime_actions.browser_corpus', 'wordpress_runtime_actions.browser_corpus', 'schemas.wordpressRuntime.browserCorpus']),
	replay_case: Object.freeze(['actions.replay_case', 'runtime_actions.replay_case', 'wordpress_runtime_actions.replay_case', 'schemas.wordpressRuntime.replayCase']),
	minimize_case: Object.freeze(['actions.minimize_case', 'runtime_actions.minimize_case', 'wordpress_runtime_actions.minimize_case', 'schemas.wordpressRuntime.minimizeCase']),
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
