'use strict';

/**
 * Internal dependencies
 */
const {
	listWordPressAdminPageScenarios,
	normalizeWordPressAdminPageScenarioInput,
} = require('./admin-page-scenarios');
const { assertPlainObject } = require('./shared');

const WORDPRESS_ADMIN_FUZZ_SURFACE_SCHEMA = 'homeboy/wordpress-admin-page-fuzz-surface/v1';
const WORDPRESS_ADMIN_FUZZ_SURFACE_DISCOVERY_SCHEMA = 'homeboy/wordpress-admin-page-fuzz-surface-discovery/v1';

const MUTATING_ACTION_TERMS = new Set([
	'activate',
	'add',
	'create',
	'deactivate',
	'delete',
	'disable',
	'edit',
	'enable',
	'export',
	'import',
	'install',
	'remove',
	'reset',
	'restore',
	'save',
	'submit',
	'trash',
	'uninstall',
	'update',
	'upload',
]);

function normalizePath(value) {
	return String(value || '')
		.replace(/^https?:\/\/[^/]+/i, '')
		.replace(/^\/+/, '/');
}

function normalizeToken(value) {
	return String(value || '')
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '_')
		.replace(/^_+|_+$/g, '') || 'surface';
}

function pathSearchParams(path) {
	const query = String(path || '').split('?')[1] || '';
	return new URLSearchParams(query);
}

function classifyWordPressAdminPageFuzzSurface(surface = {}) {
	assertPlainObject(surface, 'surface');
	const path = normalizePath(surface.path || surface.url || surface.href);
	const file = path.split('?')[0].split('/').pop() || '';

	if (file === 'index.php') {
		return 'dashboard';
	}
	if (file === 'site-editor.php') {
		return 'site_editor';
	}
	if (file === 'post-new.php' || pathSearchParams(path).has('post')) {
		return 'editor';
	}
	if (file === 'edit.php' || path.includes('/edit-tags.php') || path.includes('/users.php')) {
		return 'list_table';
	}
	if (file.startsWith('options-') || file === 'customize.php') {
		return 'settings';
	}
	if (file === 'themes.php' || file === 'theme-install.php' || file === 'plugins.php' || file === 'plugin-install.php') {
		return 'extension_management';
	}
	if (file === 'upload.php' || file === 'media-new.php') {
		return 'media';
	}
	if (file === 'admin.php' && pathSearchParams(path).has('page')) {
		return 'registered_admin_page';
	}
	return 'admin_page';
}

function getWordPressAdminPageFuzzSkipReasons(surface = {}, options = {}) {
	assertPlainObject(surface, 'surface');
	assertPlainObject(options, 'options');
	const reasons = [];
	const missingParams = Array.isArray(surface.missingParams) ? surface.missingParams : [];
	if (surface.enabled === false || surface.disabled === true) {
		reasons.push('disabled');
	}
	if (missingParams.length > 0) {
		reasons.push('missing_params');
	}
	if (surface.setup?.required === true && options.includeSetupRequired !== true) {
		reasons.push('setup_required');
	}
	if (surface.capability && Array.isArray(options.capabilities) && !options.capabilities.includes(surface.capability)) {
		reasons.push('capability_unavailable');
	}
	return [...new Set([...(Array.isArray(surface.skipReasons) ? surface.skipReasons : []), ...reasons])].sort();
}

function getWordPressAdminPageFuzzUnsafeReasons(surface = {}, options = {}) {
	assertPlainObject(surface, 'surface');
	assertPlainObject(options, 'options');
	const reasons = [];
	const rawPath = String(surface.path || surface.url || surface.href || '');
	const path = normalizePath(rawPath);
	const method = String(surface.method || 'GET').toUpperCase();
	const params = pathSearchParams(path);
	const action = normalizeToken(params.get('action') || params.get('action2') || surface.action || '');

	if (!path) {
		reasons.push('missing_path');
	} else if (/^https?:\/\//i.test(rawPath) && !rawPath.match(/\/wp-admin\//i)) {
		reasons.push('external_url');
	} else if (!path.startsWith('/wp-admin/')) {
		reasons.push('non_admin_path');
	}
	if (!['GET', 'HEAD'].includes(method)) {
		reasons.push('mutating_method');
	}
	if (MUTATING_ACTION_TERMS.has(action)) {
		reasons.push('mutating_action');
	}
	if (Array.isArray(surface.interactions) && surface.interactions.length > 0 && options.allowInteractions !== true) {
		reasons.push('interactions_not_classified_safe');
	}
	if (surface.mutates === true || surface.destructive === true) {
		reasons.push('declared_mutating_surface');
	}

	return [...new Set([...(Array.isArray(surface.unsafeReasons) ? surface.unsafeReasons : []), ...reasons])].sort();
}

function normalizeWordPressAdminPageFuzzSurface(input, options = {}) {
	assertPlainObject(options, 'options');
	const scenario = normalizeWordPressAdminPageScenarioInput(input, {
		...options,
		allowUnresolved: true,
	});
	const surfaceKind = scenario.surfaceKind || classifyWordPressAdminPageFuzzSurface(scenario);
	const skipReasons = getWordPressAdminPageFuzzSkipReasons(scenario, options);
	const unsafeReasons = getWordPressAdminPageFuzzUnsafeReasons(scenario, options);
	let status = 'ready';
	if (unsafeReasons.length > 0 && options.includeUnsafe !== true) {
		status = 'unsafe';
	} else if (skipReasons.length > 0) {
		status = 'skipped';
	}

	return {
		schema: WORDPRESS_ADMIN_FUZZ_SURFACE_SCHEMA,
		id: normalizeToken(scenario.id || scenario.path),
		label: scenario.label || scenario.id || scenario.path,
		path: scenario.path,
		surfaceKind,
		status,
		skipReasons,
		unsafeReasons,
		method: String(scenario.method || 'GET').toUpperCase(),
		ready: scenario.ready || {},
		metadata: {
			source: 'wordpress_admin_page_scenario',
			params: Array.isArray(scenario.params) ? scenario.params : [],
			missingParams: Array.isArray(scenario.missingParams) ? scenario.missingParams : [],
		},
	};
}

function discoverWordPressAdminPageFuzzSurfaces(options = {}) {
	assertPlainObject(options, 'options');
	const scenarios = Array.isArray(options.surfaces)
		? options.surfaces
		: listWordPressAdminPageScenarios(options);
	const surfaces = scenarios.map((scenario) => normalizeWordPressAdminPageFuzzSurface(scenario, options));
	const totals = surfaces.reduce((counts, surface) => {
		counts.total += 1;
		counts[surface.status] = (counts[surface.status] || 0) + 1;
		counts.byKind[surface.surfaceKind] = (counts.byKind[surface.surfaceKind] || 0) + 1;
		return counts;
	}, { total: 0, ready: 0, skipped: 0, unsafe: 0, byKind: {} });

	return {
		schema: WORDPRESS_ADMIN_FUZZ_SURFACE_DISCOVERY_SCHEMA,
		type: 'wordpress-admin-page-fuzz-surface-discovery',
		surfaces,
		totals,
	};
}

module.exports = {
	WORDPRESS_ADMIN_FUZZ_SURFACE_DISCOVERY_SCHEMA,
	WORDPRESS_ADMIN_FUZZ_SURFACE_SCHEMA,
	classifyWordPressAdminPageFuzzSurface,
	discoverWordPressAdminPageFuzzSurfaces,
	getWordPressAdminPageFuzzSkipReasons,
	getWordPressAdminPageFuzzUnsafeReasons,
	normalizeWordPressAdminPageFuzzSurface,
};
