'use strict';

/**
 * External dependencies
 */
const assert = require('node:assert/strict');

/**
 * Internal dependencies
 */
const {
	buildWordPressDiscoveryInventoryArtifact,
	formatWordPressDiscoveryInventoryMarkdownReport,
	normalizeWordPressDiscoveryBlockType,
	normalizeWordPressDiscoveryOptionSetting,
	normalizeWordPressDiscoveryShortcode,
	normalizeWordPressDiscoverySurface,
} = require('../lib/wordpress-discovery-inventory');
const { buildWordPressFuzzPlanFromSurfaces } = require('../lib/wordpress-fuzz-plan-from-surfaces');

const artifact = buildWordPressDiscoveryInventoryArtifact({
	blocks: [{
		name: 'core/paragraph',
		title: 'Paragraph',
		category: 'text',
		keywords: ['copy', 'text', 'copy'],
		attributes: { content: { type: 'string' }, dropCap: { type: 'boolean' } },
		supports: { anchor: true },
		uses_context: ['postId'],
		api_version: '3',
	}, {
		name: 'plugin/card',
		title: 'Card',
		category: 'widgets',
	}],
	shortcodes: ['gallery', { tag: 'contact-form', callback: 'render_contact_form', source: 'plugin' }],
	settings: [{ name: 'blogname', surface: 'option', autoload: 'yes' }, {
		name: 'posts_per_page',
		surface: 'setting',
		group: 'reading',
		show_in_rest: true,
		type: 'integer',
	}],
	rest: { routes: { '/wp/v2/posts': { route: '/wp/v2/posts', method: 'GET' } } },
	admin: [{ id: 'admin:dashboard', path: '/wp-admin/index.php' }],
	frontend_urls: [{ id: 'front:home', path: '/' }],
	hooks: { actions: { init: { hook: 'init' } } },
	database: {
		tables: { posts: { table: 'wp_posts' } },
		queries: { postsLookup: { query: 'SELECT ID FROM wp_posts WHERE post_type = ?' } },
	},
	external_http: { requests: [{ id: 'http:example', url: 'https://api.example.test/v1/', method: 'GET' }] },
});

assert.equal(artifact.schema, 'homeboy/wordpress-discovery-inventory/v1');
assert.deepEqual(artifact.totals, {
	blockCount: 2,
	shortcodeCount: 2,
	optionSettingCount: 2,
	surfaceCount: 11,
});
assert.deepEqual(artifact.surfaceCounts, {
	'admin-page': 1,
	block: 2,
	'database-table': 1,
	'db-query': 1,
	'external-http': 1,
	'frontend-url': 1,
	hook: 1,
	option: 2,
	'rest-route': 1,
});
assert.deepEqual(artifact.blocks.map((block) => block.name), ['core/paragraph', 'plugin/card']);
assert.deepEqual(artifact.blocks[0].attributes, ['content', 'dropCap']);
assert.deepEqual(artifact.blocks[0].keywords, ['copy', 'text']);
assert.equal(artifact.blocks[0].apiVersion, 3);
assert.deepEqual(artifact.shortcodes.map((shortcode) => shortcode.tag), ['contact-form', 'gallery']);
assert.equal(artifact.optionSettings[1].name, 'posts_per_page');
assert.equal(artifact.optionSettings[1].restVisible, true);

assert.deepEqual(normalizeWordPressDiscoveryBlockType({ name: 'core/image' }).attributes, []);
assert.equal(normalizeWordPressDiscoveryShortcode('caption').tag, 'caption');
assert.equal(normalizeWordPressDiscoveryOptionSetting('timezone_string').name, 'timezone_string');
assert.equal(normalizeWordPressDiscoveryOptionSetting('timezone_string').surface, 'option');
assert.equal(normalizeWordPressDiscoveryOptionSetting({ name: 'page_for_posts', type: 'integer' }).surface, 'option');
assert.equal(normalizeWordPressDiscoveryOptionSetting({ name: 'page_for_posts', type: 'integer' }).valueType, 'integer');
assert.equal(normalizeWordPressDiscoverySurface({ kind: 'rest', route: '/wp/v2/pages' }).type, 'rest-route');
assert.equal(normalizeWordPressDiscoverySurface({ kind: 'external_http', url: 'https://example.test/' }).type, 'external-http');
assert.throws(() => normalizeWordPressDiscoveryBlockType({ title: 'Missing name' }), /name/);
assert.throws(() => normalizeWordPressDiscoveryShortcode(' '), /tag/);
assert.throws(() => normalizeWordPressDiscoveryShortcode({ callback: 'missing_tag' }), /tag/);
assert.throws(() => normalizeWordPressDiscoveryOptionSetting(' '), /name/);
assert.throws(() => normalizeWordPressDiscoveryOptionSetting({ group: 'reading' }), /name/);

const markdown = formatWordPressDiscoveryInventoryMarkdownReport(artifact);
assert.match(markdown, /Blocks: 2; shortcodes: 2; options\/settings: 2; surfaces: 11/);
assert.match(markdown, /\| rest-route \| \/wp\/v2\/posts \| \/wp\/v2\/posts \|/);
assert.match(markdown, /\| core\/paragraph \| Paragraph \| text \|/);
assert.match(markdown, /\| contact-form \| render_contact_form \| plugin \|/);
assert.match(markdown, /\| posts_per_page \| setting \| reading \|/);

const plan = buildWordPressFuzzPlanFromSurfaces(artifact);
assert.equal(plan.targets.length, 11);
assert.equal(plan.targets.find((target) => target.type === 'rest-route').cases[0].operation.route, '/wp/v2/posts');
assert.equal(plan.targets.find((target) => target.type === 'admin-page').cases[0].intent, 'request-admin-page');
assert.equal(plan.targets.find((target) => target.type === 'frontend-url').cases[0].operation.path, '/');
assert.equal(plan.targets.find((target) => target.type === 'hook').cases[0].operation.hook, 'init');
assert.equal(plan.targets.find((target) => target.type === 'db-query').cases[0].operation.query, 'SELECT ID FROM wp_posts WHERE post_type = ?');
assert.equal(plan.targets.find((target) => target.type === 'external-http').cases[0].operation.url, 'https://api.example.test/v1/');
console.log('WordPress discovery inventory smoke passed.');
