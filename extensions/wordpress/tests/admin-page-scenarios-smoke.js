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
	WORDPRESS_ADMIN_PAGE_SCENARIOS,
	createWordPressAdminPageScenarioManifest,
	getWordPressAdminPageScenario,
	listWordPressAdminPageScenarios,
	normalizePageManifest,
	resolveWordPressAdminPageScenario,
} = require('../index');

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
	'plugins',
];

assert.deepEqual(WORDPRESS_ADMIN_PAGE_SCENARIO_IDS, expectedIds);
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
	ids: ['dashboard', 'plugins', 'themes'],
	excludeIds: ['plugins'],
});
assert.deepEqual(subset.map((scenario) => scenario.id), ['dashboard', 'themes']);

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
	ids: ['dashboard', 'site-editor-template', 'plugins'],
	params: {
		themeSlug: 'twentytwentyfive',
		templateSlug: 'home',
	},
	overrides: {
		plugins: {
			restObservationMs: 0,
		},
	},
});
assert.deepEqual(manifest.pages.map((scenario) => scenario.id), ['dashboard', 'site-editor-template', 'plugins']);
assert.equal(manifest.pages[2].restObservationMs, 0);
assert.equal(normalizePageManifest(manifest).length, 3);

assert.throws(() => getWordPressAdminPageScenario('missing'), /Unknown WordPress admin page scenario/);

console.log('WordPress admin page scenarios smoke passed.');
