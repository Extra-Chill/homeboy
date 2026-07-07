import { mkdir, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';
import {
    collectBrowserPhases,
    buildBrowserBenchResult,
    normalizeBrowserArtifact,
    normalizeBrowserBottleneck,
    normalizeBrowserPerformanceProfile,
} from '../lib/browser-result-shapes.mjs';

export { buildBrowserBenchResult };

const DEFAULT_NETWORK_IDLE_TIMEOUT_MS = 5000;
const DEFAULT_DEFERRED_INIT_MARKER_PREFIX = 'deferred_init';
const BROWSER_PERFORMANCE_STATE = new WeakMap();
const SECRET_HEADER_PATTERN = /^(authorization|cookie|set-cookie|proxy-authorization|x-api-key|x-auth-token|x-csrf-token|x-xsrf-token)$/i;
const SECRET_HEADER_PART_PATTERN = /(token|secret|session|cookie|credential|password|key)/i;

export const DEFERRED_INIT_BROWSER_PHASES = Object.freeze({
    FEATURE_NOT_NEEDED: 'feature-not-needed',
    FEATURE_NEEDED: 'feature-needed',
});

export function deferredInitBrowserMarkers(featureId, options = {}) {
    const normalizedFeatureId = normalizeDeferredInitFeatureId(featureId, 'deferredInitBrowserMarkers');
    const prefix = normalizeDeferredInitMarkerPrefix(options.prefix);

    return Object.freeze({
        featureNotNeededStart: deferredInitMarkerName(prefix, normalizedFeatureId, 'feature_not_needed.start'),
        featureNotNeededReady: deferredInitMarkerName(prefix, normalizedFeatureId, 'feature_not_needed.ready'),
        featureNeededTrigger: deferredInitMarkerName(prefix, normalizedFeatureId, 'feature_needed.trigger'),
        featureNeededReady: deferredInitMarkerName(prefix, normalizedFeatureId, 'feature_needed.ready'),
        featureNeededSuccess: deferredInitMarkerName(prefix, normalizedFeatureId, 'feature_needed.success'),
    });
}

export function deferredInitBrowserMarkerScript(featureId, options = {}) {
    const normalizedFeatureId = normalizeDeferredInitFeatureId(featureId, 'deferredInitBrowserMarkerScript');
    const markers = deferredInitBrowserMarkers(normalizedFeatureId, options);

    return `(() => {
  const startedAt = performance.now();
  const events = [];
  const elapsed = () => Math.round(performance.now() - startedAt);
  const mark = (name, data = {}) => {
    const event = { name, t_ms: elapsed(), data };
    events.push(event);
    try { performance.mark(name); } catch {}
    return event;
  };
  window.__homeboyDeferredInit = window.__homeboyDeferredInit || {};
  window.__homeboyDeferredInit[${JSON.stringify(normalizedFeatureId)}] = {
    featureId: ${JSON.stringify(normalizedFeatureId)},
    markers: ${JSON.stringify(markers)},
    events,
    mark,
  };
  mark(${JSON.stringify(markers.featureNotNeededStart)});
})();`;
}

export function summarizeDeferredInitBrowserEvidence(options = {}) {
    if (!options || typeof options !== 'object' || Array.isArray(options)) {
        throw new Error('summarizeDeferredInitBrowserEvidence requires an options object.');
    }

    const featureId = normalizeDeferredInitFeatureId(options.featureId, 'summarizeDeferredInitBrowserEvidence');
    const featureMatchers = normalizeDeferredInitMatchers(options.featureRequestMatchers, 'featureRequestMatchers', true);
    const thirdPartyMatchers = normalizeDeferredInitMatchers(options.thirdPartyRequestMatchers, 'thirdPartyRequestMatchers', false);
    const markerEvents = Array.isArray(options.markerEvents) ? options.markerEvents : [];
    const networkEntries = Array.isArray(options.networkEntries) ? options.networkEntries : [];
    const maxEarlyFeatureRequests = finiteNonNegativeNumber(options.maxEarlyFeatureRequests) ?? 0;
    const maxEarlyThirdPartyRequests = options.maxEarlyThirdPartyRequests === undefined || options.maxEarlyThirdPartyRequests === null
        ? null
        : finiteNonNegativeNumber(options.maxEarlyThirdPartyRequests);
    const minPostTriggerFeatureRequests = finiteNonNegativeNumber(options.minPostTriggerFeatureRequests) ?? 1;
    const metricsPrefix = sanitizeMetricName(options.metricsPrefix || `${featureId}_deferred_init`);
    const markers = deferredInitBrowserMarkers(featureId, { prefix: options.markerPrefix });
    const notNeededReadyMs = findDeferredInitMarkerTime(markerEvents, markers.featureNotNeededReady);
    const triggerMs = findDeferredInitMarkerTime(markerEvents, markers.featureNeededTrigger);
    const neededReadyMs = findDeferredInitMarkerTime(markerEvents, markers.featureNeededReady);
    const successMs = findDeferredInitMarkerTime(markerEvents, markers.featureNeededSuccess);
    const hasRequestTiming = networkEntries.some((entry) => deferredInitRequestTime(entry) !== null);
    const beforeTrigger = (entry) => {
        const time = deferredInitRequestTime(entry);
        return triggerMs !== null && time !== null && time < triggerMs;
    };
    const afterTrigger = (entry) => {
        const time = deferredInitRequestTime(entry);
        return triggerMs !== null && time !== null && time >= triggerMs;
    };
    const earlyFeatureRequests = countDeferredInitRequests(networkEntries, featureMatchers, beforeTrigger);
    const postTriggerFeatureRequests = countDeferredInitRequests(networkEntries, featureMatchers, afterTrigger);
    const earlyThirdPartyRequests = thirdPartyMatchers.length > 0 ? countDeferredInitRequests(networkEntries, thirdPartyMatchers, beforeTrigger) : null;
    const postTriggerThirdPartyRequests = thirdPartyMatchers.length > 0 ? countDeferredInitRequests(networkEntries, thirdPartyMatchers, afterTrigger) : null;
    const featureRequestCount = countDeferredInitRequests(networkEntries, featureMatchers);
    const thirdPartyRequestCount = thirdPartyMatchers.length > 0 ? countDeferredInitRequests(networkEntries, thirdPartyMatchers) : null;
    const earlyFeaturePass = hasRequestTiming && triggerMs !== null && earlyFeatureRequests <= maxEarlyFeatureRequests;
    const postTriggerFeaturePass = hasRequestTiming && triggerMs !== null && postTriggerFeatureRequests >= minPostTriggerFeatureRequests;
    const thirdPartyEarlyPass = maxEarlyThirdPartyRequests === null || (
        hasRequestTiming && triggerMs !== null && earlyThirdPartyRequests <= maxEarlyThirdPartyRequests
    );
    const successPass = options.success === true || successMs !== null;

    return stableJson({
        feature_id: featureId,
        phases: DEFERRED_INIT_BROWSER_PHASES,
        markers,
        metrics: {
            [`${metricsPrefix}_feature_not_needed_ready_ms`]: notNeededReadyMs,
            [`${metricsPrefix}_feature_needed_trigger_ms`]: triggerMs,
            [`${metricsPrefix}_feature_needed_ready_ms`]: neededReadyMs,
            [`${metricsPrefix}_feature_needed_success_ms`]: successMs,
            [`${metricsPrefix}_request_timing_available`]: hasRequestTiming,
            [`${metricsPrefix}_feature_request_count`]: featureRequestCount,
            [`${metricsPrefix}_feature_request_count_before_trigger`]: earlyFeatureRequests,
            [`${metricsPrefix}_feature_request_count_after_trigger`]: postTriggerFeatureRequests,
            [`${metricsPrefix}_third_party_request_count`]: thirdPartyRequestCount,
            [`${metricsPrefix}_third_party_request_count_before_trigger`]: earlyThirdPartyRequests,
            [`${metricsPrefix}_third_party_request_count_after_trigger`]: postTriggerThirdPartyRequests,
            [`${metricsPrefix}_no_early_feature_init`]: earlyFeaturePass,
            [`${metricsPrefix}_post_trigger_feature_requests`]: postTriggerFeaturePass,
            [`${metricsPrefix}_post_trigger_success`]: successPass,
        },
        assertions: [
            deferredInitAssertion(
                `${featureId}-no-early-feature-init`,
                earlyFeaturePass ? 'pass' : 'fail',
                `Observed ${earlyFeatureRequests} feature request(s) before trigger; expected <= ${maxEarlyFeatureRequests}.`
            ),
            deferredInitAssertion(
                `${featureId}-post-trigger-feature-requests`,
                postTriggerFeaturePass ? 'pass' : 'fail',
                `Observed ${postTriggerFeatureRequests} feature request(s) after trigger; expected >= ${minPostTriggerFeatureRequests}.`
            ),
            deferredInitAssertion(
                `${featureId}-post-trigger-success`,
                successPass ? 'pass' : 'fail',
                successPass ? 'Feature reported post-trigger success.' : 'Feature did not report post-trigger success.'
            ),
            ...(maxEarlyThirdPartyRequests === null ? [] : [
                deferredInitAssertion(
                    `${featureId}-no-early-third-party-init`,
                    thirdPartyEarlyPass ? 'pass' : 'fail',
                    `Observed ${earlyThirdPartyRequests} third-party request(s) before trigger; expected <= ${maxEarlyThirdPartyRequests}.`
                ),
            ]),
        ],
        metadata: {
            marker_events: markerEvents,
            early_feature_urls_sample: sampleDeferredInitUrls(networkEntries, featureMatchers, beforeTrigger),
            post_trigger_feature_urls_sample: sampleDeferredInitUrls(networkEntries, featureMatchers, afterTrigger),
            early_third_party_urls_sample: thirdPartyMatchers.length > 0 ? sampleDeferredInitUrls(networkEntries, thirdPartyMatchers, beforeTrigger) : [],
            post_trigger_third_party_urls_sample: thirdPartyMatchers.length > 0 ? sampleDeferredInitUrls(networkEntries, thirdPartyMatchers, afterTrigger) : [],
        },
    });
}

export async function runBrowserActions(page, actions, options = {}) {
    if (!Array.isArray(actions) || actions.length === 0) {
        return { actions: [], duration_ms: 0, failed: false };
    }
    if (!page || typeof page !== 'object') {
        throw new Error('runBrowserActions requires a Playwright page.');
    }

    const startedAt = performance.now();
    const evidence = [];
    for (let index = 0; index < actions.length; index += 1) {
        const normalized = normalizeBrowserAction(actions[index], index, options);
        const actionStartedAt = performance.now();
        const row = {
            index,
            name: normalized.name,
            type: normalized.type,
            target: normalized.target,
            timeout_ms: normalized.timeout,
            started_at_ms: roundNumber(actionStartedAt - startedAt),
        };
        evidence.push(row);
        try {
            const result = await executeBrowserAction(page, normalized);
            row.duration_ms = roundNumber(performance.now() - actionStartedAt);
            row.status = 'passed';
            if (result) row.result = result;
            if (typeof options.mark === 'function') {
                await options.mark(normalized.name || `interaction_${index + 1}`);
            }
        } catch (error) {
            row.duration_ms = roundNumber(performance.now() - actionStartedAt);
            row.status = 'failed';
            row.error = error?.message || String(error);
            await attachBrowserActionFailureEvidence(page, row, normalized, options).catch(() => {});
            const wrapped = new Error(formatBrowserActionFailure(row, error));
            wrapped.cause = error;
            wrapped.action = row;
            wrapped.actions = evidence;
            throw wrapped;
        }
    }

    return stableJson({
        actions: evidence,
        duration_ms: roundNumber(performance.now() - startedAt),
        failed: false,
    });
}

export async function installBrowserPerformanceObservers(page, options = {}) {
    if (!page || typeof page.on !== 'function' || typeof page.evaluate !== 'function') {
        throw new Error('installBrowserPerformanceObservers requires a Playwright page.');
    }

    const existing = BROWSER_PERFORMANCE_STATE.get(page);
    if (existing) return createBrowserPerformanceController(page, existing);

    const state = {
        options: normalizeBrowserPerformanceOptions(options),
        startedAt: performance.now(),
        network: [],
        consoleMessages: [],
        pageErrors: [],
        phaseMarks: [],
        requestStarts: new Map(),
    };

    BROWSER_PERFORMANCE_STATE.set(page, state);

    page.on('request', (request) => recordProfileRequest(state, request));
    page.on('response', (response) => recordProfileResponse(state, response));
    page.on('requestfinished', (request) => finishProfileRequest(state, request, false, null));
    page.on('requestfailed', (request) => finishProfileRequest(state, request, true, request.failure()?.errorText || 'request failed'));
    page.on('console', (message) => state.consoleMessages.push(normalizeConsoleMessage(message)));
    page.on('pageerror', (error) => state.pageErrors.push(normalizeError(error)));

    const browserObserverInstaller = () => {
        window.__homeboyBrowserPerformance = window.__homeboyBrowserPerformance || {
            largest_contentful_paint: [],
            layout_shifts: [],
            long_tasks: [],
            paints: [],
            phase_marks: [],
        };

        const target = window.__homeboyBrowserPerformance;
        const observe = (type, sink, mapper) => {
            if (!('PerformanceObserver' in window)) return;
            try {
                const observer = new PerformanceObserver((list) => {
                    for (const entry of list.getEntries()) sink.push(mapper(entry));
                });
                observer.observe({ type, buffered: true });
            } catch {
                // Some entry types are browser-specific; unsupported types are skipped.
            }
        };

        observe('largest-contentful-paint', target.largest_contentful_paint, (entry) => ({
            name: entry.name || '',
            start_time_ms: entry.startTime,
            render_time_ms: entry.renderTime,
            load_time_ms: entry.loadTime,
            size: entry.size,
            url: entry.url || '',
            element: entry.element ? entry.element.tagName.toLowerCase() : '',
        }));
        observe('layout-shift', target.layout_shifts, (entry) => ({
            name: entry.name || '',
            start_time_ms: entry.startTime,
            value: entry.value,
            had_recent_input: entry.hadRecentInput,
        }));
        observe('longtask', target.long_tasks, (entry) => ({
            name: entry.name || '',
            start_time_ms: entry.startTime,
            duration_ms: entry.duration,
        }));
        observe('paint', target.paints, (entry) => ({
            name: entry.name || '',
            start_time_ms: entry.startTime,
            duration_ms: entry.duration,
        }));
    };

    if (typeof page.addInitScript === 'function') {
        await page.addInitScript(browserObserverInstaller);
    }
    await page.evaluate(browserObserverInstaller);

    return createBrowserPerformanceController(page, state);
}

export async function collectBrowserPerformanceProfile(page, options = {}) {
    if (!page || typeof page.evaluate !== 'function') {
        throw new Error('collectBrowserPerformanceProfile requires a Playwright page.');
    }

    const state = BROWSER_PERFORMANCE_STATE.get(page) || {
        options: normalizeBrowserPerformanceOptions(options),
        startedAt: performance.now(),
        network: [],
        consoleMessages: [],
        pageErrors: [],
        phaseMarks: [],
        requestStarts: new Map(),
    };
    const config = normalizeBrowserPerformanceOptions({ ...state.options, ...options });
    const browserEntries = await page.evaluate(() => {
        const serialize = (entry) => {
            const json = entry.toJSON ? entry.toJSON() : {};
            return Object.fromEntries(
                Object.entries(json).filter(([, value]) => value === null || ['string', 'number', 'boolean'].includes(typeof value))
            );
        };
        const perf = window.__homeboyBrowserPerformance || {};
        return {
            url: window.location.href,
            navigation: performance.getEntriesByType('navigation').map(serialize),
            resources: performance.getEntriesByType('resource').map(serialize),
            paints: performance.getEntriesByType('paint').map(serialize),
            largest_contentful_paint: perf.largest_contentful_paint || [],
            layout_shifts: perf.layout_shifts || [],
            long_tasks: perf.long_tasks || [],
            phase_marks: [
                ...performance.getEntriesByType('mark')
                    .filter((entry) => entry.name.startsWith('homeboy:phase:'))
                    .map((entry) => ({ name: entry.name.replace(/^homeboy:phase:/, ''), start_time_ms: entry.startTime })),
                ...(perf.phase_marks || []),
            ],
        };
    });

    const navigation = browserEntries.navigation.map(normalizeNavigationEntry).sort(compareByNameThenStart);
    const resources = browserEntries.resources.map(normalizeResourceEntry).sort(compareByNameThenStart);
    const browserPhaseMarks = browserEntries.phase_marks.map((mark) => ({ name: sanitizePhaseName(mark.name), start_time_ms: finiteNumber(mark.start_time_ms) }));
    const browserPhaseNames = new Set(browserPhaseMarks.map((mark) => mark.name));
    const phaseMarks = [
        ...browserPhaseMarks,
        ...state.phaseMarks.filter((mark) => !browserPhaseNames.has(mark.name)),
    ].sort(comparePhaseMarks);

    return normalizeBrowserPerformanceProfile({
        schema_version: 1,
        page_url: browserEntries.url || '',
        summary: summarizeBrowserProfile({ navigation, resources, network: state.network, browserEntries, state }),
        navigation,
        resources,
        network: state.network.map((entry) => normalizeNetworkEntry(entry, config)).sort(compareNetworkEntries),
        console_messages: state.consoleMessages.slice().sort(compareByTypeTextLocation),
        page_errors: state.pageErrors.slice().sort(compareByMessage),
        paints: browserEntries.paints.map(normalizePaintEntry).sort(compareByNameThenStart),
        largest_contentful_paint: browserEntries.largest_contentful_paint.map(normalizeLargestContentfulPaintEntry).sort(compareByNameThenStart),
        layout_shifts: browserEntries.layout_shifts.map(normalizeLayoutShiftEntry).sort(compareByNameThenStart),
        long_tasks: browserEntries.long_tasks.map(normalizeLongTaskEntry).sort(compareByNameThenStart),
        phase_marks: phaseMarks,
        phases: collectBrowserPhases(phaseMarks),
    });
}

export function compareBrowserPerformanceProfiles({ baseline, candidate }, options = {}) {
    if (!baseline || !candidate) {
        throw new Error('compareBrowserPerformanceProfiles requires baseline and candidate profiles.');
    }

    const config = normalizeComparisonOptions(options);
    const baselineProfile = scopeBrowserPerformanceProfile(baseline, config);
    const candidateProfile = scopeBrowserPerformanceProfile(candidate, config);
    const baselineMetrics = profileComparisonMetrics(baselineProfile);
    const candidateMetrics = profileComparisonMetrics(candidateProfile);
    const metrics = {};
    for (const key of [...new Set([...Object.keys(baselineMetrics), ...Object.keys(candidateMetrics)])].sort()) {
        metrics[key] = compareMetric(baselineMetrics[key], candidateMetrics[key], config);
    }

    const phases = {};
    for (const name of [...new Set([...Object.keys(baseline.phases || {}), ...Object.keys(candidate.phases || {})])].sort()) {
        if (config.phaseNames.length > 0 && !config.phaseNames.includes(name)) continue;
        const comparison = compareMetric(baseline.phases?.[name]?.duration_ms, candidate.phases?.[name]?.duration_ms, config);
        phases[name] = {
            baseline_duration_ms: comparison.baseline,
            candidate_duration_ms: comparison.candidate,
            delta_ms: comparison.delta,
            percent_change: comparison.percent_change,
            status: comparison.status,
        };
    }

    const requests = compareProfileRequests(baselineProfile, candidateProfile, config);
    const transfer = compareTransferBytes(baselineProfile, candidateProfile, config);
    const slowestRequests = compareSlowestRequests(baselineProfile, candidateProfile, config);
    const failedRequests = compareFailedRequests(baselineProfile, candidateProfile, config);
    const lateRequests = compareLateRequests(baselineProfile, candidateProfile, config);
    const longTasks = compareLongTasks(baselineProfile, candidateProfile, config);
    const paints = comparePaintTimings(baselineProfile, candidateProfile, config);
    const errors = compareProfileErrors(baselineProfile, candidateProfile, config);

    return stableJson({
        schema_version: 1,
        summary: {
            requests_added: requests.added.length,
            requests_removed: requests.removed.length,
            requests_changed: requests.changed.length,
            transfer_bytes_delta: transfer.total.delta,
            failed_requests_delta: failedRequests.count.delta,
            late_requests_delta: lateRequests.count.delta,
            long_tasks_delta: longTasks.count.delta,
            console_errors_delta: errors.console_errors.delta,
            page_errors_delta: errors.page_errors.delta,
        },
        phase_scope: config.phaseNames,
        metrics,
        requests,
        transfer,
        slowest_requests: slowestRequests,
        failed_requests: failedRequests,
        late_requests: lateRequests,
        long_tasks: longTasks,
        paints,
        errors,
        phases,
    });
}

export function formatBrowserPerformanceDiffMarkdown(diff, options = {}) {
    const title = options.title || 'Browser performance diff';
    const maxRows = Number.isInteger(options.maxRows) && options.maxRows > 0 ? options.maxRows : 8;
    const lines = [`# ${title}`, ''];

    lines.push('## Summary', '');
    lines.push('| Area | Baseline | Candidate | Delta | Change | Status |');
    lines.push('| --- | ---: | ---: | ---: | ---: | --- |');
    for (const [name, metric] of summaryMetrics(diff)) {
        lines.push(formatMetricRow(name, metric));
    }

    const requestRows = [
        ['Requests added', countMetric(0, diff.requests?.added?.length || 0, diff.requests?.added?.length || 0)],
        ['Requests removed', countMetric(diff.requests?.removed?.length || 0, 0, -(diff.requests?.removed?.length || 0))],
        ['Requests changed', countMetric(0, diff.requests?.changed?.length || 0, diff.requests?.changed?.length || 0)],
    ];
    lines.push('', '## Requests', '', '| Area | Baseline | Candidate | Delta | Change | Status |', '| --- | ---: | ---: | ---: | ---: | --- |');
    for (const [name, metric] of requestRows) lines.push(formatMetricRow(name, metric));
    appendDetailRows(lines, 'Added requests', diff.requests?.added, maxRows, formatRequestDetailRow);
    appendDetailRows(lines, 'Removed requests', diff.requests?.removed, maxRows, formatRequestDetailRow);
    appendDetailRows(lines, 'Changed requests', diff.requests?.changed, maxRows, formatChangedRequestRow);

    appendMetricSection(lines, 'Transfer', [
        ['Total transfer bytes', diff.transfer?.total, 'bytes'],
        ['Network-after-ready bytes', diff.late_requests?.transfer_bytes, 'bytes'],
    ]);
    appendDetailRows(lines, 'Transfer changes by resource', diff.transfer?.changed, maxRows, formatTransferRow);

    appendMetricSection(lines, 'Timing', [
        ['Slowest request ms', diff.slowest_requests?.slowest_duration_ms, 'ms'],
        ['Late request count', diff.late_requests?.count],
        ['Late request total ms', diff.late_requests?.total_duration_ms, 'ms'],
        ['Long task count', diff.long_tasks?.count],
        ['Long task total ms', diff.long_tasks?.total_duration_ms, 'ms'],
        ['Long task max ms', diff.long_tasks?.max_duration_ms, 'ms'],
    ]);
    appendDetailRows(lines, 'Slowest request deltas', diff.slowest_requests?.changed, maxRows, formatRequestDurationRow);

    appendMetricSection(lines, 'Paint, Load, Ready, Idle', Object.entries(diff.paints || {}).map(([name, metric]) => [name, metric, 'ms']));
    appendMetricSection(lines, 'Errors', [
        ['Console messages', diff.errors?.console_messages],
        ['Console errors', diff.errors?.console_errors],
        ['Page errors', diff.errors?.page_errors],
    ]);

    const phaseRows = Object.entries(diff.phases || {});
    if (phaseRows.length > 0) {
        lines.push('', '## Phases', '', '| Phase | Baseline | Candidate | Delta | Change | Status |', '| --- | ---: | ---: | ---: | ---: | --- |');
        for (const [name, phase] of phaseRows) {
            lines.push(`| ${escapeMarkdownCell(name)} | ${formatValue(phase.baseline_duration_ms, 'ms')} | ${formatValue(phase.candidate_duration_ms, 'ms')} | ${formatSignedValue(phase.delta_ms, 'ms')} | ${formatPercent(phase.percent_change)} | ${phase.status} |`);
        }
    }

    return `${lines.join('\n')}\n`;
}

export function summarizeBrowserPerformanceProfile(profile, options = {}) {
    const config = normalizeTraceSummaryOptions(options);
    const scopedProfile = scopeBrowserPerformanceProfile(profile, { phaseNames: config.phaseNames });
    const network = scopedProfile.network || [];
    const resources = scopedProfile.resources || [];
    const longTasks = scopedProfile.long_tasks || [];
    const failedRequests = network.filter((entry) => entry.failed || finiteNumber(entry.status) >= 400);
    const slowestRequests = network
        .filter((entry) => finiteNumber(entry.duration_ms) > 0)
        .sort((a, b) => finiteNumber(b.duration_ms) - finiteNumber(a.duration_ms))
        .slice(0, config.maxItems);
    const transferFamilies = summarizeTransferFamilies(resources, config.maxItems);
    const bottlenecks = [
        ...slowestRequests.map((entry) => bottleneck('network', profilePhaseName(scopedProfile, entry, 'start_time_ms'), `${entry.method || 'GET'} ${entry.url || 'unknown'} took ${formatValue(finiteNumber(entry.duration_ms), 'ms')}`, {
            ...requestDetail(entry),
            initiator: requestInitiator(entry, resources),
        })),
        ...failedRequests.slice(0, config.maxItems).map((entry) => bottleneck('failed-request', profilePhaseName(scopedProfile, entry, 'start_time_ms'), `${entry.method || 'GET'} ${entry.url || 'unknown'} failed with ${entry.status ?? entry.failure_text ?? 'unknown error'}`, requestDetail(entry))),
        ...longTasks
            .filter((entry) => finiteNumber(entry.duration_ms) >= config.longTaskThresholdMs)
            .sort((a, b) => finiteNumber(b.duration_ms) - finiteNumber(a.duration_ms))
            .slice(0, config.maxItems)
            .map((entry) => bottleneck('long-task', profilePhaseName(scopedProfile, entry, 'start_time_ms'), `Main thread was busy for ${formatValue(finiteNumber(entry.duration_ms), 'ms')}`, {
                start_time_ms: finiteNumber(entry.start_time_ms),
                duration_ms: finiteNumber(entry.duration_ms),
                name: entry.name || '',
            })),
        ...transferFamilies.map((family) => bottleneck('transfer', 'all', `${family.family} transferred ${formatBytes(family.transfer_bytes)} across ${family.count} resource(s)`, family)),
        ...paintBottlenecks(scopedProfile),
        ...errorBottlenecks(scopedProfile),
    ].slice(0, config.maxBottlenecks);

    return stableJson({
        schema_version: 1,
        summary: {
            request_count: network.length,
            failed_request_count: failedRequests.length,
            long_task_count: longTasks.length,
            largest_request_ms: maxValue(network, 'duration_ms'),
            total_transfer_bytes: totalTransferBytes(scopedProfile),
            console_error_count: (scopedProfile.console_messages || []).filter(isConsoleError).length,
            page_error_count: (scopedProfile.page_errors || []).length,
            largest_contentful_paint_ms: profileMetric(scopedProfile, 'largest_contentful_paint_ms'),
            cumulative_layout_shift: profileMetric(scopedProfile, 'cumulative_layout_shift'),
        },
        phase_scope: config.phaseNames,
        bottlenecks,
    });
}

export function formatBrowserPerformanceSummaryMarkdown(summary, options = {}) {
    const title = options.title || 'Browser trace bottlenecks';
    const maxRows = Number.isInteger(options.maxRows) && options.maxRows > 0 ? options.maxRows : 12;
    const metrics = summary?.summary || {};
    const lines = [`# ${title}`, '', '## Summary', ''];

    lines.push('| Metric | Value |');
    lines.push('| --- | ---: |');
    lines.push(`| Requests | ${formatNumber(metrics.request_count)} |`);
    lines.push(`| Failed requests | ${formatNumber(metrics.failed_request_count)} |`);
    lines.push(`| Long tasks | ${formatNumber(metrics.long_task_count)} |`);
    lines.push(`| Largest request | ${formatValue(metrics.largest_request_ms, 'ms')} |`);
    lines.push(`| Total transfer | ${formatValue(metrics.total_transfer_bytes, 'bytes')} |`);
    lines.push(`| Console errors | ${formatNumber(metrics.console_error_count)} |`);
    lines.push(`| Page errors | ${formatNumber(metrics.page_error_count)} |`);
    lines.push(`| Largest contentful paint | ${formatValue(metrics.largest_contentful_paint_ms, 'ms')} |`);
    lines.push(`| Cumulative layout shift | ${formatNumber(metrics.cumulative_layout_shift)} |`);

    const bottlenecks = Array.isArray(summary?.bottlenecks) ? summary.bottlenecks : [];
    if (bottlenecks.length > 0) {
        lines.push('', '## Bottlenecks', '', '| Kind | Phase | Message |');
        lines.push('| --- | --- | --- |');
        for (const row of bottlenecks.slice(0, maxRows)) {
            lines.push(`| ${escapeMarkdownCell(row.kind)} | ${escapeMarkdownCell(row.phase || 'all')} | ${escapeMarkdownCell(row.message)} |`);
        }
        if (bottlenecks.length > maxRows) lines.push(`| ... | | ${bottlenecks.length - maxRows} more |`);
    }

    return `${lines.join('\n')}\n`;
}

export function formatBrowserPerformanceReport(comparison, options = {}) {
    return formatBrowserPerformanceDiffMarkdown(comparison, { title: options.title || 'Browser performance comparison', ...options });
}

export async function runBrowserBench(options) {
    const config = normalizeOptions(options);
    const playwright = await loadPlaywright();
    const browserType = playwright[config.browserName];
    if (!browserType || typeof browserType.launch !== 'function') {
        throw new Error(`Unknown Playwright browser "${config.browserName}". Expected chromium, firefox, or webkit.`);
    }

    await mkdir(config.artifactsDir, { recursive: true });

    const network = [];
    const consoleMessages = [];
    const requestStarts = new Map();
    const metrics = {};
    const artifacts = {};
    const start = performance.now();
    let browser;
    let context;
    let page;
    let tracePath;
    let performanceController;

    const mark = async (name) => {
        const key = `${sanitizeMetricName(name)}_ms`;
        metrics[key] = performance.now() - start;
        if (performanceController) await performanceController.markPhase(name);
        return metrics[key];
    };

    try {
        browser = await launchBrowser(browserType, config);
        context = await browser.newContext(config.contextOptions);
        if (config.trace) {
            tracePath = join(config.artifactsDir, `${config.id}-trace.zip`);
            await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
        }
        page = await context.newPage();
        attachPageObservers(page, network, consoleMessages, requestStarts);
        performanceController = await installBrowserPerformanceObservers(page);

        const actionFailureOptions = {
            mark,
            failureScreenshotPath: join(config.artifactsDir, `${config.id}-interaction-failure.png`),
            tracePath,
        };
        await config.action({ browser, context, page, mark, runActions: (actions, actionOptions = {}) => runBrowserActions(page, actions, { ...actionFailureOptions, ...actionOptions }) });
        if (config.actions.length > 0) {
            const interactionEvidence = await runBrowserActions(page, config.actions, { ...actionFailureOptions, ...config.actionOptions });
            metrics.browser_interaction_count = interactionEvidence.actions.length;
            metrics.browser_interaction_duration_ms = interactionEvidence.duration_ms;
            artifacts.interactions = normalizeBrowserArtifact({
                path: join(config.artifactsDir, `${config.id}-interactions.json`),
                kind: 'browser-interactions',
                label: 'Browser interaction evidence',
            });
            await writeJson(artifacts.interactions.path, interactionEvidence);
        }

        if (config.waitForNetworkIdle) {
            await recordNetworkIdle(page, metrics, start, config.networkIdleTimeoutMs);
        }

        Object.assign(metrics, await collectNavigationMetrics(page));
        Object.assign(metrics, collectNetworkMetrics(network));

        const performanceProfile = await performanceController.collect();
        const profilePath = join(config.artifactsDir, `${config.id}-browser-profile.json`);
        await writeJson(profilePath, performanceProfile);
        artifacts.browserProfile = normalizeBrowserArtifact({
            path: profilePath,
            kind: 'browser-performance-profile',
            label: 'Browser performance profile',
        });

        const traceSummary = summarizeBrowserPerformanceProfile(performanceProfile);
        Object.assign(metrics, collectBrowserSummaryMetrics(traceSummary));
        const traceSummaryPath = join(config.artifactsDir, `${config.id}-trace-summary.json`);
        await writeJson(traceSummaryPath, traceSummary);
        artifacts.traceSummary = normalizeBrowserArtifact({
            path: traceSummaryPath,
            kind: 'browser-trace-summary',
            label: 'Browser trace bottleneck summary',
        });

        const traceSummaryMarkdownPath = join(config.artifactsDir, `${config.id}-trace-summary.md`);
        await writeFile(traceSummaryMarkdownPath, formatBrowserPerformanceSummaryMarkdown(traceSummary, { title: `${config.id} browser trace bottlenecks` }));
        artifacts.traceSummaryMarkdown = normalizeBrowserArtifact({
            path: traceSummaryMarkdownPath,
            kind: 'browser-trace-summary-markdown',
            label: 'Browser trace bottleneck summary markdown',
        });

        if (config.screenshot) {
            const screenshotPath = join(config.artifactsDir, `${config.id}-screenshot.png`);
            await page.screenshot({ path: screenshotPath, fullPage: true });
            artifacts.screenshot = normalizeBrowserArtifact({
                path: screenshotPath,
                kind: 'screenshot',
                label: 'Final screenshot',
            });
        }

        const networkPath = join(config.artifactsDir, `${config.id}-network.json`);
        await writeJson(networkPath, network);
        artifacts.network = normalizeBrowserArtifact({
            path: networkPath,
            kind: 'network-log',
            label: 'Network log',
        });

        const consolePath = join(config.artifactsDir, `${config.id}-console.json`);
        await writeJson(consolePath, consoleMessages);
        artifacts.console = normalizeBrowserArtifact({
            path: consolePath,
            kind: 'console-log',
            label: 'Console log',
        });

        if (config.trace) {
            await context.tracing.stop({ path: tracePath });
            artifacts.trace = normalizeBrowserArtifact({
                path: tracePath,
                kind: 'playwright-trace',
                label: 'Playwright trace',
            });
        }
    } catch (error) {
        if (page && typeof page.screenshot === 'function') {
            const screenshotPath = join(config.artifactsDir, `${config.id}-interaction-failure.png`);
            await page.screenshot({ path: screenshotPath, fullPage: true }).catch(() => {});
            error.screenshotPath = screenshotPath;
        }
        if (context && config.trace && tracePath) {
            await context.tracing.stop({ path: tracePath }).catch(() => {});
            error.tracePath = tracePath;
        }
        throw error;
    } finally {
        if (context && config.trace) {
            try {
                await context.tracing.stop();
            } catch {
                // Trace may already be stopped after a successful run.
            }
        }
        if (browser) {
            await browser.close();
        }
    }

    return { metrics, artifacts };
}

export async function runBrowserPageScenario(options) {
    const config = normalizePageScenarioOptions(options);
    let response = null;

    const result = await config.browserBench({
        ...config.browserOptions,
        id: config.id,
        artifactsDir: config.artifactsDir,
        action: async (context) => {
            const { page, mark } = context;
            if (config.target) {
                response = await page.goto(config.target, config.gotoOptions);
                await mark('page_loaded');
            }
            if (config.action) {
                await config.action({ ...context, response, target: config.target });
            }
            await runPageScenarioAssertions({ assertions: config.pageAssertions, page, response, target: config.target });
        },
    });

    const metrics = stableJson(result.metrics || {});
    let artifacts = stableJson({ ...(result.artifacts || {}), ...config.artifacts });
    await runPageScenarioAssertions({ assertions: config.artifactAssertions, artifacts, metrics, target: config.target });

    const rawResult = stableJson({
        artifacts,
        id: config.id,
        metadata: config.metadata,
        metrics,
        target: config.target,
    });
    const rawResultPath = join(config.artifactsDir, `${config.id}-raw-result.json`);
    await writeJson(rawResultPath, config.sanitizeRawResult ? await config.sanitizeRawResult(rawResult) : rawResult);
    artifacts = stableJson({
        ...artifacts,
        rawResult: normalizeBrowserArtifact({
            path: rawResultPath,
            kind: 'browser-page-scenario-result',
            label: 'Browser page scenario raw result',
        }),
    });

    if (config.sanitizeArtifacts) {
        const sanitized = await config.sanitizeArtifacts({ artifacts, metrics, id: config.id, target: config.target });
        if (sanitized !== undefined) artifacts = stableJson(sanitized);
    }

    await runPageScenarioAssertions({ assertions: config.postSanitizeArtifactAssertions, artifacts, metrics, target: config.target });

    return { metrics, artifacts };
}

function normalizeOptions(options) {
    if (!options || typeof options !== 'object' || Array.isArray(options)) {
        throw new Error('runBrowserBench requires an options object.');
    }
    if (typeof options.action !== 'function' && !Array.isArray(options.actions)) {
        throw new Error('runBrowserBench requires an async action({ page, mark }) function or an actions array.');
    }

    const id = sanitizeFilePart(options.id || 'browser-bench');
    const componentPath = process.env.HOMEBOY_COMPONENT_PATH || process.cwd();
    const artifactsDir = resolve(
        options.artifactsDir ||
        process.env.HOMEBOY_BENCH_ARTIFACTS_DIR ||
        join(componentPath, '.homeboy-bench-artifacts', id)
    );

    return {
        id,
        artifactsDir,
        action: typeof options.action === 'function' ? options.action : async () => {},
        actions: Array.isArray(options.actions) ? options.actions : [],
        actionOptions: options.actionOptions || {},
        browserName: options.browserName || 'chromium',
        headless: options.headless !== false,
        trace: options.trace !== false,
        screenshot: options.screenshot !== false,
        waitForNetworkIdle: options.waitForNetworkIdle !== false,
        networkIdleTimeoutMs: Number(options.networkIdleTimeoutMs) || DEFAULT_NETWORK_IDLE_TIMEOUT_MS,
        launchOptions: options.launchOptions || {},
        contextOptions: options.contextOptions || {},
    };
}

function normalizePageScenarioOptions(options) {
    if (!options || typeof options !== 'object' || Array.isArray(options)) {
        throw new Error('runBrowserPageScenario requires an options object.');
    }

    const id = sanitizeFilePart(options.id || 'browser-page-scenario');
    const componentPath = process.env.HOMEBOY_COMPONENT_PATH || process.cwd();
    const artifactsDir = resolve(
        options.artifactsDir ||
        options.artifactDir ||
        process.env.HOMEBOY_BENCH_ARTIFACTS_DIR ||
        join(componentPath, '.homeboy-bench-artifacts', id)
    );
    const assertions = Array.isArray(options.assertions) ? options.assertions : [];
    const pageAssertions = assertions.filter((assertion) => !isArtifactAssertion(assertion));
    const artifactAssertions = assertions.filter(isArtifactAssertion);
    const postSanitizeArtifactAssertions = Array.isArray(options.postSanitizeAssertions) ? options.postSanitizeAssertions.filter(isArtifactAssertion) : [];
    const browserBench = options.browserBench || runBrowserBench;

    if (typeof browserBench !== 'function') {
        throw new Error('runBrowserPageScenario requires browserBench to be a function when provided.');
    }

    return {
        id,
        artifactsDir,
        target: typeof options.target === 'string' && options.target.trim() !== '' ? options.target : '',
        gotoOptions: options.gotoOptions || {},
        action: typeof options.action === 'function' ? options.action : null,
        artifacts: normalizeStaticArtifacts(options.artifacts),
        metadata: normalizeJsonObject(options.metadata, 'metadata'),
        sanitizeArtifacts: normalizeOptionalFunction(options.sanitizeArtifacts, 'sanitizeArtifacts'),
        sanitizeRawResult: normalizeOptionalFunction(options.sanitizeRawResult, 'sanitizeRawResult'),
        browserBench,
        pageAssertions,
        artifactAssertions,
        postSanitizeArtifactAssertions,
        browserOptions: {
            actions: Array.isArray(options.actions) ? options.actions : [],
            actionOptions: options.actionOptions || {},
            browserName: options.browserName,
            contextOptions: options.contextOptions,
            headless: options.headless,
            launchOptions: options.launchOptions,
            networkIdleTimeoutMs: options.networkIdleTimeoutMs,
            screenshot: options.screenshot,
            trace: options.trace,
            waitForNetworkIdle: options.waitForNetworkIdle,
        },
    };
}

async function runPageScenarioAssertions(context) {
    const assertions = Array.isArray(context.assertions) ? context.assertions : [];
    for (let index = 0; index < assertions.length; index += 1) {
        const assertion = assertions[index];
        if (typeof assertion === 'function') {
            await assertion({ ...context, assert: pageScenarioAssert });
            continue;
        }
        await runPageScenarioAssertion(assertion, index, context);
    }
}

async function runPageScenarioAssertion(assertion, index, context) {
    if (!assertion || typeof assertion !== 'object' || Array.isArray(assertion)) {
        throw new Error(`Browser page scenario assertion ${index} must be an object or function.`);
    }

    const type = assertion.type || inferPageScenarioAssertionType(assertion);
    if (type === 'status') {
        const status = typeof context.response?.status === 'function' ? context.response.status() : context.response?.status;
        const expected = assertion.expected ?? assertion.expectedStatus ?? assertion.status ?? 200;
        pageScenarioAssert(Number(status) === Number(expected), assertion.message || `Expected page status ${expected}, got ${status ?? 'none'}.`);
        return;
    }
    if (type === 'selector') {
        pageScenarioAssert(context.page && typeof context.page.waitForSelector === 'function', 'Selector assertion requires page.waitForSelector().');
        await context.page.waitForSelector(assertion.selector, { state: assertion.state || 'visible', timeout: assertion.timeout || 30000 });
        return;
    }
    if (type === 'text') {
        pageScenarioAssert(context.page && typeof context.page.getByText === 'function', 'Text assertion requires page.getByText().');
        await context.page.getByText(assertion.text, { exact: assertion.exact }).waitFor({ timeout: assertion.timeout || 30000 });
        return;
    }
    if (type === 'title') {
        pageScenarioAssert(context.page && typeof context.page.title === 'function', 'Title assertion requires page.title().');
        const title = await context.page.title();
        const expected = assertion.includes ?? assertion.title;
        pageScenarioAssert(String(title).includes(String(expected)), assertion.message || `Expected page title to include "${expected}", got "${title}".`);
        return;
    }
    if (type === 'artifact') {
        const artifactKey = assertion.key || assertion.artifact;
        const artifact = context.artifacts?.[artifactKey];
        pageScenarioAssert(Boolean(artifact), assertion.message || `Expected artifact "${artifactKey}" to be present.`);
        if (assertion.kind !== undefined) {
            pageScenarioAssert(artifact.kind === assertion.kind, assertion.message || `Expected artifact "${artifactKey}" kind "${assertion.kind}", got "${artifact.kind}".`);
        }
        return;
    }

    throw new Error(`Unsupported browser page scenario assertion type: ${type}`);
}

function inferPageScenarioAssertionType(assertion) {
    if (assertion.status !== undefined || assertion.expectedStatus !== undefined) return 'status';
    if (assertion.selector !== undefined) return 'selector';
    if (assertion.text !== undefined) return 'text';
    if (assertion.title !== undefined || assertion.includes !== undefined) return 'title';
    if (assertion.key !== undefined || assertion.artifact !== undefined) return 'artifact';
    return assertion.type;
}

function isArtifactAssertion(assertion) {
    if (typeof assertion === 'function') return false;
    if (!assertion || typeof assertion !== 'object' || Array.isArray(assertion)) return false;
    return inferPageScenarioAssertionType(assertion) === 'artifact';
}

function pageScenarioAssert(condition, message) {
    if (!condition) throw new Error(`Browser page scenario assertion failed: ${message}`);
}

function normalizeStaticArtifacts(artifacts) {
    if (artifacts === undefined) return {};
    if (!artifacts || typeof artifacts !== 'object' || Array.isArray(artifacts)) {
        throw new Error('runBrowserPageScenario artifacts must be an object when provided.');
    }
    return stableJson(artifacts);
}

function normalizeJsonObject(value, label) {
    if (value === undefined) return {};
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
        throw new Error(`runBrowserPageScenario ${label} must be an object when provided.`);
    }
    return stableJson(JSON.parse(JSON.stringify(value)));
}

