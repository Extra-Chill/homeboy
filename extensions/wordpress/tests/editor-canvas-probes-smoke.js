'use strict';

/* eslint-disable no-console */

const assert = require('node:assert/strict');

const {
	captureWordPressEditorCanvasScreenshot,
	summarizeVisibleSelectors,
	waitForWordPressEditorCanvas,
} = require('../lib/editor-canvas-probes');

class FakeLayoutLocator {
	constructor(calls, selector) {
		this.calls = calls;
		this.selector = selector;
	}

	first() {
		this.calls.push(['layout.first', this.selector]);
		return this;
	}

	async screenshot(options) {
		this.calls.push(['layout.screenshot', options.path, options.timeout]);
	}
}

class FakeFrame {
	constructor(calls) {
		this.calls = calls;
	}

	async waitForFunction(callback, arg, options) {
		this.calls.push(['frame.waitForFunction', arg.layoutSelector, arg.blockSelector, options.timeout]);
	}

	async addStyleTag(options) {
		this.calls.push(['frame.addStyleTag', options.content.includes('.components-popover')]);
	}

	async waitForTimeout(ms) {
		this.calls.push(['frame.waitForTimeout', ms]);
	}

	locator(selector) {
		this.calls.push(['frame.locator', selector]);
		return new FakeLayoutLocator(this.calls, selector);
	}
}

class FakePage {
	constructor() {
		this.calls = [];
		this.frameObject = new FakeFrame(this.calls);
	}

	async goto(url, options) {
		this.calls.push(['goto', url, options.waitUntil, options.timeout]);
	}

	async waitForSelector(selector, options) {
		this.calls.push(['waitForSelector', selector, options.timeout]);
	}

	frame(options) {
		this.calls.push(['page.frame', options.name]);
		return this.frameObject;
	}
}

class FakeElement {
	constructor({ rect, display = 'block', visibility = 'visible', opacity = '1', text = '' }) {
		this.rect = rect;
		this.style = { display, visibility, opacity };
		this.textContent = text;
	}

	getBoundingClientRect() {
		return this.rect;
	}
}

class FakeSelectorPage {
	constructor() {
		this.calls = [];
		this.elementsBySelector = {
			'.hero': [new FakeElement({ rect: { x: 1, y: 2, width: 300, height: 120 }, text: 'Hero text' })],
			'.hidden': [new FakeElement({ rect: { x: 0, y: 0, width: 10, height: 10 }, display: 'none', text: 'Hidden' })],
			'.missing': [],
		};
	}

	async $$eval(selector, callback) {
		this.calls.push(['$$eval', selector]);
		const previousWindow = global.window;
		global.window = { getComputedStyle: (element) => element.style };
		try {
			return callback(this.elementsBySelector[selector] || []);
		} finally {
			global.window = previousWindow;
		}
	}
}

async function main() {
	const page = new FakePage();
	const ready = await waitForWordPressEditorCanvas(page, { url: 'https://example.test/wp-admin/site-editor.php', timeoutMs: 1000, stabilizeMs: 5 });
	assert.equal(ready.iframeSelector, 'iframe[name="editor-canvas"]');
	assert.equal(ready.layoutSelector, '.block-editor-block-list__layout');
	assert.equal(typeof ready.readyMs, 'number');
	assert.deepEqual(page.calls[0], ['goto', 'https://example.test/wp-admin/site-editor.php', 'domcontentloaded', 1000]);
	assert.equal(page.calls.some((call) => call[0] === 'frame.addStyleTag' && call[1] === true), true);

	const screenshot = await captureWordPressEditorCanvasScreenshot(page, '/tmp/editor-canvas.png', { stabilize: false });
	assert.equal(screenshot.path, '/tmp/editor-canvas.png');
	assert.equal(page.calls.some((call) => call[0] === 'layout.screenshot' && call[1] === '/tmp/editor-canvas.png'), true);

	const selectorSummary = await summarizeVisibleSelectors(new FakeSelectorPage(), [
		{ name: 'hero', selectors: ['.hero', '.missing'] },
		{ name: 'chrome', selector: '.hidden' },
	]);
	assert.equal(selectorSummary.totals.selector_count, 3);
	assert.equal(selectorSummary.totals.visible_selector_count, 1);
	assert.equal(selectorSummary.groups[0].selectors[0].first_match.text, 'Hero text');
	assert.equal(selectorSummary.groups[1].selectors[0].visible_count, 0);

	console.log('WordPress editor canvas probes smoke passed.');
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
