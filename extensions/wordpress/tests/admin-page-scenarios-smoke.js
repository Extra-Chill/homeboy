'use strict';

/* eslint-disable no-console */

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	WORDPRESS_ADMIN_PAGE_SCENARIO_IDS,
	WORDPRESS_ADMIN_PAGE_PROFILE_SCENARIO_IDS,
	WORDPRESS_ADMIN_PAGE_SCENARIOS,
	WORDPRESS_RESOURCE_INCLUDE,
	createWordPressAdminPageScenarioMetrics,
	createWordPressAdminPageScenarioManifest,
	getWordPressAdminPageScenario,
	listWordPressAdminPageScenarios,
	normalizeWordPressAdminPageScenarioInput,
	normalizePageManifest,
	profileWordPressAdminPageScenario,
	profileWordPressAdminPageScenarios,
	resolveWordPressAdminPageScenario,
} = require('../index');

class FakeFrame {
	constructor(calls) {
		this.calls = calls;
	}

	async waitForSelector(selector) {
		this.calls.push(['frame.waitForSelector', selector]);
	}
}

class FakePage {
	constructor(resources = []) {
		this.resources = resources;
		this.calls = [];
		this.fakeFrame = new FakeFrame(this.calls);
	}

	async goto(url, options) {
		this.calls.push(['goto', url, options?.waitUntil]);
		return { status: () => 200 };
	}

	async waitForSelector(selector) {
		this.calls.push(['waitForSelector', selector]);
	}

	frame(query) {
		this.calls.push(['frame', query?.name || query]);
		return this.fakeFrame;
	}

	async evaluate() {
		return this.resources;
	}
}

assert.equal(Object.isFrozen(WORDPRESS_RESOURCE_INCLUDE), true);

const expectedIds = [
	'dashboard',
	'posts-list',
	'pages-list',
	'post-editor',
	'site-editor-root',
	'site-editor-template',
	'site-editor-static-front-page',
	'patterns',
	'navigation',
	'themes',
	'add-themes',
	'plugins',
];

assert.deepEqual(WORDPRESS_ADMIN_PAGE_SCENARIO_IDS, expectedIds);
assert.deepEqual(WORDPRESS_ADMIN_PAGE_PROFILE_SCENARIO_IDS, ['dashboard', 'add-themes', 'site-editor-root']);
assert.equal(WORDPRESS_ADMIN_PAGE_SCENARIOS.length, expectedIds.length);

for (const scenario of WORDPRESS_ADMIN_PAGE_SCENARIOS) {
	assert.equal(typeof scenario.id, 'string');
	assert.equal(typeof scenario.label, 'string');
	assert.equal(typeof (scenario.path || scenario.url), 'string');
	assert.equal(typeof scenario.ready, 'object');
	assert.equal(Array.isArray(scenario.resources.includeResourceSubstrings), true);
	assert.equal(Array.isArray(scenario.resources.excludeResourceSubstrings), true);
	assert.equal(typeof scenario.gate.browser.readyMs.warn, 'number');
	assert.equal(typeof scenario.gate.rest.restAfterReadyCount.warn, 'number');
}

const dashboard = getWordPressAdminPageScenario('dashboard');
assert.equal(dashboard.path, '/wp-admin/index.php');
dashboard.path = '/changed';
assert.equal(getWordPressAdminPageScenario('dashboard').path, '/wp-admin/index.php');

const subset = listWordPressAdminPageScenarios({
	ids: ['dashboard', 'plugins', 'add-themes'],
	excludeIds: ['plugins'],
});
assert.deepEqual(subset.map((scenario) => scenario.id), ['dashboard', 'add-themes']);

const customScenario = normalizeWordPressAdminPageScenarioInput({
	id: 'woocommerce-orders',
	label: 'WooCommerce orders',
	path: '/wp-admin/admin.php?page=wc-orders',
	ready: { selector: '.woocommerce-layout, .wp-list-table' },
});
assert.equal(customScenario.path, '/wp-admin/admin.php?page=wc-orders');
assert.equal(customScenario.resources.includeResourceSubstrings.includes('/wp-json/'), true);

const mixed = listWordPressAdminPageScenarios({
	scenarios: [
		'dashboard',
		{
			id: 'custom-settings',
			label: 'Custom settings',
			path: '/wp-admin/options-general.php?page=custom',
			ready: { selector: '#wpbody-content' },
		},
	],
});
assert.deepEqual(mixed.map((scenario) => scenario.id), ['dashboard', 'custom-settings']);

