import { mkdir, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';

const DEFAULT_NETWORK_IDLE_TIMEOUT_MS = 5000;
const BROWSER_PERFORMANCE_STATE = new WeakMap();
const SECRET_HEADER_PATTERN = /^(authorization|cookie|set-cookie|proxy-authorization|x-api-key|x-auth-token|x-csrf-token|x-xsrf-token)$/i;
const SECRET_HEADER_PART_PATTERN = /(token|secret|session|cookie|credential|password|key)/i;

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

    return stableJson({
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
        phases: collectPhases(phaseMarks),
    });
}

export function compareBrowserPerformanceProfiles({ baseline, candidate }, options = {}) {
    if (!baseline || !candidate) {
        throw new Error('compareBrowserPerformanceProfiles requires baseline and candidate profiles.');
    }

    const config = normalizeComparisonOptions(options);
    const baselineMetrics = profileComparisonMetrics(baseline);
    const candidateMetrics = profileComparisonMetrics(candidate);
    const metrics = {};
    for (const key of [...new Set([...Object.keys(baselineMetrics), ...Object.keys(candidateMetrics)])].sort()) {
        const before = finiteNumber(baselineMetrics[key]);
        const after = finiteNumber(candidateMetrics[key]);
        metrics[key] = {
            baseline: before,
            candidate: after,
            delta: after - before,
            percent_change: before === 0 ? null : ((after - before) / before) * 100,
            threshold_percent: config.thresholdPercent,
            status: metricStatus(before, after, config.thresholdPercent),
        };
    }

    const phases = {};
    for (const name of [...new Set([...Object.keys(baseline.phases || {}), ...Object.keys(candidate.phases || {})])].sort()) {
        const before = finiteNumber(baseline.phases?.[name]?.duration_ms);
        const after = finiteNumber(candidate.phases?.[name]?.duration_ms);
        phases[name] = {
            baseline_duration_ms: before,
            candidate_duration_ms: after,
            delta_ms: after - before,
            percent_change: before === 0 ? null : ((after - before) / before) * 100,
            status: metricStatus(before, after, config.thresholdPercent),
        };
    }

    return stableJson({ schema_version: 1, metrics, phases });
}

export function formatBrowserPerformanceReport(comparison, options = {}) {
    const title = options.title || 'Browser performance comparison';
    const lines = [`# ${title}`, '', '| Metric | Baseline | Candidate | Delta | Change | Status |', '| --- | ---: | ---: | ---: | ---: | --- |'];
    for (const [name, metric] of Object.entries(comparison.metrics || {})) {
        lines.push(`| ${name} | ${formatNumber(metric.baseline)} | ${formatNumber(metric.candidate)} | ${formatSignedNumber(metric.delta)} | ${formatPercent(metric.percent_change)} | ${metric.status} |`);
    }

    const phases = Object.entries(comparison.phases || {});
    if (phases.length > 0) {
        lines.push('', '## Phases', '', '| Phase | Baseline | Candidate | Delta | Change | Status |', '| --- | ---: | ---: | ---: | ---: | --- |');
        for (const [name, phase] of phases) {
            lines.push(`| ${name} | ${formatNumber(phase.baseline_duration_ms)} | ${formatNumber(phase.candidate_duration_ms)} | ${formatSignedNumber(phase.delta_ms)} | ${formatPercent(phase.percent_change)} | ${phase.status} |`);
        }
    }

    return `${lines.join('\n')}\n`;
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

    const mark = async (name) => {
        const key = `${sanitizeMetricName(name)}_ms`;
        metrics[key] = performance.now() - start;
        return metrics[key];
    };

    try {
        browser = await launchBrowser(browserType, config);
        context = await browser.newContext(config.contextOptions);
        if (config.trace) {
            await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
        }
        page = await context.newPage();
        attachPageObservers(page, network, consoleMessages, requestStarts);

        await config.action({ browser, context, page, mark });

        if (config.waitForNetworkIdle) {
            await recordNetworkIdle(page, metrics, start, config.networkIdleTimeoutMs);
        }

        Object.assign(metrics, await collectNavigationMetrics(page));
        Object.assign(metrics, collectNetworkMetrics(network));

        if (config.screenshot) {
            const screenshotPath = join(config.artifactsDir, `${config.id}-screenshot.png`);
            await page.screenshot({ path: screenshotPath, fullPage: true });
            artifacts.screenshot = {
                path: screenshotPath,
                kind: 'screenshot',
                label: 'Final screenshot',
            };
        }

        const networkPath = join(config.artifactsDir, `${config.id}-network.json`);
        await writeJson(networkPath, network);
        artifacts.network = {
            path: networkPath,
            kind: 'network-log',
            label: 'Network log',
        };

        const consolePath = join(config.artifactsDir, `${config.id}-console.json`);
        await writeJson(consolePath, consoleMessages);
        artifacts.console = {
            path: consolePath,
            kind: 'console-log',
            label: 'Console log',
        };

        if (config.trace) {
            const tracePath = join(config.artifactsDir, `${config.id}-trace.zip`);
            await context.tracing.stop({ path: tracePath });
            artifacts.trace = {
                path: tracePath,
                kind: 'playwright-trace',
                label: 'Playwright trace',
            };
        }
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

function normalizeOptions(options) {
    if (!options || typeof options !== 'object' || Array.isArray(options)) {
        throw new Error('runBrowserBench requires an options object.');
    }
    if (typeof options.action !== 'function') {
        throw new Error('runBrowserBench requires an async action({ page, mark }) function.');
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
        action: options.action,
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

function normalizeBrowserPerformanceOptions(options) {
    return {
        includeHeaders: options.includeHeaders === true,
        redactHeaders: options.redactHeaders !== false,
        headerRedactionText: options.headerRedactionText || '[redacted]',
    };
}

function normalizeComparisonOptions(options) {
    return {
        thresholdPercent: Number.isFinite(options.thresholdPercent) ? options.thresholdPercent : 5,
    };
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

function collectPhases(phaseMarks) {
    const phases = {};
    for (let i = 0; i < phaseMarks.length; i++) {
        const current = phaseMarks[i];
        const next = phaseMarks[i + 1];
        phases[current.name] = {
            start_time_ms: current.start_time_ms,
            end_time_ms: next ? next.start_time_ms : null,
            duration_ms: next ? Math.max(0, roundNumber(next.start_time_ms - current.start_time_ms)) : 0,
        };
    }
    return stableJson(phases);
}

function profileComparisonMetrics(profile) {
    const summary = profile.summary || {};
    return {
        resource_count: finiteNumber(summary.resource_count),
        network_request_count: finiteNumber(summary.network_request_count),
        failed_network_request_count: finiteNumber(summary.failed_network_request_count),
        dom_content_loaded_ms: finiteNumber(summary.dom_content_loaded_ms),
        load_event_ms: finiteNumber(summary.load_event_ms),
        largest_contentful_paint_ms: finiteNumber(summary.largest_contentful_paint_ms),
        cumulative_layout_shift: finiteNumber(summary.cumulative_layout_shift),
        long_task_count: finiteNumber(summary.long_task_count),
        long_task_total_ms: finiteNumber(summary.long_task_total_ms),
    };
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
