import { mkdir, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';

const DEFAULT_NETWORK_IDLE_TIMEOUT_MS = 5000;

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

async function writeJson(path, value) {
    await writeFile(path, JSON.stringify(value, null, 2));
}

function sanitizeMetricName(name) {
    return String(name).trim().toLowerCase().replace(/[^a-z0-9]+/g, '_').replace(/^_+|_+$/g, '') || 'mark';
}

function sanitizeFilePart(name) {
    return sanitizeMetricName(name).replace(/_/g, '-') || 'browser-bench';
}
