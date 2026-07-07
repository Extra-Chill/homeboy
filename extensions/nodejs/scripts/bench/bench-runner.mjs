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
//   HOMEBOY_BENCH_WARMUP_ITERATIONS — discarded warmups per workload (default 1)
//   HOMEBOY_BENCH_RESULTS_FILE   — where to write the envelope
//   HOMEBOY_BENCH_LIST_ONLY      — when 1, emit scenario inventory only
//   HOMEBOY_BENCH_ARGS_JSON      — optional JSON array of passthrough args for workloads
//   HOMEBOY_BENCH_PUBLIC_ARTIFACT_BASE_URL — optional public URL base for artifacts
//
// Discovers `bench/**/*.bench.{ts,mjs,js}` under the project root.
// Each workload file must export a default async function. The runner calls it
// as `fn(context)`, where context includes `{ args, artifactsDir,
// publicArtifactBaseUrl, env }`. `args` is parsed from HOMEBOY_BENCH_ARGS_JSON;
// artifactsDir and publicArtifactBaseUrl are present when their env-backed
// values are available; env is process.env. The function may return `{ metrics,
// artifacts, metadata }` to report workload-owned custom metrics, artifacts,
// and scenario labels. Metrics are averaged across measured iterations and
// merged beside the dispatcher-owned timing metrics; artifacts and metadata are
// preserved under the scenario in the results envelope.
//
//     // bench/cold-boot.bench.ts
//     export default async function (context) {
//         console.log(context.args);
//         await launchAppAndWaitForReady();
//     }
//
// One file = one scenario. Wall-clock measured with performance.now()
// around the function call. peak_bytes captured via process.memoryUsage().rss
// max across iterations. One warmup iteration per workload is discarded by
// default (matches WP runner — JIT/module-cache settle time), but expensive
// workloads may set HOMEBOY_BENCH_WARMUP_ITERATIONS=0 to disable it.
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
import { resolve, relative, basename, delimiter, dirname, isAbsolute } from 'node:path';
import { performance } from 'node:perf_hooks';
import { pathToFileURL } from 'node:url';
import {
    normalizeBenchArtifact,
    normalizeBrowserBenchWorkloadResult,
} from '../lib/browser-result-shapes.mjs';

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
const WARMUP = LIST_ONLY ? 0 : parseWarmupIterations(process.env.HOMEBOY_BENCH_WARMUP_ITERATIONS);
const DEBUG = process.env.HOMEBOY_DEBUG === '1';
const ARTIFACTS_DIR = process.env.HOMEBOY_BENCH_ARTIFACTS_DIR
    ? resolve(process.env.HOMEBOY_BENCH_ARTIFACTS_DIR)
    : undefined;
const PUBLIC_ARTIFACT_BASE_URL = publicArtifactBaseUrl();
const WORKLOAD_ARGS = parseBenchArgs(process.env.HOMEBOY_BENCH_ARGS_JSON);
const WORKLOAD_CONTEXT = {
    args: WORKLOAD_ARGS,
    artifactsDir: ARTIFACTS_DIR,
    publicArtifactBaseUrl: PUBLIC_ARTIFACT_BASE_URL || undefined,
    env: process.env,
};

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

function parseWarmupIterations(value) {
    if (value === undefined) {
        return 1;
    }

    if (!/^-?\d+$/.test(value)) {
        console.error(`FATAL: HOMEBOY_BENCH_WARMUP_ITERATIONS must be an integer, got "${value}"`);
        process.exit(2);
    }

    return Math.max(0, Number(value));
}

function parseBenchArgs(value) {
    if (value === undefined || value === '') {
        return [];
    }

    let parsed;
    try {
        parsed = JSON.parse(value);
    } catch (err) {
        console.error(`FATAL: HOMEBOY_BENCH_ARGS_JSON must be a JSON array of strings: ${err.message}`);
        process.exit(2);
    }

    if (!Array.isArray(parsed) || parsed.some((item) => typeof item !== 'string')) {
        console.error('FATAL: HOMEBOY_BENCH_ARGS_JSON must be a JSON array of strings');
        process.exit(2);
    }

    return parsed;
}

