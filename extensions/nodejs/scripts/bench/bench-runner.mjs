#!/usr/bin/env node
// Node.js bench harness — workload discovery and iteration loop. Shared
// percentile math and BenchResults envelope helpers come from Homeboy core.
//
// CONTRACT
//
// Driven by env vars set by bench-runner.sh:
//   HOMEBOY_COMPONENT_PATH       — project root
//   HOMEBOY_COMPONENT_ID         — component id (goes into envelope)
//   HOMEBOY_BENCH_ITERATIONS     — iterations per workload
//   HOMEBOY_BENCH_RESULTS_FILE   — where to write the envelope
//   HOMEBOY_BENCH_LIST_ONLY      — when 1, emit scenario inventory only
//
// Discovers `bench/**/*.bench.{ts,mjs,js}` under the project root.
// Each workload file must export a default async function. The function may
// return `{ metrics: Record<string, number> }` to report workload-owned custom
// metrics, which are averaged across measured iterations and merged beside the
// dispatcher-owned timing metrics.
//
//     // bench/cold-boot.bench.ts
//     export default async function () {
//         await launchAppAndWaitForReady();
//     }
//
// One file = one scenario. Wall-clock measured with performance.now()
// around the function call. peak_bytes captured via process.memoryUsage().rss
// max across iterations. One warmup iteration per workload is discarded
// (matches WP runner — JIT/module-cache settle time).
//
// PERCENTILE METHOD
//
// R-7 (Excel-style) linear interpolation, bit-for-bit match with the
// WP runner's pg_bench_percentile() so cross-substrate numbers compose.
//
// OUTPUT
//
// Writes the BenchResults JSON envelope (homeboy/src/core/extension/
// bench/parsing.rs::BenchResults shape) to HOMEBOY_BENCH_RESULTS_FILE.

import { mkdir, readdir, writeFile } from 'node:fs/promises';
import { resolve, relative, basename, delimiter, dirname } from 'node:path';
import { performance } from 'node:perf_hooks';
import { pathToFileURL } from 'node:url';

const helperPath = process.env.HOMEBOY_RUNTIME_BENCH_HELPER_JS;
if (!helperPath) {
    console.error('FATAL: HOMEBOY_RUNTIME_BENCH_HELPER_JS is required');
    process.exit(2);
}

const {
    homeboyBenchPercentile,
    homeboyBenchScenarioId,
    homeboyWriteBenchResults,
} = await import(pathToFileURL(helperPath).href);

const PROJECT_PATH = process.env.HOMEBOY_COMPONENT_PATH;
const COMPONENT_ID = process.env.HOMEBOY_COMPONENT_ID;
const RESULTS_FILE = process.env.HOMEBOY_BENCH_RESULTS_FILE;
const ITERATIONS = Math.max(1, Number(process.env.HOMEBOY_BENCH_ITERATIONS) || 10);
const LIST_ONLY = process.env.HOMEBOY_BENCH_LIST_ONLY === '1';
const WARMUP = 1;
const DEBUG = process.env.HOMEBOY_DEBUG === '1';

const TIMING_METRIC_KEYS = new Set([
    'mean_ms',
    'p50_ms',
    'p95_ms',
    'p99_ms',
    'min_ms',
    'max_ms',
]);

if (!PROJECT_PATH || !COMPONENT_ID || !RESULTS_FILE) {
    console.error('FATAL: missing required env vars (PROJECT_PATH/COMPONENT_ID/RESULTS_FILE)');
    process.exit(2);
}

function validateWorkloadResult(value, iterationLabel) {
    if (value === undefined) {
        return {};
    }

    if (!value || typeof value !== 'object' || Array.isArray(value)) {
        throw new Error(`${iterationLabel} returned invalid result shape (expected undefined or an object)`);
    }

    const { metrics } = value;
    if (metrics === undefined) {
        return {};
    }

    if (!metrics || typeof metrics !== 'object' || Array.isArray(metrics)) {
        throw new Error(`${iterationLabel} returned invalid metrics shape (expected an object)`);
    }

    const validated = {};
    for (const [key, metricValue] of Object.entries(metrics)) {
        if (TIMING_METRIC_KEYS.has(key)) {
            throw new Error(`${iterationLabel} returned metric "${key}", which is owned by the bench dispatcher`);
        }
        if (typeof metricValue !== 'number' || !Number.isFinite(metricValue)) {
            throw new Error(`${iterationLabel} returned metric "${key}" with non-finite numeric value`);
        }
        validated[key] = metricValue;
    }

    return validated;
}

function aggregateCustomMetrics(iterationMetrics) {
    const sums = new Map();
    const counts = new Map();

    for (const metrics of iterationMetrics) {
        for (const [key, value] of Object.entries(metrics)) {
            sums.set(key, (sums.get(key) || 0) + value);
            counts.set(key, (counts.get(key) || 0) + 1);
        }
    }

    return Object.fromEntries(
        [...sums.entries()].map(([key, sum]) => [key, sum / counts.get(key)])
    );
}