function normalizeOptionalFunction(value, label) {
    if (value === undefined) return null;
    if (typeof value !== 'function') throw new Error(`runBrowserPageScenario ${label} must be a function when provided.`);
    return value;
}

function normalizeBrowserAction(action, index, options = {}) {
    if (!action || typeof action !== 'object' || Array.isArray(action)) {
        throw new Error(`Browser action ${index} must be an object.`);
    }

    const timeout = finitePositiveNumber(action.timeout ?? options.timeout) || 30000;
    const name = sanitizeMetricName(action.name || `interaction_${index + 1}`);
    const candidates = [
        ['click', action.click],
        ['clickSelector', action.clickSelector],
        ['clickRole', action.clickRole],
        ['clickText', action.clickText],
        ['fill', action.fill],
        ['select', action.select],
        ['waitForSelector', action.waitForSelector],
        ['waitForResponse', action.waitForResponse],
        ['sleep', action.sleep],
    ].filter(([, value]) => value !== undefined);
    if (candidates.length !== 1) {
        throw new Error(`Browser action ${index} must define exactly one supported action.`);
    }

    const [type, rawSpec] = candidates[0];
    const spec = typeof rawSpec === 'string' || typeof rawSpec === 'number' ? { value: rawSpec } : { ...(rawSpec || {}) };
    return { index, name, type, timeout, spec, target: describeBrowserAction(type, spec) };
}

