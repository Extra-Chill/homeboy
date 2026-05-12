#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

node --input-type=module - <<'EOF' "$SCRIPT_DIR/browser-helper.mjs"
import assert from 'node:assert/strict';

const {
  installBrowserPerformanceObservers,
  collectBrowserPerformanceProfile,
  compareBrowserPerformanceProfiles,
  formatBrowserPerformanceReport,
  runBrowserActions,
} = await import(process.argv[2]);

class FakePage {
  constructor() {
    this.handlers = new Map();
    this.performanceSnapshot = {
      url: 'https://example.test/app',
      navigation: [{
        name: 'https://example.test/app',
        entryType: 'navigation',
        startTime: 0,
        duration: 130,
        domContentLoadedEventEnd: 55,
        loadEventEnd: 120,
        transferSize: 1500,
      }],
      resources: [{
        name: 'https://example.test/app/app.js',
        entryType: 'resource',
        initiatorType: 'script',
        startTime: 10,
        duration: 20,
        transferSize: 500,
      }],
      paints: [{ name: 'first-contentful-paint', entryType: 'paint', startTime: 42, duration: 0 }],
      largest_contentful_paint: [{ name: '', start_time_ms: 84, size: 200, url: '', element: 'h1' }],
      layout_shifts: [{ name: '', start_time_ms: 88, value: 0.02, had_recent_input: false }],
      long_tasks: [{ name: 'self', start_time_ms: 90, duration_ms: 51 }],
      phase_marks: [{ name: 'load', start_time_ms: 0 }, { name: 'interaction', start_time_ms: 75 }],
    };
  }

  on(name, handler) {
    if (!this.handlers.has(name)) this.handlers.set(name, []);
    this.handlers.get(name).push(handler);
  }

  async addInitScript() {}

  async evaluate(fn, arg) {
    if (arg !== undefined) return null;
    if (String(fn).includes('serialize =')) return this.performanceSnapshot;
    return null;
  }

  async click(selector) {
    this.emit('action', ['click', selector]);
  }

  locator(selector) {
    this.emit('action', ['locator', selector]);
    return new FakeLocator(this, selector);
  }

  getByRole(role, options) {
    this.emit('action', ['getByRole', role, options?.name]);
    return new FakeLocator(this, `role:${role}`);
  }

  getByText(text, options) {
    this.emit('action', ['getByText', text, options?.exact]);
    return new FakeLocator(this, `text:${text}`);
  }

  async waitForSelector(selector) {
    this.emit('action', ['waitForSelector', selector]);
  }

  async waitForResponse(predicate) {
    this.emit('action', ['waitForResponse']);
    const response = {
      url: () => 'https://example.test/wp-json/datamachine/v1/jobs',
      status: () => 200,
      request: () => ({ method: () => 'GET' }),
    };
    if (!predicate(response)) throw new Error('response predicate did not match');
    return response;
  }

  emit(name, value) {
    for (const handler of this.handlers.get(name) || []) handler(value);
  }
}

class FakeLocator {
  constructor(page, selector) {
    this.page = page;
    this.selector = selector;
  }

  nth(index) {
    this.page.emit('action', ['locator.nth', this.selector, index]);
    return this;
  }

  async click() {
    this.page.emit('action', ['locator.click', this.selector]);
  }

  async fill(value) {
    this.page.emit('action', ['locator.fill', this.selector, value]);
  }

  async selectOption(value) {
    this.page.emit('action', ['locator.selectOption', this.selector, value]);
  }

  async waitFor(options) {
    this.page.emit('action', ['locator.waitFor', this.selector, options?.state]);
  }
}

const request = {
  url: () => 'https://example.test/app/app.js',
  method: () => 'GET',
  resourceType: () => 'script',
  headers: () => ({ authorization: 'Bearer secret', accept: 'text/javascript', 'x-api-key': 'super-secret' }),
  failure: () => ({ errorText: 'blocked' }),
};
const response = {
  request: () => request,
  status: () => 200,
  headers: () => ({ 'content-type': 'text/javascript', 'set-cookie': 'sid=secret' }),
};

const page = new FakePage();
const actionCalls = [];
page.on('action', (call) => actionCalls.push(call));
const actionEvidence = await runBrowserActions(page, [
  { click: '.button-primary' },
  { clickRole: { role: 'button', name: 'Admin' } },
  { fill: { selector: '.filter', value: 'queued' } },
  { select: { selector: 'select', index: 1, optionIndex: 2 } },
  { waitForSelector: '.modal' },
  { waitForResponse: { substring: '/wp-json/datamachine/v1/jobs', status: 200 } },
  { sleep: 1 },
]);
assert.equal(actionEvidence.actions.length, 7);
assert.equal(actionEvidence.actions[0].status, 'passed');
assert.equal(actionCalls.some((call) => call[0] === 'getByRole' && call[1] === 'button' && call[2] === 'Admin'), true);
assert.equal(actionCalls.some((call) => call[0] === 'locator.selectOption' && call[2]?.index === 2), true);

const controller = await installBrowserPerformanceObservers(page, { includeHeaders: true });
page.emit('request', request);
page.emit('response', response);
page.emit('requestfinished', request);
page.emit('console', { type: () => 'warning', text: () => 'slow path', location: () => ({ url: 'app.js', lineNumber: 1, columnNumber: 2 }) });
page.emit('pageerror', new Error('boom'));
await controller.markPhase('load');

const profile = await collectBrowserPerformanceProfile(page, { includeHeaders: true });
assert.equal(profile.schema_version, 1);
assert.equal(profile.summary.dom_content_loaded_ms, 55);
assert.equal(profile.summary.load_event_ms, 120);
assert.equal(profile.summary.resource_count, 1);
assert.equal(profile.summary.network_request_count, 1);
assert.equal(profile.summary.console_message_count, 1);
assert.equal(profile.summary.page_error_count, 1);
assert.equal(profile.network[0].request_headers.authorization, '[redacted]');
assert.equal(profile.network[0].request_headers['x-api-key'], '[redacted]');
assert.equal(profile.network[0].request_headers.accept, 'text/javascript');
assert.equal(profile.network[0].response_headers['set-cookie'], '[redacted]');
assert.equal(profile.largest_contentful_paint[0].start_time_ms, 84);
assert.equal(profile.layout_shifts[0].value, 0.02);
assert.equal(profile.long_tasks[0].duration_ms, 51);
assert.ok(profile.phases.load);

const candidate = JSON.parse(JSON.stringify(profile));
candidate.summary.load_event_ms = 150;
candidate.summary.long_task_total_ms = 25;
candidate.phases.load.duration_ms = 100;
const comparison = compareBrowserPerformanceProfiles({ baseline: profile, candidate }, { thresholdPercent: 5 });
assert.equal(comparison.metrics.load_event_ms.status, 'regressed');
assert.equal(comparison.metrics.long_task_total_ms.status, 'improved');
assert.equal(comparison.phases.load.status, 'regressed');

const report = formatBrowserPerformanceReport(comparison, { title: 'Smoke comparison' });
assert.match(report, /# Smoke comparison/);
assert.match(report, /load_event_ms/);
assert.match(report, /## Phases/);

console.log('Node.js browser performance profiler smoke passed.');
EOF
