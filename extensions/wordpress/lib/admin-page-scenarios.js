'use strict';

/**
 * Internal dependencies
 */
const { profileWordPressPages } = require('./page-profiler');

const WORDPRESS_ADMIN_RESOURCE_INCLUDE = [
	'/wp-json/',
	'?rest_route=',
	'/wp-admin/',
	'/wp-content/',
	'/wp-includes/',
];

const WORDPRESS_ADMIN_RESOURCE_EXCLUDE = [
	'/wp-admin/admin-ajax.php?action=heartbeat',
];

const WORDPRESS_ADMIN_BROWSER_GATE = {
	readyMs: { warn: 2500, fail: 5000 },
	requestCount: { warn: 120, fail: 180 },
	failedRequestCount: { warn: 1, fail: 1 },
};

const WORDPRESS_ADMIN_REST_GATE = {
	restAfterReadyCount: { warn: 1, fail: 5 },
	restNetworkCount: { warn: 20, fail: 40 },
};

const DEFAULT_SCENARIO_RESOURCES = {
	includeResourceSubstrings: WORDPRESS_ADMIN_RESOURCE_INCLUDE,
	excludeResourceSubstrings: WORDPRESS_ADMIN_RESOURCE_EXCLUDE,
};

const EDITOR_READY = {
	selector: '.edit-post-layout, .interface-interface-skeleton',
};

const SITE_EDITOR_READY = {
	selector: '.edit-site, .interface-interface-skeleton, iframe[name="editor-canvas"]',
	frameName: 'editor-canvas',
	frameSelector: 'body',
};

const WORDPRESS_ADMIN_PAGE_SCENARIOS = [
	{
		id: 'dashboard',
		label: 'Dashboard',
		path: '/wp-admin/index.php',
		ready: { selector: '#dashboard-widgets' },
	},
	{
		id: 'posts-list',
		label: 'Posts list',
		path: '/wp-admin/edit.php',
		ready: { selector: 'body.post-type-post .wp-list-table, body.edit-php .wp-list-table' },
	},
	{
		id: 'pages-list',
		label: 'Pages list',
		path: '/wp-admin/edit.php?post_type=page',
		ready: { selector: 'body.post-type-page .wp-list-table' },
	},
	{
		id: 'post-editor',
		label: 'Post editor',
		path: '/wp-admin/post-new.php',
		ready: EDITOR_READY,
		setup: {
			recommended: 'Run against a site where the block editor is enabled for posts.',
		},
	},
	{
		id: 'site-editor-root',
		label: 'Site Editor root',
		path: '/wp-admin/site-editor.php',
		ready: SITE_EDITOR_READY,
		setup: {
			recommended: 'Run against a block theme.',
		},
	},
	{
		id: 'site-editor-template',
		label: 'Site Editor template route',
		path: '/wp-admin/site-editor.php?postType=wp_template&postId={themeSlug}%2F%2F{templateSlug}&canvas=edit',
		ready: SITE_EDITOR_READY,
		params: ['themeSlug', 'templateSlug'],
		setup: {
			recommended: 'Provide themeSlug and templateSlug, for example twentytwentyfive/home.',
		},
	},
	{
		id: 'site-editor-static-front-page',
		label: 'Site Editor static front page route',
		path: '/wp-admin/site-editor.php?postType=page&postId={frontPageId}&canvas=edit',
		ready: SITE_EDITOR_READY,
		params: ['frontPageId'],
		setup: {
			recommended: 'Create a static front page and pass its page ID as frontPageId.',
		},
	},
	{
		id: 'patterns',
		label: 'Patterns',
		path: '/wp-admin/site-editor.php?path=%2Fpatterns',
		ready: { selector: '.edit-site, .interface-interface-skeleton' },
		setup: {
			recommended: 'Run against a block theme for Site Editor pattern management.',
		},
	},
	{
		id: 'navigation',
		label: 'Navigation',
		path: '/wp-admin/site-editor.php?path=%2Fnavigation',
		ready: { selector: '.edit-site, .interface-interface-skeleton' },
		setup: {
			recommended: 'Run against a block theme with the Site Editor enabled.',
		},
	},
	{
		id: 'themes',
		label: 'Themes',
		path: '/wp-admin/themes.php',
		ready: { selector: '.theme-browser, .themes' },
	},
	{
		id: 'plugins',
		label: 'Plugins',
		path: '/wp-admin/plugins.php',
		ready: { selector: '.wp-list-table.plugins' },
	},
].map((scenario) => ({
	resources: DEFAULT_SCENARIO_RESOURCES,
	gate: {
		browser: WORDPRESS_ADMIN_BROWSER_GATE,
		rest: WORDPRESS_ADMIN_REST_GATE,
	},
	interactions: [],
	...scenario,
}));