async function executeBrowserAction(page, action) {
    const { type, spec, timeout } = action;
    if (type === 'sleep') {
        await new Promise((resolve) => setTimeout(resolve, Number(spec.ms ?? spec.value ?? 0)));
        return null;
    }
    if (type === 'waitForResponse') {
        if (typeof page.waitForResponse !== 'function') throw new Error('waitForResponse requires page.waitForResponse().');
        const response = await page.waitForResponse((candidate) => responseMatches(candidate, spec), { timeout });
        return normalizeActionResponse(response);
    }
    if (type === 'waitForSelector') {
        const selector = requiredString(spec.selector ?? spec.value, 'waitForSelector.selector');
        if (Number.isInteger(spec.index)) {
            const locator = selectorLocator(page, selector, spec.index);
            await locator.waitFor({ state: spec.state || 'visible', timeout });
            return null;
        }
        if (typeof page.waitForSelector !== 'function') throw new Error('waitForSelector requires page.waitForSelector().');
        await page.waitForSelector(selector, { state: spec.state || 'visible', timeout });
        return null;
    }
    if (type === 'clickRole') {
        const role = requiredString(spec.role, 'clickRole.role');
        const locator = indexedLocator(page.getByRole(role, roleOptions(spec)), spec.index);
        await locator.click({ timeout });
        return null;
    }
    if (type === 'clickText') {
        const text = requiredString(spec.text ?? spec.value, 'clickText.text');
        const locator = indexedLocator(page.getByText(text, { exact: spec.exact }), spec.index);
        await locator.click({ timeout });
        return null;
    }
    if (type === 'click' || type === 'clickSelector') {
        const selector = requiredString(spec.selector ?? spec.value, `${type}.selector`);
        if (Number.isInteger(spec.index)) {
            await selectorLocator(page, selector, spec.index).click({ timeout });
            return null;
        }
        if (typeof page.click === 'function') {
            await page.click(selector, { timeout });
            return null;
        }
        await selectorLocator(page, selector, 0).click({ timeout });
        return null;
    }
    if (type === 'fill') {
        const selector = requiredString(spec.selector, 'fill.selector');
        await selectorLocator(page, selector, spec.index).fill(String(spec.value ?? ''), { timeout });
        return null;
    }
    if (type === 'select') {
        const selector = requiredString(spec.selector, 'select.selector');
        await selectorLocator(page, selector, spec.index).selectOption(selectOptionValue(spec), { timeout });
        return null;
    }
    throw new Error(`Unsupported browser action type: ${type}`);
}

