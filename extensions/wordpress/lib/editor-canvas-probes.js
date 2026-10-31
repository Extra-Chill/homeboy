'use strict';

const DEFAULT_EDITOR_CANVAS_IFRAME_SELECTOR = 'iframe[name="editor-canvas"]';
const DEFAULT_EDITOR_CANVAS_LAYOUT_SELECTOR = '.block-editor-block-list__layout';
const DEFAULT_EDITOR_CANVAS_BLOCK_SELECTOR = '.block-editor-block-list__block, [data-block]';
const DEFAULT_EDITOR_CANVAS_TIMEOUT_MS = 30000;

async function waitForWordPressEditorCanvas(page, options = {}) {
 if (!page || typeof page !== 'object') {
  throw new TypeError('waitForWordPressEditorCanvas requires a Playwright page');
 }
 const timeout = Number(options.timeoutMs || options.timeout || DEFAULT_EDITOR_CANVAS_TIMEOUT_MS);
 const iframeSelector = options.iframeSelector || DEFAULT_EDITOR_CANVAS_IFRAME_SELECTOR;
 const layoutSelector = options.layoutSelector || DEFAULT_EDITOR_CANVAS_LAYOUT_SELECTOR;
 const blockSelector = options.blockSelector || DEFAULT_EDITOR_CANVAS_BLOCK_SELECTOR;
 const startedAt = Date.now();

 if (options.url && typeof page.goto === 'function') {
  await page.goto(options.url, { waitUntil: options.waitUntil || 'domcontentloaded', timeout });
 }
 if (typeof page.waitForSelector === 'function') {
  await page.waitForSelector(iframeSelector, { timeout });
 }

 const frame = await resolveEditorCanvasFrame(page, iframeSelector, options);
 if (!frame) {
  throw new Error('WordPress editor canvas iframe did not expose a content frame');
 }

 if (typeof frame.waitForFunction !== 'function') {
  throw new TypeError('waitForWordPressEditorCanvas requires frame.waitForFunction()');
 }
	await frame.waitForFunction(
		({ layoutSelector: innerLayoutSelector, blockSelector: innerBlockSelector }) => {
			const layout = document.querySelector(innerLayoutSelector);
			if (!layout) {
				return false;
			}
			const rect = layout.getBoundingClientRect();
			const loading = layout.matches('.is-loading, [aria-busy="true"]') || layout.querySelector('.is-loading, [aria-busy="true"], .components-spinner');
			const blockCount = layout.querySelectorAll(innerBlockSelector).length;
   return rect.width > 0 && rect.height > 0 && !loading && blockCount > 0;
  },
  { layoutSelector, blockSelector },
  { timeout }
 );

 if (options.stabilize !== false) {
  await stabilizeEditorCanvas(frame, options);
 }

 return {
  frame,
  iframeSelector,
  layoutSelector,
  blockSelector,
  readyMs: Date.now() - startedAt,
 };
}

async function captureWordPressEditorCanvasScreenshot(page, screenshotPath, options = {}) {
 const ready = await waitForWordPressEditorCanvas(page, options);
 if (!screenshotPath || typeof screenshotPath !== 'string') {
  throw new TypeError('captureWordPressEditorCanvasScreenshot requires screenshotPath');
 }
 const layout = ready.frame.locator(ready.layoutSelector).first();
 await layout.screenshot({ path: screenshotPath, timeout: Number(options.timeoutMs || options.timeout || DEFAULT_EDITOR_CANVAS_TIMEOUT_MS) });
 return {
  path: screenshotPath,
  readyMs: ready.readyMs,
  layoutSelector: ready.layoutSelector,
 };
}

async function summarizeVisibleSelectors(page, groups) {
 if (!page || typeof page.$$eval !== 'function') {
  throw new TypeError('summarizeVisibleSelectors requires a Playwright page with $$eval()');
 }
 const normalizedGroups = normalizeSelectorGroups(groups);
 const evaluatedGroups = [];

 for (const group of normalizedGroups) {
  const selectors = [];
  for (const selector of group.selectors) {
   try {
    const matches = await page.$$eval(selector, (elements) => elements.map((element) => {
     const rect = element.getBoundingClientRect();
     const style = window.getComputedStyle(element);
     const visible = rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity || '1') > 0;
     return {
      visible,
      boundingBox: {
       x: Math.round(rect.x),
       y: Math.round(rect.y),
       width: Math.round(rect.width),
       height: Math.round(rect.height),
      },
      text: String(element.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 160),
     };
    }));
    selectors.push(normalizeSelectorSummary(selector, matches));
   } catch (error) {
    selectors.push({
     selector,
     count: 0,
     visible_count: 0,
     nonzero_bounding_box_count: 0,
     first_match: null,
     error: error?.message || String(error),
    });
   }
  }
  evaluatedGroups.push(normalizeSelectorGroupSummary(group.name, selectors));
 }

 return {
  groups: evaluatedGroups,
  totals: visibleSelectorTotals(evaluatedGroups),
 };
}

