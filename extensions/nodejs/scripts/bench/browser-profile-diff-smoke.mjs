import assert from 'node:assert/strict';

import {
    compareBrowserPerformanceProfiles,
    formatBrowserPerformanceDiffMarkdown,
} from './browser-helper.mjs';

const baseline = {
    summary: {
        resource_count: 2,
        network_request_count: 3,
        failed_network_request_count: 1,
        dom_content_loaded_ms: 500,
        load_event_ms: 900,
        largest_contentful_paint_ms: 800,
        cumulative_layout_shift: 0.01,
        long_task_count: 1,
        long_task_total_ms: 60,
        console_message_count: 2,
        page_error_count: 1,
        ready_ms: 700,
        network_idle_ms: 1200,
    },
    resources: [
        { name: 'https://example.test/app.js', initiator_type: 'script', transfer_size: 1000, duration: 70 },
        { name: 'https://example.test/style.css', initiator_type: 'link', transfer_size: 500, duration: 25 },
    ],
    network: [
        { method: 'GET', url: 'https://example.test/app.js', resource_type: 'script', status: 200, failed: false, start_time_ms: 100, duration_ms: 80 },
        { method: 'GET', url: 'https://example.test/api', resource_type: 'fetch', status: 500, failed: true, start_time_ms: 650, duration_ms: 220, failure_text: 'server error' },
        { method: 'GET', url: 'https://example.test/late-old', resource_type: 'fetch', status: 200, failed: false, start_time_ms: 760, duration_ms: 120 },
    ],
    paints: [
        { name: 'first-paint', start_time_ms: 200 },
        { name: 'first-contentful-paint', start_time_ms: 260 },
    ],
    long_tasks: [{ start_time_ms: 300, duration_ms: 60 }],
    console_messages: [
        { type: 'log', text: 'baseline log' },
        { type: 'error', text: 'baseline console error' },
    ],
    page_errors: [{ name: 'Error', message: 'baseline page error', stack: '' }],
    phase_marks: [{ name: 'ready', start_time_ms: 700 }, { name: 'network-idle', start_time_ms: 1200 }],
    phases: {
        boot: { start_time_ms: 0, end_time_ms: 500, duration_ms: 250 },
        hydrate: { start_time_ms: 500, end_time_ms: 900, duration_ms: 300 },
    },
};

const candidate = {
    summary: {
        resource_count: 2,
        network_request_count: 3,
        failed_network_request_count: 1,
        dom_content_loaded_ms: 450,
        load_event_ms: 1000,
        largest_contentful_paint_ms: 700,
        cumulative_layout_shift: 0.02,
        long_task_count: 2,
        long_task_total_ms: 130,
        console_message_count: 2,
        page_error_count: 0,
        ready_ms: 650,
        network_idle_ms: 1100,
    },
    resources: [
        { name: 'https://example.test/app.js', initiator_type: 'script', transfer_size: 1400, duration: 100 },
        { name: 'https://example.test/new.css', initiator_type: 'link', transfer_size: 700, duration: 30 },
    ],
    network: [
        { method: 'GET', url: 'https://example.test/app.js', resource_type: 'script', status: 200, failed: false, start_time_ms: 100, duration_ms: 120 },
        { method: 'GET', url: 'https://example.test/api', resource_type: 'fetch', status: 200, failed: false, start_time_ms: 640, duration_ms: 180 },
        { method: 'GET', url: 'https://example.test/late-new', resource_type: 'fetch', status: 404, failed: true, start_time_ms: 760, duration_ms: 160, failure_text: 'not found' },
    ],
    paints: [
        { name: 'first-paint', start_time_ms: 180 },
        { name: 'first-contentful-paint', start_time_ms: 240 },
    ],
    long_tasks: [{ start_time_ms: 300, duration_ms: 70 }, { start_time_ms: 500, duration_ms: 60 }],
    console_messages: [
        { type: 'warn', text: 'candidate warning' },
        { type: 'error', text: 'candidate console error' },
    ],
    page_errors: [],
    phase_marks: [{ name: 'ready', start_time_ms: 650 }, { name: 'network-idle', start_time_ms: 1100 }],
    phases: {
        boot: { start_time_ms: 0, end_time_ms: 500, duration_ms: 230 },
        hydrate: { start_time_ms: 500, end_time_ms: 900, duration_ms: 360 },
    },
};

const diff = compareBrowserPerformanceProfiles({ baseline, candidate }, { thresholdPercent: 5 });

assert.equal(diff.requests.added.length, 1);
assert.equal(diff.requests.removed.length, 1);
assert.equal(diff.requests.changed.length, 2);
assert.equal(diff.transfer.total.delta, 600);
assert.equal(diff.slowest_requests.slowest_duration_ms.delta, -40);
assert.equal(diff.failed_requests.count.delta, 0);
assert.equal(diff.late_requests.count.delta, 0);
assert.equal(diff.long_tasks.count.delta, 1);
assert.equal(diff.paints.first_contentful_paint_ms.delta, -20);
assert.equal(diff.paints.load_event_ms.delta, 100);
assert.equal(diff.paints.ready_ms.delta, -50);
assert.equal(diff.paints.network_idle_ms.delta, -100);
assert.equal(diff.errors.console_errors.delta, 0);
assert.equal(diff.errors.page_errors.delta, -1);
assert.equal(diff.phases.hydrate.delta_ms, 60);

const bootDiff = compareBrowserPerformanceProfiles({ baseline, candidate }, { phase: 'boot' });
assert.deepEqual(bootDiff.phase_scope, ['boot']);
assert.equal(bootDiff.phases.boot.delta_ms, -20);
assert.equal(bootDiff.metrics.network_request_count.delta, 0);
assert.equal(bootDiff.requests.added.length, 0);
assert.equal(bootDiff.requests.removed.length, 0);

const hydrateDiff = compareBrowserPerformanceProfiles({ baseline, candidate }, { phase: 'hydrate' });
assert.deepEqual(hydrateDiff.phase_scope, ['hydrate']);
assert.equal(hydrateDiff.metrics.network_request_count.delta, 0);
assert.equal(hydrateDiff.requests.added.length, 1);
assert.equal(hydrateDiff.requests.removed.length, 1);
assert.deepEqual(Object.keys(hydrateDiff.phases), ['hydrate']);
assert.equal(hydrateDiff.phases.hydrate.delta_ms, 60);

const markdown = formatBrowserPerformanceDiffMarkdown(diff, { title: 'Smoke browser profile diff' });
assert.match(markdown, /# Smoke browser profile diff/);
assert.match(markdown, /## Requests/);
assert.match(markdown, /https:\/\/example\.test\/late-new/);
assert.match(markdown, /## Phases/);

console.log('browser profile diff smoke passed');