function selectorLocator(page, selector, index = 0) {
    if (typeof page.locator !== 'function') throw new Error('selector actions require page.locator().');
    return indexedLocator(page.locator(selector), index);
}

function indexedLocator(locator, index = 0) {
    if (Number.isInteger(index) && index > 0) return locator.nth(index);
    return locator;
}

function roleOptions(spec) {
    const options = {};
    if (spec.name !== undefined) options.name = spec.name;
    if (spec.exact !== undefined) options.exact = spec.exact;
    return options;
}

function selectOptionValue(spec) {
    if (spec.optionIndex !== undefined) return { index: spec.optionIndex };
    if (spec.label !== undefined) return { label: spec.label };
    if (spec.value !== undefined) return spec.value;
    throw new Error('select requires value, label, or optionIndex.');
}

function responseMatches(response, spec) {
    const url = typeof response.url === 'function' ? response.url() : response.url;
    const method = typeof response.request === 'function' ? response.request()?.method?.() : undefined;
    const status = typeof response.status === 'function' ? response.status() : response.status;
    if (spec.method && String(method || '').toUpperCase() !== String(spec.method).toUpperCase()) return false;
    if (spec.status !== undefined && Number(status) !== Number(spec.status)) return false;
    if (spec.substring && !String(url).includes(spec.substring)) return false;
    if (spec.url && !String(url).includes(spec.url)) return false;
    if (spec.pattern && !(new RegExp(spec.pattern).test(String(url)))) return false;
    return Boolean(spec.substring || spec.url || spec.pattern || spec.method || spec.status !== undefined);
}

