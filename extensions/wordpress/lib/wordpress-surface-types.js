'use strict';

const WORDPRESS_SURFACE_TYPES = Object.freeze([
	'admin-page',
	'ajax-action',
	'block',
	'capability',
	'cron-event',
	'crud-resource',
	'database-table',
	'db-query',
	'external-http',
	'frontend-url',
	'hook',
	'media',
	'option',
	'post-type',
	'rest-route',
	'role',
	'setting',
	'taxonomy',
	'user',
	'wp-cli-command',
]);

const WORDPRESS_SURFACE_TYPE_SET = new Set(WORDPRESS_SURFACE_TYPES);

const WORDPRESS_SURFACE_TYPE_ALIASES = new Map([
	['action', 'hook'],
	['admin', 'admin-page'],
	['admin_page', 'admin-page'],
	['ajax', 'ajax-action'],
	['ajax_action', 'ajax-action'],
	['ajax-action', 'ajax-action'],
	['block-type', 'block'],
	['block_type', 'block'],
	['capabilities', 'capability'],
	['cron', 'cron-event'],
	['crud', 'crud-resource'],
	['crud_resource', 'crud-resource'],
	['database', 'database-table'],
	['database_query', 'db-query'],
	['database-query', 'db-query'],
	['database_table', 'database-table'],
	['db', 'database-table'],
	['db_table', 'database-table'],
	['db-table', 'database-table'],
	['db_query', 'db-query'],
	['external_http', 'external-http'],
	['filter', 'hook'],
	['frontend', 'frontend-url'],
	['frontend_url', 'frontend-url'],
	['http', 'external-http'],
	['http-request', 'external-http'],
	['http_request', 'external-http'],
	['option_setting', 'option'],
	['post_type', 'post-type'],
	['rest', 'rest-route'],
	['rest_route', 'rest-route'],
	['roles', 'role'],
	['setting', 'setting'],
	['settings', 'setting'],
	['taxonomy-term', 'taxonomy'],
	['taxonomy_term', 'taxonomy'],
	['users', 'user'],
	['wp-cli', 'wp-cli-command'],
]);

const WORDPRESS_RUNTIME_SURFACE_TYPES = Object.freeze({
	'admin-page': 'admin_page',
	'ajax-action': 'ajax_action',
	block: 'block',
	'database-table': 'db_table',
	'frontend-url': 'frontend_url',
	'rest-route': 'rest_route',
});

const WORDPRESS_RUNTIME_SURFACE_TYPE_SET = new Set(Object.values(WORDPRESS_RUNTIME_SURFACE_TYPES));

const WORDPRESS_COVERAGE_SURFACE_TYPES = Object.freeze({
	...WORDPRESS_RUNTIME_SURFACE_TYPES,
	capability: 'capability',
	'cron-event': 'cron_event',
	'crud-resource': 'crud_resource',
	'db-query': 'db_query',
	'external-http': 'external_http',
	hook: 'hook',
	media: 'media',
	option: 'option',
	'post-type': 'post_type',
	role: 'role',
	setting: 'setting',
	taxonomy: 'taxonomy',
	user: 'user',
	'wp-cli-command': 'wp_cli_command',
});

const WORDPRESS_SURFACE_COLLECTION_TYPE_BY_KEY = Object.freeze({
	hooks: 'hook',
	cron: 'cron-event',
	cron_events: 'cron-event',
	cronEvents: 'cron-event',
	capabilities: 'capability',
	crud: 'crud-resource',
	crudResources: 'crud-resource',
	crud_resources: 'crud-resource',
	database: 'database-table',
	db: 'database-table',
	databaseTables: 'database-table',
	database_tables: 'database-table',
	dbQueries: 'db-query',
	db_queries: 'db-query',
	options: 'option',
	settings: 'setting',
	post_types: 'post-type',
	postTypes: 'post-type',
	taxonomies: 'taxonomy',
	media: 'media',
	users: 'user',
	roles: 'role',
	blocks: 'block',
	frontend: 'frontend-url',
	frontendUrls: 'frontend-url',
	frontend_urls: 'frontend-url',
	admin: 'admin-page',
	adminPages: 'admin-page',
	admin_pages: 'admin-page',
	ajax: 'ajax-action',
	actions: 'ajax-action',
	ajaxActions: 'ajax-action',
	ajax_actions: 'ajax-action',
	externalHttp: 'external-http',
	external_http: 'external-http',
	http: 'external-http',
	httpRequests: 'external-http',
	http_requests: 'external-http',
	rest: 'rest-route',
	restRoutes: 'rest-route',
	routes: 'rest-route',
});