function validateWorkloadResult(value, iterationLabel) {
    if (value === undefined) {
        return { metrics: {}, artifacts: {}, metadata: {} };
    }

    if (!value || typeof value !== 'object' || Array.isArray(value)) {
        throw new Error(`${iterationLabel} returned invalid result shape (expected undefined or an object)`);
    }

    const normalizedResult = normalizeBrowserBenchWorkloadResult(value);
    const browserNormalized = normalizedResult !== value;
    const { metrics, artifacts, metadata } = normalizedResult;

    if (metrics !== undefined && (!metrics || typeof metrics !== 'object' || Array.isArray(metrics))) {
        throw new Error(`${iterationLabel} returned invalid metrics shape (expected an object)`);
    }

    const validatedMetrics = {};
    for (const [key, metricValue] of Object.entries(metrics || {})) {
        if (TIMING_METRIC_KEYS.has(key)) {
            throw new Error(`${iterationLabel} returned metric "${key}", which is owned by the bench dispatcher`);
        }
        if (typeof metricValue !== 'number' || !Number.isFinite(metricValue)) {
            throw new Error(`${iterationLabel} returned metric "${key}" with non-finite numeric value`);
        }
        validatedMetrics[key] = metricValue;
    }

    return {
        metrics: validatedMetrics,
        artifacts: validateWorkloadArtifacts(artifacts, iterationLabel, { browserNormalized }),
        metadata: validateWorkloadMetadata(metadata, iterationLabel),
    };
}

function validateWorkloadMetadata(metadata, iterationLabel) {
    if (metadata === undefined) {
        return {};
    }

    if (!metadata || typeof metadata !== 'object' || Array.isArray(metadata)) {
        throw new Error(`${iterationLabel} returned invalid metadata shape (expected an object)`);
    }

    try {
        return JSON.parse(JSON.stringify(metadata));
    } catch (err) {
        throw new Error(`${iterationLabel} returned metadata that is not JSON-serializable: ${err.message}`);
    }
}

function validateWorkloadArtifacts(artifacts, iterationLabel, options = {}) {
    if (artifacts === undefined) {
        return {};
    }

    if (!artifacts || typeof artifacts !== 'object' || Array.isArray(artifacts)) {
        throw new Error(`${iterationLabel} returned invalid artifacts shape (expected an object)`);
    }

    const validated = {};
    for (const [key, artifact] of Object.entries(artifacts)) {
        if (!options.browserNormalized) {
            validated[key] = validateGenericWorkloadArtifact(key, artifact, iterationLabel);
            continue;
        }

        const normalized = normalizeBenchArtifact(artifact);
        if (Object.keys(normalized).length === 0) {
            throw new Error(`${iterationLabel} returned artifact "${key}" with invalid shape (expected string path or descriptor object)`);
        }
        for (const [field, fieldValue] of Object.entries(normalized)) {
            normalized[field] = validateJsonField(fieldValue, `${iterationLabel} returned artifact "${key}" field "${field}"`);
        }
        if (normalized.path) {
            const publicUrl = normalized.url || resolvePublicArtifactUrl(normalized.path);
            if (publicUrl) normalized.url = publicUrl;
        }
        validated[key] = normalized;
    }

    return validated;
}

function validateGenericWorkloadArtifact(key, artifact, iterationLabel) {
    if (typeof artifact === 'string') {
        if (artifact.length === 0) {
            throw new Error(`${iterationLabel} returned artifact "${key}" with empty path`);
        }
        return { path: artifact };
    }

    if (!artifact || typeof artifact !== 'object' || Array.isArray(artifact)) {
        throw new Error(`${iterationLabel} returned artifact "${key}" with invalid shape (expected string path or object)`);
    }
    if (typeof artifact.path !== 'string' || artifact.path.length === 0) {
        throw new Error(`${iterationLabel} returned artifact "${key}" without a non-empty string path`);
    }
    if (artifact.kind !== undefined && typeof artifact.kind !== 'string') {
        throw new Error(`${iterationLabel} returned artifact "${key}" with non-string kind`);
    }
    if (artifact.label !== undefined && typeof artifact.label !== 'string') {
        throw new Error(`${iterationLabel} returned artifact "${key}" with non-string label`);
    }
    if (artifact.url !== undefined && typeof artifact.url !== 'string') {
        throw new Error(`${iterationLabel} returned artifact "${key}" with non-string url`);
    }

    const normalized = { path: artifact.path };
    if (artifact.kind !== undefined) normalized.kind = artifact.kind;
    if (artifact.label !== undefined) normalized.label = artifact.label;
    for (const [field, value] of Object.entries(artifact)) {
        if (field === 'path' || field === 'kind' || field === 'label' || field === 'url') {
            continue;
        }
        normalized[field] = validateJsonField(value, `${iterationLabel} returned artifact "${key}" field "${field}"`);
    }
    const publicUrl = artifact.url || resolvePublicArtifactUrl(artifact.path);
    if (publicUrl) normalized.url = publicUrl;
    return normalized;
}