const WORDPRESS_ADMIN_PAGE_SCENARIO_IDS = WORDPRESS_ADMIN_PAGE_SCENARIOS.map((scenario) => scenario.id);

function clone(value) {
	return JSON.parse(JSON.stringify(value));
}

function assertPlainObject(value, name) {
	if (value === null || typeof value !== 'object' || Array.isArray(value)) {
		throw new TypeError(`${name} must be an object`);
	}
}

function replaceParams(value, params, missing) {
	if (typeof value === 'string') {
		return value.replace(/\{([A-Za-z0-9_:-]+)\}/g, (match, key) => {
			if (params[key] === undefined || params[key] === null || params[key] === '') {
				missing.add(key);
				return match;
			}
			return encodeURIComponent(String(params[key]));
		});
	}
	if (Array.isArray(value)) {
		return value.map((entry) => replaceParams(entry, params, missing));
	}
	if (value && typeof value === 'object') {
		return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, replaceParams(entry, params, missing)]));
	}
	return value;
}

function resolveWordPressAdminPageScenario(scenario, options = {}) {
	assertPlainObject(scenario, 'scenario');
	assertPlainObject(options, 'options');
	const params = options.params || {};
	assertPlainObject(params, 'options.params');

	const missing = new Set();
	const resolved = replaceParams(clone(scenario), params, missing);
	resolved.missingParams = [...missing];
	if (resolved.missingParams.length > 0 && options.allowUnresolved === false) {
		throw new Error(`Scenario ${scenario.id || scenario.label || 'unknown'} is missing params: ${resolved.missingParams.join(', ')}`);
	}
	return resolved;
}

function getWordPressAdminPageScenario(id, options = {}) {
	if (typeof id !== 'string' || id.trim() === '') {
		throw new TypeError('scenario id must be a non-empty string');
	}
	const scenario = WORDPRESS_ADMIN_PAGE_SCENARIOS.find((entry) => entry.id === id);
	if (!scenario) {
		throw new Error(`Unknown WordPress admin page scenario: ${id}`);
	}
	return resolveWordPressAdminPageScenario(scenario, options);
}

function normalizeWordPressAdminPageScenarioInput(scenario, options = {}) {
	if (typeof scenario === 'string') {
		return getWordPressAdminPageScenario(scenario, options);
	}
	return resolveWordPressAdminPageScenario({
		resources: DEFAULT_SCENARIO_RESOURCES,
		gate: {
			browser: WORDPRESS_ADMIN_BROWSER_GATE,
			rest: WORDPRESS_ADMIN_REST_GATE,
		},
		interactions: [],
		...scenario,
	}, options);
}

function listWordPressAdminPageScenarios(options = {}) {
	assertPlainObject(options, 'options');
	const scenarios = options.scenarios === undefined
		? (options.ids === undefined ? WORDPRESS_ADMIN_PAGE_SCENARIO_IDS : options.ids)
		: options.scenarios;
	if (!Array.isArray(scenarios)) {
		throw new TypeError('options.scenarios or options.ids must be an array when provided');
	}
	const excludeIds = new Set(options.excludeIds || []);
	return scenarios
		.map((scenario) => normalizeWordPressAdminPageScenarioInput(scenario, options))
		.filter((scenario) => !excludeIds.has(scenario.id));
}

function createWordPressAdminPageScenarioManifest(options = {}) {
	const pages = listWordPressAdminPageScenarios(options);
	const overrides = options.overrides || {};
	assertPlainObject(overrides, 'options.overrides');

	return {
		pages: pages.map((page) => ({
			...page,
			...(overrides[page.id] || {}),
		})),
	};
}

async function profileWordPressAdminPageScenarios(input = {}) {
	assertPlainObject(input, 'input');
	const manifest = input.manifest || createWordPressAdminPageScenarioManifest(input);
	return profileWordPressPages({
		...input,
		manifest,
	});
}

module.exports = {
	WORDPRESS_ADMIN_BROWSER_GATE,
	WORDPRESS_ADMIN_PAGE_SCENARIO_IDS,
	WORDPRESS_ADMIN_PAGE_SCENARIOS,
	WORDPRESS_ADMIN_RESOURCE_EXCLUDE,
	WORDPRESS_ADMIN_RESOURCE_INCLUDE,
	WORDPRESS_ADMIN_REST_GATE,
	createWordPressAdminPageScenarioManifest,
	getWordPressAdminPageScenario,
	listWordPressAdminPageScenarios,
	normalizeWordPressAdminPageScenarioInput,
	profileWordPressAdminPageScenarios,
	resolveWordPressAdminPageScenario,
};
