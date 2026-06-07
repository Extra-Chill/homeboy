'use strict';

/**
 * External dependencies
 */
const { readFile } = require('node:fs/promises');

/**
 * Internal dependencies
 */
const {
	buildWordPressAdminPageSweepSummary,
	profileWordPressPages,
	summarizeWordPressAdminPageProfile,
} = require('./page-profiler');
const {
	WORDPRESS_RESOURCE_INCLUDE,
	assertPlainObject,
} = require('./shared');

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

const WORDPRESS_ADMIN_SCALE_SWEEP_DEFAULT_READY = { selector: '#wpbody-content, body.wp-admin', timeout: 120000 };

const DEFAULT_SCENARIO_RESOURCES = {
	includeResourceSubstrings: WORDPRESS_RESOURCE_INCLUDE,
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
		id: 'add-themes',
		label: 'Add Themes',
		path: '/wp-admin/theme-install.php',
		ready: { selector: 'body.theme-install-php, .theme-browser, .wp-filter' },
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
const WORDPRESS_ADMIN_PAGE_PROFILE_SCENARIO_IDS = [
	'dashboard',
	'add-themes',
	'site-editor-root',
];

const WORDPRESS_ADMIN_SCALE_SWEEP_DEFAULT_PAGES = [
	{ id: 'dashboard', path: '/wp-admin/index.php' },
	{ id: 'plugins', path: '/wp-admin/plugins.php' },
	{ id: 'themes', path: '/wp-admin/themes.php', ready: { selector: '.theme-browser, #wpbody-content', timeout: 120000 } },
	{ id: 'posts', path: '/wp-admin/edit.php' },
	{ id: 'add-post', path: '/wp-admin/post-new.php', ready: { selector: '.edit-post-layout, #editor, body.wp-admin', timeout: 120000 } },
];

function clone(value) {
	return JSON.parse(JSON.stringify(value));
}

function pageIdFromPath(pagePath) {
	return String(pagePath || '')
		.replace(/^https?:\/\/[^/]+/i, '')
		.replace(/^\/+/, '')
		.replace(/[^A-Za-z0-9_.:-]+/g, '-')
		.replace(/^-|-$/g, '') || 'wordpress-admin-page';
}

function metricId(value) {
	return String(value || 'page')
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '_')
		.replace(/^_+|_+$/g, '') || 'page';
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
	let scenarios = options.scenarios;
	if (scenarios === undefined) {
		scenarios = options.ids === undefined ? WORDPRESS_ADMIN_PAGE_SCENARIO_IDS : options.ids;
	}
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

function normalizeWordPressAdminScaleSweepManifest(manifest, options = {}) {
	assertPlainObject(options, 'options');
	assertPlainObject(manifest, 'WordPress admin scale sweep manifest');
	if (!Array.isArray(manifest.pages) || manifest.pages.length === 0) {
		throw new Error('WordPress admin scale sweep manifest requires a non-empty pages array');
	}
	const resourceInclude = options.resourceInclude || options.pageProfiler?.DEFAULT_RESOURCE_INCLUDE || WORDPRESS_RESOURCE_INCLUDE;

	return {
		...manifest,
		pages: manifest.pages.map((page, index) => {
			assertPlainObject(page, `WordPress admin scale sweep page ${index + 1}`);
			if (!page.path || typeof page.path !== 'string') {
				throw new Error(`WordPress admin scale sweep page ${index + 1} requires a path`);
			}

			const id = page.id || pageIdFromPath(page.path);
			const ready = page.ready || WORDPRESS_ADMIN_SCALE_SWEEP_DEFAULT_READY;
			return {
				...page,
				id,
				metricId: page.metricId || metricId(id),
				label: page.label || id,
				ready,
				resources: {
					includeResourceSubstrings: resourceInclude,
					...(page.resources || {}),
				},
				timeout: Number(page.timeout || ready.timeout || 120000),
				interactions: Array.isArray(page.interactions) ? page.interactions : [],
			};
		}),
	};
}

async function loadWordPressAdminScaleSweepManifest(options = {}) {
	assertPlainObject(options, 'options');
	const rawJson = options.json || process.env.HOMEBOY_WORDPRESS_ADMIN_SCALE_SWEEP_MANIFEST_JSON;
	const manifestPath = options.path || process.env.HOMEBOY_WORDPRESS_ADMIN_SCALE_SWEEP_MANIFEST;
	if (rawJson) {
		return normalizeWordPressAdminScaleSweepManifest(JSON.parse(rawJson), options);
	}
	if (manifestPath) {
		return normalizeWordPressAdminScaleSweepManifest(JSON.parse(await readFile(manifestPath, 'utf8')), options);
	}

	return normalizeWordPressAdminScaleSweepManifest({ pages: clone(WORDPRESS_ADMIN_SCALE_SWEEP_DEFAULT_PAGES) }, options);
}

function metricName(value) {
	return String(value || 'page').trim().toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '') || 'page';
}