function validateJsonField(value, label) {
    try {
        return JSON.parse(JSON.stringify(value));
    } catch (err) {
        throw new Error(`${label} that is not JSON-serializable: ${err.message}`);
    }
}

function publicArtifactBaseUrl() {
    return (
        process.env.HOMEBOY_BENCH_PUBLIC_ARTIFACT_BASE_URL
        || process.env.HOMEBOY_PUBLIC_ARTIFACT_BASE_URL
        || process.env.HOMEBOY_ARTIFACT_PUBLIC_BASE_URL
        || ''
    ).trim();
}

function resolvePublicArtifactUrl(artifactPath) {
    if (/^https?:\/\//i.test(artifactPath)) {
        return artifactPath;
    }
    if (!PUBLIC_ARTIFACT_BASE_URL) {
        return undefined;
    }

    const relativePath = publicArtifactRelativePath(artifactPath);
    if (!relativePath) {
        return undefined;
    }

    const base = PUBLIC_ARTIFACT_BASE_URL.endsWith('/') ? PUBLIC_ARTIFACT_BASE_URL : `${PUBLIC_ARTIFACT_BASE_URL}/`;
    return new URL(relativePath.split('/').map(encodeURIComponent).join('/'), base).toString();
}

function publicArtifactRelativePath(artifactPath) {
    if (!isAbsolute(artifactPath)) {
        return artifactPath.replace(/^\.\//, '').replace(/^\/+/, '');
    }
    if (!ARTIFACTS_DIR) {
        return undefined;
    }

    const resolvedArtifactPath = resolve(artifactPath);
    const relativePath = relative(ARTIFACTS_DIR, resolvedArtifactPath);
    if (relativePath.startsWith('..') || isAbsolute(relativePath)) {
        return undefined;
    }
    return relativePath.split(/[/\\]+/).join('/');
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

function aggregateArtifacts(iterationArtifacts) {
    return Object.assign({}, ...iterationArtifacts);
}

function aggregateMetadata(iterationMetadata) {
    return Object.assign({}, ...iterationMetadata);
}

function resultCountMetrics(metadata) {
    const resultCounts = metadata.result_counts;
    if (!resultCounts || typeof resultCounts !== 'object' || Array.isArray(resultCounts)) {
        return {};
    }

    const metrics = {};
    for (const [status, count] of Object.entries(resultCounts)) {
        if (typeof count === 'number' && Number.isFinite(count)) {
            metrics[`${status}_count`] = count;
        }
    }
    return metrics;
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
            validateWorkloadResult(await fn(WORKLOAD_CONTEXT), `warmup iteration ${i + 1}/${WARMUP}`);
        } catch (err) {
            return { error: `warmup iteration threw: ${err.message}` };
        }
    }

    const timings = [];
    const customMetrics = [];
    const customArtifacts = [];
    const customMetadata = [];
    let peakRss = 0;
    for (let i = 0; i < ITERATIONS; i++) {
        const start = performance.now();
        try {
            const workloadResult = validateWorkloadResult(await fn(WORKLOAD_CONTEXT), `iteration ${i + 1}/${ITERATIONS}`);
            customMetrics.push(workloadResult.metrics);
            customArtifacts.push(workloadResult.artifacts);
            customMetadata.push(workloadResult.metadata);
        } catch (err) {
            return { error: `iteration ${i + 1}/${ITERATIONS} threw: ${err.message}` };
        }
        timings.push(performance.now() - start);
        const rss = process.memoryUsage().rss;
        if (rss > peakRss) peakRss = rss;
    }

    timings.sort((a, b) => a - b);
    return {
        timings,
        peakRss,
        customMetrics: aggregateCustomMetrics(customMetrics),
        customArtifacts: aggregateArtifacts(customArtifacts),
        customMetadata: aggregateMetadata(customMetadata),
    };
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

        const metadata = result.customMetadata;
        const scenario = {
            id,
            file: rel,
            source,
            iterations: t.length,
            metrics: { ...timingMetrics, ...resultCountMetrics(metadata), ...result.customMetrics },
            memory: { peak_bytes: result.peakRss },
        };
        if (Object.keys(result.customArtifacts).length > 0) {
            scenario.artifacts = result.customArtifacts;
        }
        if (Object.keys(metadata).length > 0) {
            scenario.metadata = metadata;
        }
        scenarios.push(scenario);

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