async function resolveEditorCanvasFrame(page, iframeSelector, options = {}) {
	if (options.frame) {
		return options.frame;
	}
	if (typeof page.frame === 'function') {
		const frame = page.frame({ name: options.frameName || 'editor-canvas' });
		if (frame) {
			return frame;
		}
	}
 if (typeof page.locator === 'function') {
  const handle = await page.locator(iframeSelector).elementHandle();
  if (handle && typeof handle.contentFrame === 'function') {
   return handle.contentFrame();
  }
 }
 return null;
}

async function stabilizeEditorCanvas(frame, options = {}) {
 if (typeof frame.addStyleTag === 'function') {
  await frame.addStyleTag({ content: editorCanvasStabilizingCss() });
 }
 const waitMs = Number(options.stabilizeMs ?? 300);
 if (waitMs > 0 && typeof frame.waitForTimeout === 'function') {
  await frame.waitForTimeout(waitMs);
 }
}

function editorCanvasStabilizingCss() {
 return `
*, *::before, *::after {
  animation-delay: 0s !important;
  animation-duration: 0.001ms !important;
  caret-color: transparent !important;
  transition-delay: 0s !important;
  transition-duration: 0.001ms !important;
}
.block-editor-block-list__block,
.block-editor-block-list__block::before,
.block-editor-block-list__block::after,
.is-selected,
.is-highlighted,
.has-child-selected {
  box-shadow: none !important;
  outline: 0 !important;
}
.block-editor-block-contextual-toolbar,
.block-editor-block-list__insertion-point,
.block-editor-block-list__breadcrumb,
.block-editor-inserter,
.block-editor-inserter__quick-inserter,
.block-editor-rich-text__editable::after,
.components-popover,
.components-toolbar,
.components-toolbar-group,
.is-root-container > .block-list-appender {
  display: none !important;
}`;
}

function normalizeSelectorGroups(groups) {
 if (!Array.isArray(groups)) {
  throw new TypeError('visible selector groups must be an array');
 }
 return groups.map((group, index) => {
  if (!group || typeof group !== 'object') {
   throw new TypeError(`visible selector group ${index + 1} must be an object`);
  }
  const selectors = Array.isArray(group.selectors) ? group.selectors : [group.selector].filter(Boolean);
  return {
   name: String(group.name || `group_${index + 1}`),
   selectors: selectors.map((selector) => String(selector || '').trim()).filter(Boolean),
  };
 }).filter((group) => group.selectors.length > 0);
}

function normalizeSelectorSummary(selector, matches) {
 return {
  selector,
  count: matches.length,
  visible_count: matches.filter((match) => match.visible).length,
  nonzero_bounding_box_count: matches.filter((match) => match.boundingBox.width > 0 && match.boundingBox.height > 0).length,
  first_match: matches[0] || null,
  error: '',
 };
}

function normalizeSelectorGroupSummary(name, selectors) {
 return {
  name,
  selectors,
  selector_count: selectors.length,
  missing_selector_count: selectors.filter((item) => item.count === 0).length,
  errored_selector_count: selectors.filter((item) => item.error).length,
  matched_selector_count: selectors.filter((item) => item.count > 0).length,
  visible_selector_count: selectors.filter((item) => item.visible_count > 0).length,
  nonzero_bounding_box_selector_count: selectors.filter((item) => item.nonzero_bounding_box_count > 0).length,
 };
}

function visibleSelectorTotals(groups) {
 return groups.reduce((totals, group) => {
  totals.selector_count += group.selector_count;
  totals.missing_selector_count += group.missing_selector_count;
  totals.errored_selector_count += group.errored_selector_count;
  totals.matched_selector_count += group.matched_selector_count;
  totals.visible_selector_count += group.visible_selector_count;
  totals.nonzero_bounding_box_selector_count += group.nonzero_bounding_box_selector_count;
  return totals;
 }, {
  selector_count: 0,
  missing_selector_count: 0,
  errored_selector_count: 0,
  matched_selector_count: 0,
  visible_selector_count: 0,
  nonzero_bounding_box_selector_count: 0,
 });
}

module.exports = {
 DEFAULT_EDITOR_CANVAS_BLOCK_SELECTOR,
 DEFAULT_EDITOR_CANVAS_IFRAME_SELECTOR,
 DEFAULT_EDITOR_CANVAS_LAYOUT_SELECTOR,
 captureWordPressEditorCanvasScreenshot,
 editorCanvasStabilizingCss,
 summarizeVisibleSelectors,
 waitForWordPressEditorCanvas,
};