function createWordPressAdminPageScenarioMetrics(profile) {
	const summary = summarizeWordPressAdminPageProfile(profile);
	const prefix = `wordpress_admin_${metricName(summary.id)}`;
	return {
		[`${prefix}_ready_ms`]: summary.readyMs,
		[`${prefix}_resource_count`]: summary.resourceCount,
		[`${prefix}_rest_count`]: summary.restCount,
		[`${prefix}_failed_request_count`]: summary.failedRequestCount,
		[`${prefix}_failure_count`]: summary.failureCount,
	};
}

async function navigateWordPressAdminAutoLogin(input) {
	const { autoLoginUrl, page, mark } = input;
	if (typeof autoLoginUrl !== 'string' || autoLoginUrl.trim() === '') {
		return undefined;
	}
	if (!page || typeof page.goto !== 'function') {
		throw new TypeError('page must provide goto()');
	}
	const response = await page.goto(autoLoginUrl, {
		waitUntil: input.autoLoginWaitUntil || 'commit',
		timeout: input.autoLoginTimeout || input.timeout || 120000,
	});
	if (typeof mark === 'function') {
		await mark('wordpress_admin_auto_login');
	}
	return {
		url: autoLoginUrl,
		status: response && typeof response.status === 'function' ? response.status() : 0,
	};
}

async function profileWordPressAdminPageScenario(input = {}) {
	assertPlainObject(input, 'input');
	const baseUrl = input.baseUrl || input.siteUrl;
	const scenarioInput = input.scenario || input.scenarioId || input.id;
	const scenario = normalizeWordPressAdminPageScenarioInput(scenarioInput, input);
	const autoLogin = await navigateWordPressAdminAutoLogin(input);
	const result = await profileWordPressPages({
		...input,
		baseUrl,
		manifest: {
			pages: [scenario],
		},
	});
	const profile = result.pages[0];
	const summary = summarizeWordPressAdminPageProfile(profile);

	return {
		...profile,
		autoLogin,
		metrics: createWordPressAdminPageScenarioMetrics(profile),
		metadata: {
			scenario,
			summary,
		},
	};
}

async function profileWordPressAdminPageScenarios(input = {}) {
	assertPlainObject(input, 'input');
	const baseUrl = input.baseUrl || input.siteUrl;
	const manifest = input.manifest || createWordPressAdminPageScenarioManifest(input);
	const autoLogin = await navigateWordPressAdminAutoLogin(input);
	const result = await profileWordPressPages({
		...input,
		baseUrl,
		manifest,
	});
	const summary = buildWordPressAdminPageSweepSummary(result);
	return {
		...result,
		autoLogin,
		metrics: Object.fromEntries(result.pages.flatMap((profile) => Object.entries(createWordPressAdminPageScenarioMetrics(profile)))),
		metadata: {
			summary,
		},
	};
}

module.exports = {
	WORDPRESS_ADMIN_BROWSER_GATE,
	WORDPRESS_ADMIN_PAGE_SCENARIO_IDS,
	WORDPRESS_ADMIN_PAGE_PROFILE_SCENARIO_IDS,
	WORDPRESS_ADMIN_PAGE_SCENARIOS,
	WORDPRESS_ADMIN_RESOURCE_EXCLUDE,
	WORDPRESS_ADMIN_SCALE_SWEEP_DEFAULT_PAGES,
	WORDPRESS_ADMIN_SCALE_SWEEP_DEFAULT_READY,
	WORDPRESS_RESOURCE_INCLUDE,
	WORDPRESS_ADMIN_REST_GATE,
	createWordPressAdminPageScenarioMetrics,
	createWordPressAdminPageScenarioManifest,
	getWordPressAdminPageScenario,
	listWordPressAdminPageScenarios,
	loadWordPressAdminScaleSweepManifest,
	normalizeWordPressAdminScaleSweepManifest,
	normalizeWordPressAdminPageScenarioInput,
	profileWordPressAdminPageScenario,
	profileWordPressAdminPageScenarios,
	resolveWordPressAdminPageScenario,
};
