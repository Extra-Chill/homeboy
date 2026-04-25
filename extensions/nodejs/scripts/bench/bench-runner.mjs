#!/usr/bin/env node
// Node.js bench harness — workload discovery, iteration loop, percentile
// math, and BenchResults envelope serialization.
//
// CONTRACT
//
// Driven by env vars set by bench-runner.sh:
//   HOMEBOY_COMPONENT_PATH       — project root
//   HOMEBOY_COMPONENT_ID         — component id (goes into envelope)
//   HOMEBOY_BENCH_ITERATIONS     — iterations per workload
//   HOMEBOY_BENCH_RESULTS_FILE   — where to write the envelope
//
// Discovers `bench/**/*.bench.{ts,mjs,js}` under the project root.
// Each workload file must export a default async function:
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

import { readdir, writeFile, mkdir } from 'node:fs/promises';
import { resolve, relative, basename, dirname } from 'node:path';
import { performance } from 'node:perf_hooks';
import { pathToFileURL } from 'node:url';

const PROJECT_PATH = process.env.HOMEBOY_COMPONENT_PATH;
const COMPONENT_ID = process.env.HOMEBOY_COMPONENT_ID;
const RESULTS_FILE = process.env.HOMEBOY_BENCH_RESULTS_FILE;
const ITERATIONS = Math.max(1, Number(process.env.HOMEBOY_BENCH_ITERATIONS) || 10);
const WARMUP = 1;
const DEBUG = process.env.HOMEBOY_DEBUG === '1';

if (!PROJECT_PATH || !COMPONENT_ID || !RESULTS_FILE) {
    console.error('FATAL: missing required env vars (PROJECT_PATH/COMPONENT_ID/RESULTS_FILE)');
    process.exit(2);
}

// R-7 percentile — matches pg_bench_percentile() in the WP runner.
function percentile(sortedMs, p) {
    const n = sortedMs.length;
    if (n === 0) return 0;
    if (n === 1) return sortedMs[0];
    const rank = p * (n - 1);
    const lo = Math.floor(rank);
    const hi = Math.ceil(rank);
    if (lo === hi) return sortedMs[lo];
    const frac = rank - lo;
    return sortedMs[lo] * (1 - frac) + sortedMs[hi] * frac;
}

// "ColdBoot.bench.ts" → "cold-boot". Matches WP runner's slug rule.
function scenarioId(file) {
    return basename(file)
        .replace(/\.bench\.(ts|mjs|cjs|js)$/, '')
        .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '');
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
            await fn();
        } catch (err) {
            return { error: `warmup iteration threw: ${err.message}` };
        }
    }

    const timings = [];
    let peakRss = 0;
    for (let i = 0; i < ITERATIONS; i++) {
        const start = performance.now();
        try {
            await fn();
        } catch (err) {
            return { error: `iteration ${i + 1}/${ITERATIONS} threw: ${err.message}` };
        }
        timings.push(performance.now() - start);
        const rss = process.memoryUsage().rss;
        if (rss > peakRss) peakRss = rss;
    }

    timings.sort((a, b) => a - b);
    return { timings, peakRss };
}

async function main() {
    const benchDir = resolve(PROJECT_PATH, 'bench');
    const files = await discoverWorkloads(benchDir);

    if (DEBUG) console.error(`DEBUG: discovered ${files.length} workloads under ${benchDir}`);

    const scenarios = [];
    let hadError = false;

    for (const file of files) {
        const id = scenarioId(file);
        const rel = relative(PROJECT_PATH, file);
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
        scenarios.push({
            id,
            file: rel,
            iterations: t.length,
            metrics: {
                mean_ms: t.reduce((a, b) => a + b, 0) / t.length,
                p50_ms: percentile(t, 0.50),
                p95_ms: percentile(t, 0.95),
                p99_ms: percentile(t, 0.99),
                min_ms: t[0],
                max_ms: t[t.length - 1],
            },
            memory: { peak_bytes: result.peakRss },
        });

        process.stdout.write(
            `WORKLOAD_DONE:  ${id}  p50=${percentile(t, 0.50).toFixed(2)}ms  p95=${percentile(t, 0.95).toFixed(2)}ms\n`
        );
    }

    const envelope = {
        component_id: COMPONENT_ID,
        iterations: ITERATIONS,
        scenarios,
    };

    await mkdir(dirname(RESULTS_FILE), { recursive: true });
    await writeFile(RESULTS_FILE, JSON.stringify(envelope, null, 2));

    if (DEBUG) console.error(`DEBUG: results written to ${RESULTS_FILE}`);

    if (hadError) process.exit(1);
}

main().catch((err) => {
    console.error('BENCH_FATAL:', err.stack || err.message);
    process.exit(1);
});
