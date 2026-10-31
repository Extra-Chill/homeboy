import assert from 'node:assert/strict';
import test from 'node:test';
import vm from 'node:vm';

import {
    DEFERRED_INIT_BROWSER_PHASES,
    deferredInitBrowserMarkerScript,
    deferredInitBrowserMarkers,
    summarizeDeferredInitBrowserEvidence,
} from '../nodejs/scripts/bench/browser-helper.mjs';

test('deferredInitBrowserMarkerScript installs a product-neutral browser channel', () => {
    const script = deferredInitBrowserMarkerScript('lazy-widget');
    const marks = [];
    const context = {
        performance: {
            now: () => 10,
            mark: (name) => marks.push(name),
        },
        window: {},
    };

    vm.runInNewContext(script, context);

    const channel = context.window.__homeboyDeferredInit['lazy-widget'];
    assert.equal(channel.featureId, 'lazy-widget');
    assert.deepEqual(JSON.parse(JSON.stringify(channel.markers)), deferredInitBrowserMarkers('lazy-widget'));
    assert.deepEqual(JSON.parse(JSON.stringify(channel.events)), [
        {
            data: {},
            name: 'deferred_init.lazy-widget.feature_not_needed.start',
            t_ms: 0,
        },
    ]);
    assert.deepEqual(marks, ['deferred_init.lazy-widget.feature_not_needed.start']);
});

test('summarizeDeferredInitBrowserEvidence emits phase metrics and passing assertions', () => {
    const markers = deferredInitBrowserMarkers('lazy-widget');
    const summary = summarizeDeferredInitBrowserEvidence({
        featureId: 'lazy-widget',
        markerEvents: [
            { name: markers.featureNotNeededReady, t_ms: 125 },
            { name: markers.featureNeededTrigger, t_ms: 500 },
            { name: markers.featureNeededReady, t_ms: 725 },
            { name: markers.featureNeededSuccess, t_ms: 750 },
        ],
        networkEntries: [
            { url: 'https://example.test/page', start_time_ms: 10 },
            { url: 'https://cdn.example.test/lazy-widget.js', start_time_ms: 525 },
            { url: 'https://api.example.test/lazy-widget/config', start_time_ms: 620 },
            { url: 'https://third-party.example.test/tag.js', start_time_ms: 650 },
        ],
        featureRequestMatchers: ['lazy-widget'],
        thirdPartyRequestMatchers: [/third-party\.example/],
        maxEarlyFeatureRequests: 0,
        maxEarlyThirdPartyRequests: 0,
        minPostTriggerFeatureRequests: 2,
    });

    assert.equal(summary.feature_id, 'lazy-widget');
    assert.deepEqual(summary.phases, DEFERRED_INIT_BROWSER_PHASES);
    assert.equal(summary.metrics.lazy_widget_deferred_init_feature_not_needed_ready_ms, 125);
    assert.equal(summary.metrics.lazy_widget_deferred_init_feature_needed_trigger_ms, 500);
    assert.equal(summary.metrics.lazy_widget_deferred_init_feature_request_count_before_trigger, 0);
    assert.equal(summary.metrics.lazy_widget_deferred_init_feature_request_count_after_trigger, 2);
    assert.equal(summary.metrics.lazy_widget_deferred_init_third_party_request_count_before_trigger, 0);
    assert.equal(summary.metrics.lazy_widget_deferred_init_no_early_feature_init, true);
    assert.deepEqual(summary.assertions, [
        {
            id: 'lazy-widget-no-early-feature-init',
            message: 'Observed 0 feature request(s) before trigger; expected <= 0.',
            status: 'pass',
        },
        {
            id: 'lazy-widget-post-trigger-feature-requests',
            message: 'Observed 2 feature request(s) after trigger; expected >= 2.',
            status: 'pass',
        },
        {
            id: 'lazy-widget-post-trigger-success',
            message: 'Feature reported post-trigger success.',
            status: 'pass',
        },
        {
            id: 'lazy-widget-no-early-third-party-init',
            message: 'Observed 0 third-party request(s) before trigger; expected <= 0.',
            status: 'pass',
        },
    ]);
    assert.deepEqual(summary.metadata.post_trigger_feature_urls_sample, [
        'https://cdn.example.test/lazy-widget.js',
        'https://api.example.test/lazy-widget/config',
    ]);
});

test('summarizeDeferredInitBrowserEvidence reports early initialization failures', () => {
    const markers = deferredInitBrowserMarkers('lazy-widget');
    const summary = summarizeDeferredInitBrowserEvidence({
        featureId: 'lazy-widget',
        markerEvents: [{ name: markers.featureNeededTrigger, t_ms: 500 }],
        networkEntries: [
            { url: 'https://cdn.example.test/lazy-widget.js', start_time_ms: 100 },
            { url: 'https://third-party.example.test/tag.js', start_time_ms: 150 },
        ],
        featureRequestMatchers: ['lazy-widget'],
        thirdPartyRequestMatchers: ['third-party.example'],
        maxEarlyFeatureRequests: 0,
        maxEarlyThirdPartyRequests: 0,
    });

    assert.equal(summary.metrics.lazy_widget_deferred_init_feature_request_count_before_trigger, 1);
    assert.equal(summary.metrics.lazy_widget_deferred_init_third_party_request_count_before_trigger, 1);
    assert.equal(summary.metrics.lazy_widget_deferred_init_no_early_feature_init, false);
    assert.equal(summary.metrics.lazy_widget_deferred_init_post_trigger_success, false);
    assert.deepEqual(summary.assertions.map((assertion) => assertion.status), ['fail', 'fail', 'fail', 'fail']);
    assert.deepEqual(summary.metadata.early_feature_urls_sample, ['https://cdn.example.test/lazy-widget.js']);
    assert.deepEqual(summary.metadata.early_third_party_urls_sample, ['https://third-party.example.test/tag.js']);
});
