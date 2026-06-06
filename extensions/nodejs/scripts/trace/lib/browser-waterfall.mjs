import {
    collectBrowserPhases,
    normalizeBrowserNetworkRequest,
    normalizeBrowserPerformanceProfile,
    normalizeBrowserProfileTimings,
    normalizeBrowserTiming,
    stableJson,
} from '../../../../scripts/lib/browser-result-shapes.mjs';

export async function collectBrowserWaterfall(page, options = {}) {
    if (!page || typeof page.evaluate !== 'function') {
        throw new Error('collectBrowserWaterfall requires a Playwright/Puppeteer-like page with evaluate().');
    }

    const snapshot = await page.evaluate(browserWaterfallSnapshot);
    return normalizeBrowserWaterfall(snapshot, options);
}

export function createBrowserWaterfallCollector(options = {}) {
    return {
        async collect(page = options.page, overrides = {}) {
            return collectBrowserWaterfall(page, { ...options, ...overrides });
        },
        normalize(snapshot, overrides = {}) {
            return normalizeBrowserWaterfall(snapshot, { ...options, ...overrides });
        },
    };
}

export function normalizeBrowserWaterfall(snapshot, options = {}) {
    const source = snapshot && typeof snapshot === 'object' && !Array.isArray(snapshot) ? snapshot : {};
    const resources = Array.isArray(source.resources) ? source.resources : [];
    const network = Array.isArray(source.network) ? source.network : [];
    const phaseMarks = Array.isArray(source.phase_marks) ? source.phase_marks : [];
    const profile = normalizeBrowserPerformanceProfile({
        ...source,
        resources,
        network,
        phase_marks: phaseMarks,
        phases: source.phases || collectBrowserPhases(phaseMarks),
    });
    const rows = normalizeBrowserProfileTimings(profile, options)
        .map((entry) => normalizeBrowserTiming(entry, options))
        .filter(Boolean)
        .map((entry) => normalizeWaterfallRow(entry))
        .sort(compareRows);

    return stableJson({
        schema: 'homeboy/browser-waterfall/v1',
        page_url: source.page_url || source.url || '',
        summary: summarizeWaterfall(rows, profile),
        rows,
        profile,
    });
}

export function browserWaterfallSnapshot() {
    const serializeTiming = (entry) => {
        const json = typeof entry.toJSON === 'function' ? entry.toJSON() : {};
        return {
            ...json,
            name: entry.name,
            initiatorType: entry.initiatorType,
            startTime: entry.startTime,
            duration: entry.duration,
            fetchStart: entry.fetchStart,
            requestStart: entry.requestStart,
            responseStart: entry.responseStart,
            responseEnd: entry.responseEnd,
            transferSize: entry.transferSize,
            encodedBodySize: entry.encodedBodySize,
            decodedBodySize: entry.decodedBodySize,
        };
    };
    const navigation = performance.getEntriesByType('navigation').map(serializeTiming);
    const resources = performance.getEntriesByType('resource').map(serializeTiming);
    const paints = performance.getEntriesByType('paint').map(serializeTiming);

    return {
        page_url: window.location.href,
        navigation,
        resources,
        paints,
        phase_marks: [
            { name: 'navigation_start', start_time_ms: 0 },
            ...paints.map((entry) => ({ name: entry.name, start_time_ms: entry.startTime })),
        ],
        summary: {
            navigation_count: navigation.length,
            resource_count: resources.length,
            paint_count: paints.length,
        },
    };
}

function normalizeWaterfallRow(entry) {
    const request = normalizeBrowserNetworkRequest(entry);
    return stableJson({
        url: entry.url,
        normalized_url: entry.normalizedUrl,
        method: request.method || entry.method,
        status: request.status ?? entry.status,
        failed: Boolean(entry.failed),
        resource_type: request.resource_type || entry.initiatorType,
        initiator_type: entry.initiatorType,
        phase: entry.phase,
        start_time_ms: entry.startTime,
        ttfb_ms: entry.ttfbMs,
        duration_ms: entry.durationMs,
        transfer_size_bytes: finiteOrNull(entry.raw?.transferSize ?? entry.transferSize),
        encoded_body_size_bytes: finiteOrNull(entry.raw?.encodedBodySize ?? entry.encodedBodySize),
        decoded_body_size_bytes: finiteOrNull(entry.raw?.decodedBodySize ?? entry.decodedBodySize),
    });
}

function summarizeWaterfall(rows, profile) {
    const failedRows = rows.filter((row) => row.failed || (typeof row.status === 'number' && row.status >= 400));
    const totalTransferSize = rows.reduce((total, row) => total + (row.transfer_size_bytes || 0), 0);
    const slowest = rows.reduce((current, row) => {
        if (!current || (row.duration_ms || 0) > (current.duration_ms || 0)) return row;
        return current;
    }, null);

    return stableJson({
        request_count: rows.length,
        failed_request_count: failedRows.length,
        transfer_size_bytes: totalTransferSize,
        slowest_request_ms: slowest?.duration_ms ?? null,
        slowest_request_url: slowest?.normalized_url || slowest?.url || null,
        resource_count: Array.isArray(profile.resources) ? profile.resources.length : rows.length,
    });
}

function compareRows(a, b) {
    return (a.start_time_ms ?? 0) - (b.start_time_ms ?? 0) || String(a.url).localeCompare(String(b.url));
}

function finiteOrNull(value) {
    return typeof value === 'number' && Number.isFinite(value) ? Math.round(value * 1000) / 1000 : null;
}
