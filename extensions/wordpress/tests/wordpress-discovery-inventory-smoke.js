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
} = require('../lib/wordpress-discovery-inventory');

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
});

assert.equal(artifact.schema, 'homeboy/wordpress-discovery-inventory/v1');
assert.deepEqual(artifact.totals, {
	blockCount: 2,
	shortcodeCount: 2,
	optionSettingCount: 2,
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
assert.throws(() => normalizeWordPressDiscoveryBlockType({ title: 'Missing name' }), /name/);
assert.throws(() => normalizeWordPressDiscoveryShortcode(' '), /tag/);
assert.throws(() => normalizeWordPressDiscoveryShortcode({ callback: 'missing_tag' }), /tag/);
assert.throws(() => normalizeWordPressDiscoveryOptionSetting(' '), /name/);
assert.throws(() => normalizeWordPressDiscoveryOptionSetting({ group: 'reading' }), /name/);

const markdown = formatWordPressDiscoveryInventoryMarkdownReport(artifact);
assert.match(markdown, /Blocks: 2; shortcodes: 2; options\/settings: 2/);
assert.match(markdown, /\| core\/paragraph \| Paragraph \| text \|/);
assert.match(markdown, /\| contact-form \| render_contact_form \| plugin \|/);
assert.match(markdown, /\| posts_per_page \| setting \| reading \|/);

console.log('WordPress discovery inventory smoke passed.');