async function discoverWorkloads(dir) {
    const found = [];
    async function walk(d) {
        let entries;
        try {
            entries = await readdir(d, { withFileTypes: true });
        } catch {
            return;
        }
        for (const entry of entries) {
            const full = resolve(d, entry.name);
            if (entry.isDirectory()) {
                if (entry.name === 'node_modules' || entry.name.startsWith('.')) continue;
                await walk(full);
            } else if (/\.bench\.(ts|mjs|cjs|js)$/.test(entry.name)) {
                found.push(full);
            }
        }
    }
    await walk(dir);
    return found.sort();
}

async function runWorkload(file) {
    let mod;
    try {
        mod = await import(pathToFileURL(file).href);
    } catch (err) {
        return { error: `import failed: ${err.message}` };
    }

    const fn = mod.default;
    if (typeof fn !== 'function') {
        return { skipped: true, reason: 'no default export (expected an async function)' };
    }

    // Warmup pass — discarded.
    for (let i = 0; i < WARMUP; i++) {
        try {
            validateWorkloadResult(await fn(), `warmup iteration ${i + 1}/${WARMUP}`);
        } catch (err) {
            return { error: `warmup iteration threw: ${err.message}` };
        }
    }

    const timings = [];
    const customMetrics = [];
    let peakRss = 0;
    for (let i = 0; i < ITERATIONS; i++) {
        const start = performance.now();
        try {
            customMetrics.push(validateWorkloadResult(await fn(), `iteration ${i + 1}/${ITERATIONS}`));
        } catch (err) {
            return { error: `iteration ${i + 1}/${ITERATIONS} threw: ${err.message}` };
        }
        timings.push(performance.now() - start);
        const rss = process.memoryUsage().rss;
        if (rss > peakRss) peakRss = rss;
    }

    timings.sort((a, b) => a - b);
    return { timings, peakRss, customMetrics: aggregateCustomMetrics(customMetrics) };
}

async function main() {
    const benchDir = resolve(PROJECT_PATH, 'bench');
    const inTreeFiles = (await discoverWorkloads(benchDir)).map((file) => ({ file, source: 'in_tree' }));
    const extraFiles = (process.env.HOMEBOY_BENCH_EXTRA_WORKLOADS || '')
        .split(delimiter)
        .filter(Boolean)
        .map((file) => ({ file: resolve(PROJECT_PATH, file), source: 'rig' }));
    const files = [...inTreeFiles, ...extraFiles].sort((a, b) => a.file.localeCompare(b.file));

    if (DEBUG) console.error(`DEBUG: discovered ${files.length} workloads under ${benchDir}`);

    if (LIST_ONLY) {
        const scenarios = files.map(({ file, source }) => ({
            id: homeboyBenchScenarioId(file, /\.bench\.(ts|mjs|cjs|js)$/),
            file: source === 'in_tree' ? relative(PROJECT_PATH, file) : file,
            source,
            iterations: 0,
            default_iterations: ITERATIONS,
            tags: [],
            metrics: {},
        }));

        await mkdir(dirname(RESULTS_FILE), { recursive: true });
        await writeFile(RESULTS_FILE, JSON.stringify({
            component_id: COMPONENT_ID,
            iterations: 0,
            scenarios,
        }, null, 2));

        process.stdout.write(`Discovered ${scenarios.length} Node.js bench scenarios.\n`);
        return;
    }

    const scenarios = [];
    let hadError = false;

    for (const workload of files) {
        const { file, source } = workload;
        const id = homeboyBenchScenarioId(file, /\.bench\.(ts|mjs|cjs|js)$/);
        const rel = source === 'in_tree' ? relative(PROJECT_PATH, file) : file;
        process.stdout.write(`WORKLOAD_BEGIN: ${id} (${basename(file)})\n`);

        const result = await runWorkload(file);

        if (result.error) {
            console.error(`WORKLOAD_ERROR: ${id} — ${result.error}`);
            hadError = true;
            continue;
        }
        if (result.skipped) {
            console.error(`WORKLOAD_SKIP: ${id} — ${result.reason}`);
            continue;
        }

        const t = result.timings;
        const timingMetrics = {
            mean_ms: t.reduce((a, b) => a + b, 0) / t.length,
            p50_ms: homeboyBenchPercentile(t, 0.50),
            p95_ms: homeboyBenchPercentile(t, 0.95),
            p99_ms: homeboyBenchPercentile(t, 0.99),
            min_ms: t[0],
            max_ms: t[t.length - 1],
        };

        scenarios.push({
            id,
            file: rel,
            source,
            iterations: t.length,
            metrics: { ...timingMetrics, ...result.customMetrics },
            memory: { peak_bytes: result.peakRss },
        });

        process.stdout.write(
            `WORKLOAD_DONE:  ${id}  p50=${homeboyBenchPercentile(t, 0.50).toFixed(2)}ms  p95=${homeboyBenchPercentile(t, 0.95).toFixed(2)}ms\n`
        );
    }

    await homeboyWriteBenchResults(RESULTS_FILE, COMPONENT_ID, ITERATIONS, scenarios);

    if (DEBUG) console.error(`DEBUG: results written to ${RESULTS_FILE}`);

    if (hadError) process.exit(1);
}

main().catch((err) => {
    console.error('BENCH_FATAL:', err.stack || err.message);
    process.exit(1);
});