const template = getWordPressAdminPageScenario('site-editor-template', {
	params: {
		themeSlug: 'twentytwentyfive',
		templateSlug: 'home',
	},
});
assert.equal(template.path, '/wp-admin/site-editor.php?postType=wp_template&postId=twentytwentyfive%2F%2Fhome&canvas=edit');
assert.deepEqual(template.missingParams, []);

const staticFrontPage = getWordPressAdminPageScenario('site-editor-static-front-page');
assert.deepEqual(staticFrontPage.missingParams, ['frontPageId']);
assert.throws(
	() => resolveWordPressAdminPageScenario(WORDPRESS_ADMIN_PAGE_SCENARIOS.find((scenario) => scenario.id === 'site-editor-static-front-page'), { allowUnresolved: false }),
	/missing params: frontPageId/
);

const manifest = createWordPressAdminPageScenarioManifest({
	scenarios: ['dashboard', 'add-themes', 'site-editor-template', customScenario],
	params: {
		themeSlug: 'twentytwentyfive',
		templateSlug: 'home',
	},
	overrides: {
		'woocommerce-orders': {
			restObservationMs: 0,
		},
	},
});
assert.deepEqual(manifest.pages.map((scenario) => scenario.id), ['dashboard', 'add-themes', 'site-editor-template', 'woocommerce-orders']);
assert.equal(manifest.pages[3].restObservationMs, 0);
assert.equal(normalizePageManifest(manifest).length, 4);

assert.throws(() => getWordPressAdminPageScenario('missing'), /Unknown WordPress admin page scenario/);

async function main() {
	const marks = [];
	const page = new FakePage([
		{ name: 'https://example.test/wp-admin/load-styles.php', startTime: 1, duration: 10 },
		{ name: 'https://example.test/wp-json/wp/v2/types?context=edit', startTime: 2, duration: 20, transferSize: 100 },
	]);
	const dashboardProfile = await profileWordPressAdminPageScenario({
		page,
		siteUrl: 'https://example.test',
		autoLoginUrl: 'https://example.test/wp-login.php?autologin=1',
		scenario: 'dashboard',
		restObservationMs: 0,
		mark: async (name) => marks.push(name),
	});

	assert.equal(dashboardProfile.id, 'dashboard');
	assert.equal(dashboardProfile.path, '/wp-admin/index.php');
	assert.equal(dashboardProfile.autoLogin.status, 200);
	assert.equal(dashboardProfile.metadata.summary.id, 'dashboard');
	assert.equal(typeof dashboardProfile.metrics.wordpress_admin_dashboard_ready_ms, 'number');
	assert.equal(page.calls[0][1], 'https://example.test/wp-login.php?autologin=1');
	assert.equal(page.calls.some((call) => call[0] === 'waitForSelector' && call[1] === '#dashboard-widgets'), true);
	assert.deepEqual(marks, ['wordpress_admin_auto_login', 'dashboard_commit', 'dashboard_ready']);

	const metrics = createWordPressAdminPageScenarioMetrics(dashboardProfile);
	assert.equal(metrics.wordpress_admin_dashboard_rest_count, 1);

	const sweepPage = new FakePage();
	const sweep = await profileWordPressAdminPageScenarios({
		page: sweepPage,
		baseUrl: 'https://example.test',
		scenarios: ['dashboard', 'add-themes', 'site-editor-root'],
		restObservationMs: 0,
	});
	assert.deepEqual(sweep.pages.map((profile) => profile.id), ['dashboard', 'add-themes', 'site-editor-root']);
	assert.equal(sweep.metadata.summary.totals.pageCount, 3);
	assert.equal(typeof sweep.metrics.wordpress_admin_add_themes_ready_ms, 'number');
	assert.equal(sweepPage.calls.some((call) => call[0] === 'goto' && call[1] === 'https://example.test/wp-admin/theme-install.php'), true);
	assert.equal(sweepPage.calls.some((call) => call[0] === 'frame' && call[1] === 'editor-canvas'), true);
	assert.equal(sweepPage.calls.some((call) => call[0] === 'frame.waitForSelector' && call[1] === 'body'), true);

	console.log('WordPress admin page scenarios smoke passed.');
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