function normalizeActionResponse(response) {
    if (!response) return null;
    return {
        url: typeof response.url === 'function' ? response.url() : response.url,
        status: typeof response.status === 'function' ? response.status() : response.status,
    };
}

async function attachBrowserActionFailureEvidence(page, row, action, options) {
    if (typeof page.screenshot === 'function' && options.failureScreenshotPath) {
        await page.screenshot({ path: options.failureScreenshotPath, fullPage: true });
        row.screenshot = options.failureScreenshotPath;
    }
    if (options.tracePath) row.trace = options.tracePath;
    row.action = { name: action.name, type: action.type, target: action.target };
}

function formatBrowserActionFailure(row, error) {
    const parts = [
        `Browser action ${row.index} (${row.name || row.type}) failed`,
        `type=${row.type}`,
        `target=${row.target || 'unknown'}`,
        `timeout=${row.timeout_ms}ms`,
    ];
    if (row.screenshot) parts.push(`screenshot=${row.screenshot}`);
    if (row.trace) parts.push(`trace=${row.trace}`);
    parts.push(error?.message || String(error));
    return parts.join('; ');
}

function describeBrowserAction(type, spec) {
    if (type === 'clickRole') return `role:${spec.role}${spec.name !== undefined ? ` name:${spec.name}` : ''}`;
    if (type === 'clickText') return `text:${spec.text ?? spec.value ?? ''}`;
    if (type === 'waitForResponse') return `response:${spec.substring || spec.url || spec.pattern || spec.method || spec.status || ''}`;
    if (type === 'sleep') return `${spec.ms ?? spec.value ?? 0}ms`;
    return spec.selector ?? spec.value ?? '';
}

function requiredString(value, label) {
    if (typeof value !== 'string' || value.trim() === '') throw new Error(`${label} must be a non-empty string.`);
    return value;
}

function finitePositiveNumber(value) {
    const number = Number(value);
    return Number.isFinite(number) && number > 0 ? number : undefined;
}

async function loadPlaywright() {
    const require = createRequire(import.meta.url);
    const searchPaths = [process.cwd()];
    if (process.env.HOMEBOY_COMPONENT_PATH) searchPaths.push(process.env.HOMEBOY_COMPONENT_PATH);

    let resolved;
    try {
        resolved = require.resolve('playwright', { paths: searchPaths });
    } catch (err) {
        throw new Error([
            'Playwright is required for runBrowserBench but was not found.',
            'Install it in the benchmarked project with: npm i -D playwright',
            'Then install browser binaries with: npx playwright install chromium',
            `Resolution error: ${err.message}`,
        ].join('\n'));
    }

    try {
        const mod = await import(pathToFileURL(resolved).href);
        return mod.chromium ? mod : mod.default;
    } catch (err) {
        throw new Error(`Failed to load Playwright from ${resolved}: ${err.message}`);
    }
}

async function launchBrowser(browserType, config) {
    try {
        return await browserType.launch({ headless: config.headless, ...config.launchOptions });
    } catch (err) {
        throw new Error([
            'Playwright browser launch failed.',
            'If browser binaries are missing, run: npx playwright install chromium',
            'If system dependencies are missing, run Playwright\'s dependency installer for your platform.',
            `Launch error: ${err.message}`,
        ].join('\n'));
    }
}

function attachPageObservers(page, network, consoleMessages, requestStarts) {
    page.on('request', (request) => {
        requestStarts.set(request, performance.now());
        network.push({
            url: request.url(),
            method: request.method(),
            resource_type: request.resourceType(),
            status: null,
            failed: false,
            duration_ms: null,
        });
    });

    page.on('response', (response) => {
        const request = response.request();
        const entry = findNetworkEntry(network, request.url(), request.method());
        if (entry) {
            entry.status = response.status();
            entry.failed = entry.failed || response.status() >= 400;
        }
    });

    page.on('requestfinished', (request) => {
        finishNetworkEntry(network, requestStarts, request, false, null);
    });

    page.on('requestfailed', (request) => {
        finishNetworkEntry(network, requestStarts, request, true, request.failure()?.errorText || 'request failed');
    });

    page.on('console', (message) => {
        consoleMessages.push({
            type: message.type(),
            text: message.text(),
            location: message.location(),
        });
    });
}

function finishNetworkEntry(network, requestStarts, request, failed, failureText) {
    const entry = findNetworkEntry(network, request.url(), request.method());
    if (!entry) return;

    const startedAt = requestStarts.get(request);
    if (startedAt !== undefined) {
        entry.duration_ms = performance.now() - startedAt;
        requestStarts.delete(request);
    }
    if (failed) {
        entry.failed = true;
        entry.failure_text = failureText;
    }
}

function findNetworkEntry(network, url, method) {
    for (let i = network.length - 1; i >= 0; i--) {
        const entry = network[i];
        if (entry.url === url && entry.method === method) return entry;
    }
    return null;
}

async function recordNetworkIdle(page, metrics, start, timeout) {
    try {
        await page.waitForLoadState('networkidle', { timeout });
        metrics.browser_network_idle_ms = performance.now() - start;
    } catch {
        metrics.browser_network_idle_ms = timeout;
    }
}

async function collectNavigationMetrics(page) {
    const timing = await page.evaluate(() => {
        const nav = performance.getEntriesByType('navigation').at(-1);
        if (!nav) return null;
        return {
            domcontentloaded: nav.domContentLoadedEventEnd,
            load: nav.loadEventEnd,
        };
    });

    const metrics = {};
    if (timing) {
        if (Number.isFinite(timing.domcontentloaded)) {
            metrics.browser_domcontentloaded_ms = timing.domcontentloaded;
        }
        if (Number.isFinite(timing.load)) {
            metrics.browser_load_ms = timing.load;
        }
    }
    return metrics;
}

function collectNetworkMetrics(network) {
    const durations = network
        .map((entry) => entry.duration_ms)
        .filter((value) => typeof value === 'number' && Number.isFinite(value));

    return {
        browser_request_count: network.length,
        browser_failed_request_count: network.filter((entry) => entry.failed).length,
        browser_slowest_request_ms: durations.length > 0 ? Math.max(...durations) : 0,
    };
}

function collectBrowserSummaryMetrics(traceSummary) {
    const summary = traceSummary.summary || {};
    return {
        browser_long_task_count: finiteNumber(summary.long_task_count),
        browser_trace_bottleneck_count: Array.isArray(traceSummary.bottlenecks) ? traceSummary.bottlenecks.length : 0,
        browser_transfer_bytes: finiteNumber(summary.total_transfer_bytes),
        browser_console_error_count: finiteNumber(summary.console_error_count),
        browser_page_error_count: finiteNumber(summary.page_error_count),
        browser_largest_contentful_paint_ms: finiteNumber(summary.largest_contentful_paint_ms),
        browser_cumulative_layout_shift: finiteNumber(summary.cumulative_layout_shift),
    };
}

function normalizeTraceSummaryOptions(options) {
    return {
        phaseNames: normalizePhaseNames(options.phases ?? options.phase ?? options.phaseName),
        maxItems: Number.isInteger(options.maxItems) && options.maxItems > 0 ? options.maxItems : 5,
        maxBottlenecks: Number.isInteger(options.maxBottlenecks) && options.maxBottlenecks > 0 ? options.maxBottlenecks : 20,
        longTaskThresholdMs: Number.isFinite(options.longTaskThresholdMs) ? options.longTaskThresholdMs : 50,
    };
}

function bottleneck(kind, phase, message, data) {
    return normalizeBrowserBottleneck({ kind, phase: phase || 'all', message, data });
}

function summarizeTransferFamilies(resources, maxItems) {
    const families = new Map();
    for (const entry of resources) {
        const family = resourceFamily(entry);
        const current = families.get(family) || { family, count: 0, transfer_bytes: 0, duration_ms: 0 };
        current.count += 1;
        current.transfer_bytes += finiteNumber(entry.transfer_size);
        current.duration_ms += finiteNumber(entry.duration);
        families.set(family, current);
    }
    return [...families.values()]
        .map((entry) => stableJson({ ...entry, transfer_bytes: roundNumber(entry.transfer_bytes), duration_ms: roundNumber(entry.duration_ms) }))
        .filter((entry) => entry.transfer_bytes > 0)
        .sort((a, b) => b.transfer_bytes - a.transfer_bytes || a.family.localeCompare(b.family))
        .slice(0, maxItems);
}

function resourceFamily(entry) {
    const initiator = entry.initiator_type || entry.initiatorType || '';
    if (initiator) return initiator;
    const url = entry.name || entry.url || '';
    const extension = String(url).split('?')[0].match(/\.([a-z0-9]+)$/i)?.[1];
    return extension ? extension.toLowerCase() : 'other';
}

function requestInitiator(request, resources) {
    const resource = (resources || []).find((entry) => entry.name === request.url);
    return resource ? resourceFamily(resource) : request.resource_type || '';
}

function paintBottlenecks(profile) {
    const rows = [];
    const lcpMs = profileMetric(profile, 'largest_contentful_paint_ms');
    if (lcpMs > 0) {
        rows.push(bottleneck('paint', profilePhaseName(profile, { start_time_ms: lcpMs }, 'start_time_ms'), `Largest contentful paint at ${formatValue(lcpMs, 'ms')}`, { largest_contentful_paint_ms: lcpMs }));
    }
    const cls = profileMetric(profile, 'cumulative_layout_shift');
    if (cls > 0) {
        rows.push(bottleneck('layout-shift', 'all', `Cumulative layout shift is ${formatNumber(cls)}`, { cumulative_layout_shift: cls }));
    }
    return rows;
}

function errorBottlenecks(profile) {
    const consoleErrors = (profile.console_messages || []).filter(isConsoleError);
    const pageErrors = profile.page_errors || [];
    const rows = [];
    if (consoleErrors.length > 0) {
        rows.push(bottleneck('console-error', 'all', `${consoleErrors.length} console error(s) recorded`, { count: consoleErrors.length, examples: consoleErrors.slice(0, 3).map(consoleMessageKey) }));
    }
    if (pageErrors.length > 0) {
        rows.push(bottleneck('page-error', 'all', `${pageErrors.length} page error(s) recorded`, { count: pageErrors.length, examples: pageErrors.slice(0, 3).map(errorKey) }));
    }
    return rows;
}

function profilePhaseName(profile, entry, ...timeKeys) {
    const phases = Object.entries(profile.phases || {});
    const start = firstFinite(...timeKeys.map((key) => entry[key]));
    for (const [name, phase] of phases) {
        const phaseStart = finiteNumber(phase.start_time_ms);
        const phaseEnd = phase.end_time_ms === null || phase.end_time_ms === undefined ? Infinity : finiteNumber(phase.end_time_ms);
        if (start >= phaseStart && start <= phaseEnd) return name;
    }
    return 'all';
}

function normalizeBrowserPerformanceOptions(options) {
    return {
        includeHeaders: options.includeHeaders === true,
        redactHeaders: options.redactHeaders !== false,
        headerRedactionText: options.headerRedactionText || '[redacted]',
    };
}