const WORDPRESS_SURFACE_COLLECTION_KEYS = Object.freeze([
	'surfaces',
	...Object.keys(WORDPRESS_SURFACE_COLLECTION_TYPE_BY_KEY),
]);

const WORDPRESS_RUNTIME_SURFACE_ID_PREFIXES = Object.freeze({
	admin_page: 'admin',
	ajax_action: 'ajax',
	block: 'block',
	capability: 'capability',
	cron_event: 'cron',
	crud_resource: 'crud',
	db_table: 'db',
	db_query: 'db-query',
	external_http: 'http',
	frontend_url: 'frontend',
	hook: 'hook',
	media: 'media',
	option: 'option',
	post_type: 'post-type',
	rest_route: 'rest',
	role: 'role',
	setting: 'setting',
	taxonomy: 'taxonomy',
	user: 'user',
	wp_cli_command: 'wp-cli',
});

function normalizeSurfaceTypeKey(value) {
	return String(value || '').trim().toLowerCase().replace(/\s+/g, '-');
}

function normalizeWordPressSurfaceType(value, options = {}) {
	const key = normalizeSurfaceTypeKey(value);
	const canonical = WORDPRESS_SURFACE_TYPE_ALIASES.get(key) || (WORDPRESS_SURFACE_TYPE_SET.has(key) ? key : '');
	if (canonical) {
		return options.runtime ? WORDPRESS_RUNTIME_SURFACE_TYPES[canonical] || '' : canonical;
	}
	return options.allowUnknown ? String(value || '').trim() : '';
}

function isWordPressSurfaceType(value) {
	return WORDPRESS_SURFACE_TYPE_SET.has(value);
}

function normalizeWordPressRuntimeSurfaceType(value) {
	const runtimeType = normalizeWordPressSurfaceType(value, { runtime: true });
	if (runtimeType) {
		return runtimeType;
	}
	const key = normalizeSurfaceTypeKey(value);
	return WORDPRESS_RUNTIME_SURFACE_TYPE_SET.has(key) ? key : '';
}

function normalizeWordPressCoverageSurfaceType(value) {
	const canonical = normalizeWordPressSurfaceType(value);
	if (canonical) {
		return WORDPRESS_COVERAGE_SURFACE_TYPES[canonical] || canonical.replace(/-/g, '_');
	}
	const key = normalizeSurfaceTypeKey(value);
	return key ? key.replace(/-/g, '_') : '';
}

function wordpressSurfaceTypeFromCollectionKey(key) {
	return WORDPRESS_SURFACE_COLLECTION_TYPE_BY_KEY[key];
}

module.exports = {
	WORDPRESS_RUNTIME_SURFACE_ID_PREFIXES,
	WORDPRESS_RUNTIME_SURFACE_TYPES,
	WORDPRESS_SURFACE_COLLECTION_KEYS,
	WORDPRESS_SURFACE_COLLECTION_TYPE_BY_KEY,
	WORDPRESS_SURFACE_TYPES,
	isWordPressSurfaceType,
	normalizeWordPressCoverageSurfaceType,
	normalizeWordPressRuntimeSurfaceType,
	normalizeWordPressSurfaceType,
	wordpressSurfaceTypeFromCollectionKey,
};