function normalizeComparisonOptions(options) {
    const phaseNames = normalizePhaseNames(options.phases ?? options.phase ?? options.phaseName);
    return {
        thresholdPercent: Number.isFinite(options.thresholdPercent) ? options.thresholdPercent : 5,
        phaseNames,
    };
}

function normalizePhaseNames(value) {
    const values = Array.isArray(value) ? value : value ? [value] : [];
    return values.map(sanitizePhaseName).filter(Boolean).sort();
}

function scopeBrowserPerformanceProfile(profile, config) {
    if (!config.phaseNames || config.phaseNames.length === 0) return profile;

    const network = filterEntriesByPhases(profile, profile.network || [], config.phaseNames, 'start_time_ms');
    const resources = filterEntriesByPhases(profile, profile.resources || [], config.phaseNames, 'start_time');
    const paints = filterEntriesByPhases(profile, profile.paints || [], config.phaseNames, 'start_time_ms', 'start_time');
    const longTasks = filterEntriesByPhases(profile, profile.long_tasks || [], config.phaseNames, 'start_time_ms');

    return stableJson({
        ...profile,
        network,
        resources,
        paints,
        long_tasks: longTasks,
        summary: {
            ...(profile.summary || {}),
            failed_network_request_count: network.filter((entry) => entry.failed || finiteNumber(entry.status) >= 400).length,
            long_task_count: longTasks.length,
            long_task_total_ms: sumValues(longTasks, 'duration_ms'),
            network_request_count: network.length,
            resource_count: resources.length,
        },
    });
}

function filterEntriesByPhases(profile, entries, phaseNames, ...timeKeys) {
    return entries.filter((entry) => phaseNames.some((phaseName) => entryFallsInPhase(profile, entry, phaseName, timeKeys)));
}

function entryFallsInPhase(profile, entry, phaseName, timeKeys) {
    const phase = profile.phases?.[phaseName];
    if (!phase) return false;
    const start = firstFinite(...timeKeys.map((key) => entry[key]));
    const phaseStart = finiteNumber(phase.start_time_ms);
    const phaseEnd = phase.end_time_ms === null || phase.end_time_ms === undefined ? Infinity : finiteNumber(phase.end_time_ms);
    return start >= phaseStart && start <= phaseEnd;
}

function createBrowserPerformanceController(page, state) {
    return {
        markPhase: async (name) => {
            const phase = sanitizePhaseName(name);
            const startTime = performance.now() - state.startedAt;
            state.phaseMarks.push({ name: phase, start_time_ms: roundNumber(startTime) });
            await page.evaluate((phaseName) => {
                const fullName = `homeboy:phase:${phaseName}`;
                performance.mark(fullName);
                window.__homeboyBrowserPerformance = window.__homeboyBrowserPerformance || { phase_marks: [] };
                window.__homeboyBrowserPerformance.phase_marks = window.__homeboyBrowserPerformance.phase_marks || [];
                window.__homeboyBrowserPerformance.phase_marks.push({ name: phaseName, start_time_ms: performance.now() });
            }, phase);
            return startTime;
        },
        collect: (options = {}) => collectBrowserPerformanceProfile(page, options),
    };
}

function recordProfileRequest(state, request) {
    const entry = {
        url: request.url(),
        method: request.method(),
        resource_type: request.resourceType(),
        status: null,
        failed: false,
        start_time_ms: performance.now() - state.startedAt,
        duration_ms: null,
    };
    if (state.options.includeHeaders) {
        entry.request_headers = sanitizeHeaders(request.headers(), state.options);
    }
    state.requestStarts.set(request, performance.now());
    state.network.push(entry);
}

function recordProfileResponse(state, response) {
    const request = response.request();
    const entry = findProfileNetworkEntry(state.network, request.url(), request.method());
    if (!entry) return;
    entry.status = response.status();
    entry.failed = entry.failed || response.status() >= 400;
    if (state.options.includeHeaders) {
        entry.response_headers = sanitizeHeaders(response.headers(), state.options);
    }
}

function finishProfileRequest(state, request, failed, failureText) {
    const entry = findProfileNetworkEntry(state.network, request.url(), request.method());
    if (!entry) return;

    const startedAt = state.requestStarts.get(request);
    if (startedAt !== undefined) {
        entry.duration_ms = performance.now() - startedAt;
        state.requestStarts.delete(request);
    }
    if (failed) {
        entry.failed = true;
        entry.failure_text = failureText;
    }
}

function findProfileNetworkEntry(network, url, method) {
    for (let i = network.length - 1; i >= 0; i--) {
        const entry = network[i];
        if (entry.url === url && entry.method === method && entry.duration_ms === null) return entry;
    }
    return findNetworkEntry(network, url, method);
}

function sanitizeHeaders(headers, options) {
    return Object.fromEntries(
        Object.entries(headers || {})
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([name, value]) => [name.toLowerCase(), shouldRedactHeader(name, options) ? options.headerRedactionText : String(value)])
    );
}

function shouldRedactHeader(name, options) {
    return options.redactHeaders && (SECRET_HEADER_PATTERN.test(name) || SECRET_HEADER_PART_PATTERN.test(name));
}

function normalizeConsoleMessage(message) {
    return stableJson({
        type: message.type(),
        text: message.text(),
        location: message.location(),
    });
}

function normalizeError(error) {
    return stableJson({
        name: error?.name || 'Error',
        message: error?.message || String(error),
        stack: typeof error?.stack === 'string' ? error.stack : '',
    });
}

function normalizeNavigationEntry(entry) {
    return pickRounded(entry, [
        'name',
        'entryType',
        'startTime',
        'duration',
        'initiatorType',
        'redirectStart',
        'redirectEnd',
        'fetchStart',
        'domainLookupStart',
        'domainLookupEnd',
        'connectStart',
        'connectEnd',
        'requestStart',
        'responseStart',
        'responseEnd',
        'domInteractive',
        'domContentLoadedEventStart',
        'domContentLoadedEventEnd',
        'domComplete',
        'loadEventStart',
        'loadEventEnd',
        'transferSize',
        'encodedBodySize',
        'decodedBodySize',
    ]);
}

function normalizeResourceEntry(entry) {
    return pickRounded(entry, [
        'name',
        'entryType',
        'startTime',
        'duration',
        'initiatorType',
        'nextHopProtocol',
        'renderBlockingStatus',
        'workerStart',
        'redirectStart',
        'redirectEnd',
        'fetchStart',
        'domainLookupStart',
        'domainLookupEnd',
        'connectStart',
        'connectEnd',
        'requestStart',
        'responseStart',
        'responseEnd',
        'transferSize',
        'encodedBodySize',
        'decodedBodySize',
    ]);
}

function normalizeNetworkEntry(entry, options) {
    const normalized = stableJson({
        url: entry.url || '',
        method: entry.method || '',
        resource_type: entry.resource_type || '',
        status: entry.status ?? null,
        failed: Boolean(entry.failed),
        start_time_ms: finiteOrNull(entry.start_time_ms),
        duration_ms: finiteOrNull(entry.duration_ms),
        failure_text: entry.failure_text || undefined,
    });
    if (options.includeHeaders) {
        if (entry.request_headers) normalized.request_headers = sanitizeHeaders(entry.request_headers, options);
        if (entry.response_headers) normalized.response_headers = sanitizeHeaders(entry.response_headers, options);
    }
    return stableJson(normalized);
}

function normalizePaintEntry(entry) {
    return pickRounded(entry, ['name', 'entryType', 'startTime', 'duration']);
}

function normalizeLargestContentfulPaintEntry(entry) {
    return stableJson({
        name: entry.name || '',
        start_time_ms: finiteNumber(entry.start_time_ms),
        render_time_ms: finiteNumber(entry.render_time_ms),
        load_time_ms: finiteNumber(entry.load_time_ms),
        size: finiteNumber(entry.size),
        url: entry.url || '',
        element: entry.element || '',
    });
}

function normalizeLayoutShiftEntry(entry) {
    return stableJson({
        name: entry.name || '',
        start_time_ms: finiteNumber(entry.start_time_ms),
        value: finiteNumber(entry.value),
        had_recent_input: Boolean(entry.had_recent_input),
    });
}

function normalizeLongTaskEntry(entry) {
    return stableJson({
        name: entry.name || '',
        start_time_ms: finiteNumber(entry.start_time_ms),
        duration_ms: finiteNumber(entry.duration_ms),
    });
}

function summarizeBrowserProfile({ navigation, resources, network, browserEntries, state }) {
    const nav = navigation.at(-1) || {};
    const lcp = (browserEntries.largest_contentful_paint || []).map((entry) => finiteNumber(entry.start_time_ms));
    return stableJson({
        navigation_count: navigation.length,
        resource_count: resources.length,
        network_request_count: network.length,
        failed_network_request_count: network.filter((entry) => entry.failed).length,
        dom_content_loaded_ms: finiteNumber(nav.dom_content_loaded_event_end),
        load_event_ms: finiteNumber(nav.load_event_end),
        largest_contentful_paint_ms: lcp.length > 0 ? Math.max(...lcp) : 0,
        cumulative_layout_shift: (browserEntries.layout_shifts || [])
            .filter((entry) => !entry.had_recent_input)
            .reduce((sum, entry) => sum + finiteNumber(entry.value), 0),
        long_task_count: (browserEntries.long_tasks || []).length,
        long_task_total_ms: (browserEntries.long_tasks || []).reduce((sum, entry) => sum + finiteNumber(entry.duration_ms), 0),
        console_message_count: state.consoleMessages.length,
        page_error_count: state.pageErrors.length,
    });
}

function profileComparisonMetrics(profile) {
    const summary = profile.summary || {};
    const readyMs = firstFinite(summary.ready_ms, summary.app_ready_ms, profile.ready_ms);
    const networkIdleMs = firstFinite(summary.network_idle_ms, summary.browser_network_idle_ms, profile.network_idle_ms);
    return {
        resource_count: finiteNumber(summary.resource_count),
        network_request_count: finiteNumber(summary.network_request_count),
        failed_network_request_count: finiteNumber(summary.failed_network_request_count),
        total_transfer_bytes: totalTransferBytes(profile),
        dom_content_loaded_ms: finiteNumber(summary.dom_content_loaded_ms),
        load_event_ms: finiteNumber(summary.load_event_ms),
        ready_ms: readyMs,
        network_idle_ms: networkIdleMs,
        first_contentful_paint_ms: paintTiming(profile, 'first-contentful-paint'),
        first_paint_ms: paintTiming(profile, 'first-paint'),
        largest_contentful_paint_ms: finiteNumber(summary.largest_contentful_paint_ms),
        cumulative_layout_shift: finiteNumber(summary.cumulative_layout_shift),
        long_task_count: finiteNumber(summary.long_task_count),
        long_task_total_ms: finiteNumber(summary.long_task_total_ms),
        console_message_count: finiteNumber(summary.console_message_count ?? profile.console_messages?.length),
        page_error_count: finiteNumber(summary.page_error_count ?? profile.page_errors?.length),
    };
}

function compareProfileRequests(baseline, candidate, config) {
    const baselineEntries = keyedRequests(baseline.network || []);
    const candidateEntries = keyedRequests(candidate.network || []);
    const added = [];
    const removed = [];
    const changed = [];

    for (const [key, entry] of candidateEntries) {
        if (!baselineEntries.has(key)) added.push(requestDetail(entry));
    }
    for (const [key, entry] of baselineEntries) {
        if (!candidateEntries.has(key)) removed.push(requestDetail(entry));
    }
    for (const [key, before] of baselineEntries) {
        const after = candidateEntries.get(key);
        if (!after) continue;
        const fields = compareRequestFields(before, after, config);
        if (Object.keys(fields).length > 0) {
            changed.push(stableJson({ key, request: requestIdentity(after), fields }));
        }
    }

    return stableJson({
        count: compareMetric(baselineEntries.size, candidateEntries.size, config),
        added: added.sort(compareRequestDetails),
        removed: removed.sort(compareRequestDetails),
        changed: changed.sort((a, b) => a.key.localeCompare(b.key)),
    });
}

function compareRequestFields(before, after, config) {
    const fields = {};
    for (const key of ['status', 'failed', 'resource_type', 'failure_text']) {
        const baselineValue = before[key] ?? null;
        const candidateValue = after[key] ?? null;
        if (baselineValue !== candidateValue) fields[key] = { baseline: baselineValue, candidate: candidateValue };
    }
    for (const key of ['duration_ms', 'start_time_ms']) {
        const comparison = compareMetric(before[key], after[key], config);
        if (comparison.delta !== 0) fields[key] = comparison;
    }
    return stableJson(fields);
}

function compareTransferBytes(baseline, candidate, config) {
    const baselineEntries = keyedResources(baseline.resources || []);
    const candidateEntries = keyedResources(candidate.resources || []);
    const changed = [];
    for (const [key, before] of baselineEntries) {
        const after = candidateEntries.get(key);
        if (!after) continue;
        const comparison = compareMetric(before.transfer_size, after.transfer_size, config);
        if (comparison.delta !== 0) changed.push(stableJson({ key, resource: resourceIdentity(after), transfer_bytes: comparison }));
    }
    return stableJson({
        total: compareMetric(totalTransferBytes(baseline), totalTransferBytes(candidate), config),
        added: [...candidateEntries].filter(([key]) => !baselineEntries.has(key)).map(([, entry]) => resourceDetail(entry)).sort(compareResourceDetails),
        removed: [...baselineEntries].filter(([key]) => !candidateEntries.has(key)).map(([, entry]) => resourceDetail(entry)).sort(compareResourceDetails),
        changed: changed.sort((a, b) => Math.abs(b.transfer_bytes.delta) - Math.abs(a.transfer_bytes.delta) || a.key.localeCompare(b.key)),
    });
}

function compareSlowestRequests(baseline, candidate, config) {
    const baselineSlowest = slowestRequest(baseline.network || []);
    const candidateSlowest = slowestRequest(candidate.network || []);
    const baselineEntries = keyedRequests(baseline.network || []);
    const candidateEntries = keyedRequests(candidate.network || []);
    const changed = [];
    for (const [key, before] of baselineEntries) {
        const after = candidateEntries.get(key);
        if (!after) continue;
        const duration = compareMetric(before.duration_ms, after.duration_ms, config);
        if (duration.delta !== 0) changed.push(stableJson({ key, request: requestIdentity(after), duration_ms: duration }));
    }
    return stableJson({
        baseline: baselineSlowest ? requestDetail(baselineSlowest) : null,
        candidate: candidateSlowest ? requestDetail(candidateSlowest) : null,
        slowest_duration_ms: compareMetric(baselineSlowest?.duration_ms, candidateSlowest?.duration_ms, config),
        changed: changed.sort((a, b) => Math.abs(b.duration_ms.delta) - Math.abs(a.duration_ms.delta) || a.key.localeCompare(b.key)),
    });
}

function compareFailedRequests(baseline, candidate, config) {
    const baselineFailed = (baseline.network || []).filter((entry) => entry.failed || finiteNumber(entry.status) >= 400);
    const candidateFailed = (candidate.network || []).filter((entry) => entry.failed || finiteNumber(entry.status) >= 400);
    return stableJson({
        count: compareMetric(baselineFailed.length, candidateFailed.length, config),
        added: diffRequestSet(baselineFailed, candidateFailed).added,
        removed: diffRequestSet(baselineFailed, candidateFailed).removed,
    });
}

function compareLateRequests(baseline, candidate, config) {
    const baselineLate = lateRequests(baseline);
    const candidateLate = lateRequests(candidate);
    return stableJson({
        ready_ms: compareMetric(profileReadyMs(baseline), profileReadyMs(candidate), config),
        count: compareMetric(baselineLate.length, candidateLate.length, config),
        total_duration_ms: compareMetric(sumValues(baselineLate, 'duration_ms'), sumValues(candidateLate, 'duration_ms'), config),
        transfer_bytes: compareMetric(transferBytesForRequests(baselineLate, baseline), transferBytesForRequests(candidateLate, candidate), config),
        added: diffRequestSet(baselineLate, candidateLate).added,
        removed: diffRequestSet(baselineLate, candidateLate).removed,
    });
}

function compareLongTasks(baseline, candidate, config) {
    const baselineTasks = baseline.long_tasks || [];
    const candidateTasks = candidate.long_tasks || [];
    return stableJson({
        count: compareMetric(baselineTasks.length, candidateTasks.length, config),
        total_duration_ms: compareMetric(sumValues(baselineTasks, 'duration_ms'), sumValues(candidateTasks, 'duration_ms'), config),
        max_duration_ms: compareMetric(maxValue(baselineTasks, 'duration_ms'), maxValue(candidateTasks, 'duration_ms'), config),
    });
}

function comparePaintTimings(baseline, candidate, config) {
    return stableJson({
        first_paint_ms: compareMetric(paintTiming(baseline, 'first-paint'), paintTiming(candidate, 'first-paint'), config),
        first_contentful_paint_ms: compareMetric(paintTiming(baseline, 'first-contentful-paint'), paintTiming(candidate, 'first-contentful-paint'), config),
        largest_contentful_paint_ms: compareMetric(profileMetric(baseline, 'largest_contentful_paint_ms'), profileMetric(candidate, 'largest_contentful_paint_ms'), config),
        dom_content_loaded_ms: compareMetric(profileMetric(baseline, 'dom_content_loaded_ms'), profileMetric(candidate, 'dom_content_loaded_ms'), config),
        load_event_ms: compareMetric(profileMetric(baseline, 'load_event_ms'), profileMetric(candidate, 'load_event_ms'), config),
        ready_ms: compareMetric(profileReadyMs(baseline), profileReadyMs(candidate), config),
        network_idle_ms: compareMetric(profileNetworkIdleMs(baseline), profileNetworkIdleMs(candidate), config),
    });
}

function compareProfileErrors(baseline, candidate, config) {
    const baselineConsole = baseline.console_messages || [];
    const candidateConsole = candidate.console_messages || [];
    const baselineConsoleErrors = baselineConsole.filter(isConsoleError);
    const candidateConsoleErrors = candidateConsole.filter(isConsoleError);
    const baselinePageErrors = baseline.page_errors || [];
    const candidatePageErrors = candidate.page_errors || [];
    return stableJson({
        console_messages: compareMetric(baselineConsole.length, candidateConsole.length, config),
        console_errors: compareMetric(baselineConsoleErrors.length, candidateConsoleErrors.length, config),
        page_errors: compareMetric(baselinePageErrors.length, candidatePageErrors.length, config),
        added_console_errors: diffTextSet(baselineConsoleErrors, candidateConsoleErrors, consoleMessageKey).added,
        removed_console_errors: diffTextSet(baselineConsoleErrors, candidateConsoleErrors, consoleMessageKey).removed,
        added_page_errors: diffTextSet(baselinePageErrors, candidatePageErrors, errorKey).added,
        removed_page_errors: diffTextSet(baselinePageErrors, candidatePageErrors, errorKey).removed,
    });
}

function compareMetric(baseline, candidate, config) {
    const before = finiteNumber(baseline);
    const after = finiteNumber(candidate);
    return {
        baseline: before,
        candidate: after,
        delta: roundNumber(after - before),
        percent_change: before === 0 ? null : roundNumber(((after - before) / before) * 100),
        threshold_percent: config.thresholdPercent,
        status: metricStatus(before, after, config.thresholdPercent),
    };
}

function countMetric(baseline, candidate, delta) {
    return { baseline, candidate, delta, percent_change: baseline === 0 ? null : (delta / baseline) * 100, status: delta === 0 ? 'unchanged' : delta > 0 ? 'regressed' : 'improved' };
}

function keyedRequests(entries) {
    const seen = new Map();
    const keyed = new Map();
    for (const entry of entries) {
        const identity = [entry.method || 'GET', entry.url || '', entry.resource_type || ''].join(' ');
        const count = (seen.get(identity) || 0) + 1;
        seen.set(identity, count);
        keyed.set(`${identity} #${count}`, entry);
    }
    return keyed;
}

function keyedResources(entries) {
    const seen = new Map();
    const keyed = new Map();
    for (const entry of entries) {
        const identity = [entry.name || '', entry.initiator_type || ''].join(' ');
        const count = (seen.get(identity) || 0) + 1;
        seen.set(identity, count);
        keyed.set(`${identity} #${count}`, entry);
    }
    return keyed;
}

function requestIdentity(entry) {
    return stableJson({
        method: entry.method || '',
        url: entry.url || '',
        resource_type: entry.resource_type || '',
    });
}

function requestDetail(entry) {
    return stableJson({
        ...requestIdentity(entry),
        status: entry.status ?? null,
        failed: Boolean(entry.failed),
        start_time_ms: finiteOrNull(entry.start_time_ms),
        duration_ms: finiteOrNull(entry.duration_ms),
        failure_text: entry.failure_text || undefined,
    });
}

function resourceIdentity(entry) {
    return stableJson({
        name: entry.name || '',
        initiator_type: entry.initiator_type || '',
    });
}

function resourceDetail(entry) {
    return stableJson({
        ...resourceIdentity(entry),
        duration_ms: finiteOrNull(entry.duration),
        transfer_bytes: finiteNumber(entry.transfer_size),
    });
}

function diffRequestSet(baseline, candidate) {
    const baselineEntries = keyedRequests(baseline);
    const candidateEntries = keyedRequests(candidate);
    return stableJson({
        added: [...candidateEntries].filter(([key]) => !baselineEntries.has(key)).map(([, entry]) => requestDetail(entry)).sort(compareRequestDetails),
        removed: [...baselineEntries].filter(([key]) => !candidateEntries.has(key)).map(([, entry]) => requestDetail(entry)).sort(compareRequestDetails),
    });
}

function diffTextSet(baseline, candidate, keyFn) {
    const baselineKeys = new Set(baseline.map(keyFn));
    const candidateKeys = new Set(candidate.map(keyFn));
    return stableJson({
        added: candidate.map(keyFn).filter((key) => !baselineKeys.has(key)).sort(),
        removed: baseline.map(keyFn).filter((key) => !candidateKeys.has(key)).sort(),
    });
}

function slowestRequest(entries) {
    return entries.reduce((slowest, entry) => finiteNumber(entry.duration_ms) > finiteNumber(slowest?.duration_ms) ? entry : slowest, null);
}

function lateRequests(profile) {
    const readyMs = profileReadyMs(profile);
    if (readyMs <= 0) return [];
    return (profile.network || []).filter((entry) => finiteNumber(entry.start_time_ms) > readyMs);
}

function totalTransferBytes(profile) {
    return sumValues(profile.resources || [], 'transfer_size');
}

function transferBytesForRequests(requests, profile) {
    const urls = new Set(requests.map((entry) => entry.url).filter(Boolean));
    return (profile.resources || []).reduce((sum, entry) => urls.has(entry.name) ? sum + finiteNumber(entry.transfer_size) : sum, 0);
}

function paintTiming(profile, paintName) {
    const paint = (profile.paints || []).find((entry) => entry.name === paintName);
    return finiteNumber(paint?.start_time_ms ?? paint?.start_time ?? paint?.startTime);
}

function profileMetric(profile, key) {
    return firstFinite(profile.summary?.[key], profile.metrics?.[key], profile[key]);
}

function profileReadyMs(profile) {
    return firstFinite(
        profile.summary?.ready_ms,
        profile.summary?.app_ready_ms,
        profile.metrics?.ready_ms,
        profile.ready_ms,
        phaseStart(profile, 'ready')
    );
}

function profileNetworkIdleMs(profile) {
    return firstFinite(
        profile.summary?.network_idle_ms,
        profile.summary?.browser_network_idle_ms,
        profile.metrics?.network_idle_ms,
        profile.metrics?.browser_network_idle_ms,
        profile.network_idle_ms,
        phaseStart(profile, 'network-idle')
    );
}

function phaseStart(profile, phaseName) {
    return finiteNumber((profile.phase_marks || []).find((mark) => mark.name === phaseName)?.start_time_ms);
}

function sumValues(entries, key) {
    return roundNumber(entries.reduce((sum, entry) => sum + finiteNumber(entry[key]), 0));
}

function maxValue(entries, key) {
    return entries.reduce((max, entry) => Math.max(max, finiteNumber(entry[key])), 0);
}

function firstFinite(...values) {
    for (const value of values) {
        if (typeof value === 'number' && Number.isFinite(value)) return roundNumber(value);
    }
    return 0;
}

function isConsoleError(entry) {
    return ['error', 'assert'].includes(String(entry.type || '').toLowerCase());
}

function consoleMessageKey(entry) {
    return [entry.type || '', entry.text || '', JSON.stringify(entry.location || {})].join(' ');
}

function errorKey(entry) {
    return [entry.name || 'Error', entry.message || '', entry.stack || ''].join(' ');
}

function compareRequestDetails(a, b) {
    return String(a.url || '').localeCompare(String(b.url || '')) || String(a.method || '').localeCompare(String(b.method || ''));
}

function compareResourceDetails(a, b) {
    return String(a.name || '').localeCompare(String(b.name || ''));
}

function metricStatus(baseline, candidate, thresholdPercent) {
    if (baseline === 0 && candidate === 0) return 'unchanged';
    if (baseline === 0) return candidate > 0 ? 'regressed' : 'improved';
    const percentChange = ((candidate - baseline) / baseline) * 100;
    if (percentChange > thresholdPercent) return 'regressed';
    if (percentChange < -thresholdPercent) return 'improved';
    return 'unchanged';
}

function pickRounded(source, keys) {
    const out = {};
    for (const key of keys) {
        if (source[key] === undefined) continue;
        const normalizedKey = key.replace(/[A-Z]/g, (match) => `_${match.toLowerCase()}`);
        out[normalizedKey] = typeof source[key] === 'number' ? roundNumber(source[key]) : source[key];
    }
    return stableJson(out);
}

function normalizeDeferredInitFeatureId(featureId, caller) {
    if (typeof featureId !== 'string' || featureId.trim() === '') {
        throw new Error(`${caller} requires a non-empty string featureId.`);
    }
    return featureId.trim();
}

function normalizeDeferredInitMarkerPrefix(prefix) {
    if (prefix === undefined || prefix === null || prefix === '') return DEFAULT_DEFERRED_INIT_MARKER_PREFIX;
    return String(prefix).trim() || DEFAULT_DEFERRED_INIT_MARKER_PREFIX;
}

function deferredInitMarkerName(prefix, featureId, name) {
    return `${prefix}.${featureId}.${name}`;
}

function normalizeDeferredInitMatchers(matchers, label, required) {
    const values = Array.isArray(matchers) ? matchers : [];
    if (required && values.length === 0) {
        throw new Error(`summarizeDeferredInitBrowserEvidence requires at least one ${label}.`);
    }
    return values.map((matcher) => compileDeferredInitMatcher(matcher));
}

function compileDeferredInitMatcher(matcher) {
    if (typeof matcher === 'function') return matcher;
    if (matcher instanceof RegExp) return (entry) => matcher.test(deferredInitRequestUrl(entry));
    if (typeof matcher === 'string') return (entry) => deferredInitRequestUrl(entry).includes(matcher);
    throw new Error(`Unsupported deferred-init request matcher: ${String(matcher)}`);
}

function findDeferredInitMarkerTime(markerEvents, marker) {
    const event = markerEvents.find((candidate) => candidate?.name === marker);
    return deferredInitEventTime(event);
}

function deferredInitEventTime(event) {
    return finiteNumberOrNull(event?.t_ms ?? event?.time_ms ?? event?.timestamp_ms ?? event?.startTime ?? event?.start_time_ms);
}

function deferredInitRequestTime(entry) {
    return finiteNumberOrNull(
        entry?.t_ms ??
        entry?.time_ms ??
        entry?.timestamp_ms ??
        entry?.startTime ??
        entry?.start_time_ms ??
        entry?.request?.startTime ??
        entry?.request?.start_time_ms
    );
}

function deferredInitRequestUrl(entry) {
    return entry?.url || entry?.request?.url || entry?.response?.url || '';
}

function countDeferredInitRequests(entries, matchers, predicate = () => true) {
    return entries.filter((entry) => predicate(entry) && matchesAnyDeferredInitMatcher(entry, matchers)).length;
}

function sampleDeferredInitUrls(entries, matchers, predicate = () => true, limit = 20) {
    return entries
        .filter((entry) => predicate(entry) && matchesAnyDeferredInitMatcher(entry, matchers))
        .map(deferredInitRequestUrl)
        .filter(Boolean)
        .slice(0, limit);
}

function matchesAnyDeferredInitMatcher(entry, matchers) {
    return matchers.some((matcher) => matcher(entry));
}

function deferredInitAssertion(id, status, message) {
    return { id, message, status };
}

function finiteNonNegativeNumber(value) {
    const number = Number(value);
    return Number.isFinite(number) && number >= 0 ? number : null;
}

function finiteNumberOrNull(value) {
    const number = Number(value);
    return Number.isFinite(number) ? roundNumber(number) : null;
}

function stableJson(value) {
    if (Array.isArray(value)) return value.map(stableJson);
    if (!value || typeof value !== 'object') return value;
    return Object.fromEntries(
        Object.entries(value)
            .filter(([, entryValue]) => entryValue !== undefined)
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([key, entryValue]) => [key, stableJson(entryValue)])
    );
}

function compareByNameThenStart(a, b) {
    return String(a.name || '').localeCompare(String(b.name || '')) || finiteNumber(a.start_time_ms) - finiteNumber(b.start_time_ms);
}

function comparePhaseMarks(a, b) {
    return finiteNumber(a.start_time_ms) - finiteNumber(b.start_time_ms) || String(a.name || '').localeCompare(String(b.name || ''));
}

function compareNetworkEntries(a, b) {
    return finiteNumber(a.start_time_ms) - finiteNumber(b.start_time_ms) || String(a.url).localeCompare(String(b.url));
}

function compareByTypeTextLocation(a, b) {
    return String(a.type || '').localeCompare(String(b.type || '')) || String(a.text || '').localeCompare(String(b.text || ''));
}

function compareByMessage(a, b) {
    return String(a.message || '').localeCompare(String(b.message || ''));
}

function finiteNumber(value) {
    return typeof value === 'number' && Number.isFinite(value) ? roundNumber(value) : 0;
}

function finiteOrNull(value) {
    return typeof value === 'number' && Number.isFinite(value) ? roundNumber(value) : null;
}

function roundNumber(value) {
    return Math.round(value * 1000) / 1000;
}

function sanitizePhaseName(name) {
    return sanitizeMetricName(name).replace(/_/g, '-') || 'phase';
}

function summaryMetrics(diff) {
    return [
        ['Request count', diff.requests?.count],
        ['Total transfer bytes', diff.transfer?.total],
        ['Failed requests', diff.failed_requests?.count],
        ['Network-after-ready requests', diff.late_requests?.count],
        ['Long tasks', diff.long_tasks?.count],
        ['Load event ms', diff.paints?.load_event_ms],
        ['Ready ms', diff.paints?.ready_ms],
        ['Network idle ms', diff.paints?.network_idle_ms],
        ['Console errors', diff.errors?.console_errors],
        ['Page errors', diff.errors?.page_errors],
    ].filter(([, metric]) => metric);
}

function appendMetricSection(lines, title, rows) {
    const filteredRows = rows.filter(([, metric]) => metric);
    if (filteredRows.length === 0) return;
    lines.push('', `## ${title}`, '', '| Metric | Baseline | Candidate | Delta | Change | Status |', '| --- | ---: | ---: | ---: | ---: | --- |');
    for (const [name, metric, unit] of filteredRows) lines.push(formatMetricRow(name, metric, unit));
}

function appendDetailRows(lines, title, rows, maxRows, formatter) {
    if (!Array.isArray(rows) || rows.length === 0) return;
    lines.push('', `### ${title}`, '');
    lines.push(formatter.header);
    lines.push(formatter.separator);
    for (const row of rows.slice(0, maxRows)) lines.push(formatter(row));
    if (rows.length > maxRows) lines.push(`| ... | ${rows.length - maxRows} more | | | |`);
}

function formatMetricRow(name, metric, unit = '') {
    return `| ${escapeMarkdownCell(name)} | ${formatValue(metric.baseline, unit)} | ${formatValue(metric.candidate, unit)} | ${formatSignedValue(metric.delta, unit)} | ${formatPercent(metric.percent_change)} | ${metric.status} |`;
}

function formatRequestDetailRow(row) {
    return `| ${escapeMarkdownCell(row.method)} | ${escapeMarkdownCell(row.url)} | ${escapeMarkdownCell(row.resource_type)} | ${row.status ?? 'n/a'} | ${formatValue(row.duration_ms, 'ms')} |`;
}
formatRequestDetailRow.header = '| Method | URL | Type | Status | Duration |';
formatRequestDetailRow.separator = '| --- | --- | --- | ---: | ---: |';

function formatChangedRequestRow(row) {
    const fields = Object.entries(row.fields || {})
        .map(([key, value]) => `${key}: ${formatFieldChange(value)}`)
        .join('<br>');
    return `| ${escapeMarkdownCell(row.request?.method)} | ${escapeMarkdownCell(row.request?.url)} | ${escapeMarkdownCell(row.request?.resource_type)} | ${escapeMarkdownCell(fields)} | |`;
}
formatChangedRequestRow.header = '| Method | URL | Type | Changes | |';
formatChangedRequestRow.separator = '| --- | --- | --- | --- | --- |';

function formatTransferRow(row) {
    return `| ${escapeMarkdownCell(row.resource?.name)} | ${escapeMarkdownCell(row.resource?.initiator_type)} | ${formatValue(row.transfer_bytes.baseline, 'bytes')} | ${formatValue(row.transfer_bytes.candidate, 'bytes')} | ${formatSignedValue(row.transfer_bytes.delta, 'bytes')} |`;
}
formatTransferRow.header = '| Resource | Type | Baseline | Candidate | Delta |';
formatTransferRow.separator = '| --- | --- | ---: | ---: | ---: |';

function formatRequestDurationRow(row) {
    return `| ${escapeMarkdownCell(row.request?.method)} | ${escapeMarkdownCell(row.request?.url)} | ${formatValue(row.duration_ms.baseline, 'ms')} | ${formatValue(row.duration_ms.candidate, 'ms')} | ${formatSignedValue(row.duration_ms.delta, 'ms')} |`;
}
formatRequestDurationRow.header = '| Method | URL | Baseline | Candidate | Delta |';
formatRequestDurationRow.separator = '| --- | --- | ---: | ---: | ---: |';

function formatFieldChange(value) {
    if (value && typeof value === 'object' && 'delta' in value) {
        return `${formatValue(value.baseline)} -> ${formatValue(value.candidate)} (${formatSignedValue(value.delta)})`;
    }
    return `${formatValue(value?.baseline)} -> ${formatValue(value?.candidate)}`;
}

function formatValue(value, unit = '') {
    if (typeof value !== 'number' || !Number.isFinite(value)) return 'n/a';
    if (unit === 'bytes') return formatBytes(value);
    if (unit === 'ms') return `${value.toFixed(2)} ms`;
    return formatNumber(value);
}

function formatSignedValue(value, unit = '') {
    if (typeof value !== 'number' || !Number.isFinite(value)) return 'n/a';
    if (unit === 'bytes') return `${value >= 0 ? '+' : ''}${formatBytes(value)}`;
    if (unit === 'ms') return `${value >= 0 ? '+' : ''}${value.toFixed(2)} ms`;
    return formatSignedNumber(value);
}

function formatBytes(value) {
    const sign = value < 0 ? '-' : '';
    const absolute = Math.abs(value);
    if (absolute >= 1024 * 1024) return `${sign}${(absolute / (1024 * 1024)).toFixed(2)} MiB`;
    if (absolute >= 1024) return `${sign}${(absolute / 1024).toFixed(2)} KiB`;
    return `${sign}${absolute.toFixed(0)} B`;
}

function escapeMarkdownCell(value) {
    return String(value ?? '').replace(/\|/g, '\\|').replace(/\n/g, '<br>');
}

function formatNumber(value) {
    return typeof value === 'number' && Number.isFinite(value) ? value.toFixed(2) : 'n/a';
}

function formatSignedNumber(value) {
    if (typeof value !== 'number' || !Number.isFinite(value)) return 'n/a';
    return `${value >= 0 ? '+' : ''}${value.toFixed(2)}`;
}

function formatPercent(value) {
    if (typeof value !== 'number' || !Number.isFinite(value)) return 'n/a';
    return `${value >= 0 ? '+' : ''}${value.toFixed(2)}%`;
}

async function writeJson(path, value) {
    await writeFile(path, JSON.stringify(value, null, 2));
}

function sanitizeMetricName(name) {
    return String(name).trim().toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '') || 'mark';
}

function sanitizeFilePart(name) {
    return sanitizeMetricName(name).replace(/_/g, '-') || 'browser-bench';
}
