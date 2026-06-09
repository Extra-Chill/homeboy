import { appendFile, mkdir, writeFile } from 'node:fs/promises';
import { basename, dirname } from 'node:path';

const homeboyBenchProgressStart = Date.now();

// R-7 percentile, the runner contract used by Homeboy BenchResults producers.
export function homeboyBenchPercentile(sortedValues, p) {
    const n = sortedValues.length;
    if (n === 0) return 0;
    if (n === 1) return sortedValues[0];
    const rank = p * (n - 1);
    const lo = Math.floor(rank);
    const hi = Math.ceil(rank);
    if (lo === hi) return sortedValues[lo];
    const frac = rank - lo;
    return sortedValues[lo] * (1 - frac) + sortedValues[hi] * frac;
}

// scenario slug helper: turn a workload basename into a stable BenchScenario id.
export function homeboyBenchScenarioId(file, extensionPattern = /\.[^.]+$/) {
    return basename(file)
        .replace(extensionPattern, '')
        .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '');
}

export function homeboyBenchSelectedScenarios(value = process.env.HOMEBOY_BENCH_SCENARIOS || '') {
    return String(value)
        .split(',')
        .map((scenario) => scenario.trim())
        .filter(Boolean);
}

export function homeboyBenchScenarioSelected(scenario, selected = homeboyBenchSelectedScenarios()) {
    if (!scenario) return false;
    if (!selected || selected.length === 0) return true;
    return selected.includes(scenario);
}

export function homeboyBenchArtifactRef(path, { kind, label } = {}) {
    if (!path) throw new Error('homeboyBenchArtifactRef requires a path');
    const ref = { path };
    if (kind) ref.kind = kind;
    if (label) ref.label = label;
    return ref;
}

export function homeboyBenchResultsEnvelope(componentId, iterations, scenarios) {
    return {
        component_id: componentId,
        iterations,
        scenarios,
    };
}

export function homeboyBenchScenarioInventoryEntry({
    id,
    file,
    source,
    defaultIterations,
    tags = [],
    metrics = {},
    metadata,
    artifacts,
}) {
    const scenario = {
        id,
        iterations: 0,
        tags,
        metrics,
    };
    if (defaultIterations !== undefined) scenario.default_iterations = defaultIterations;
    if (file) scenario.file = file;
    if (source) scenario.source = source;
    if (metadata && Object.keys(metadata).length > 0) scenario.metadata = metadata;
    if (artifacts && Object.keys(artifacts).length > 0) scenario.artifacts = artifacts;
    return scenario;
}

export function homeboyBenchScenarioInventoryEnvelope(componentId, defaultIterations, scenarios) {
    return homeboyBenchResultsEnvelope(
        componentId,
        0,
        scenarios.map((scenario) => ({
            ...scenario,
            iterations: 0,
            default_iterations: scenario.default_iterations ?? defaultIterations,
        }))
    );
}

export async function homeboyWriteBenchResults(resultsFile, componentId, iterations, scenarios) {
    await mkdir(dirname(resultsFile), { recursive: true });
    await writeFile(
        resultsFile,
        JSON.stringify(homeboyBenchResultsEnvelope(componentId, iterations, scenarios), null, 2)
    );
}

export async function homeboyWriteEmptyBenchResults(resultsFile, componentId, iterations = 0) {
    await homeboyWriteBenchResults(resultsFile, componentId, iterations, []);
}

export async function homeboyWriteBenchScenarioInventory(resultsFile, componentId, defaultIterations, scenarios) {
    await mkdir(dirname(resultsFile), { recursive: true });
    await writeFile(
        resultsFile,
        JSON.stringify(homeboyBenchScenarioInventoryEnvelope(componentId, defaultIterations, scenarios), null, 2)
    );
}

export function homeboyBenchProgress(event = {}) {
    if (!homeboyBenchProgressEnabled()) return;

    const elapsedMs = Number.isFinite(event.elapsed_ms)
        ? event.elapsed_ms
        : Date.now() - homeboyBenchProgressStart;
    const parts = [event.scenario || event.workload || 'bench', homeboyFormatBenchElapsed(elapsedMs)];

    if (event.run || event.run_id) parts.splice(1, 0, `[${event.run || event.run_id}]`);
    if (event.phase) parts.push(`phase=${event.phase}`);
    if (event.turn !== undefined) parts.push(`turn=${event.turn}`);
    if (event.tools !== undefined) parts.push(`tools=${event.tools}`);
    if (event.tool_count !== undefined && event.tools === undefined) parts.push(`tools=${event.tool_count}`);
    if (event.tool) parts.push(`tool=${homeboyBenchProgressText(event.tool)}`);
    if (event.last) parts.push(`last=${homeboyBenchProgressText(event.last)}`);
    if (event.message) parts.push(homeboyBenchProgressText(event.message));

    const line = `${parts.join(' ')}\n`;
    if ((process.env.HOMEBOY_BENCH_PROGRESS_STREAM || 'stderr') === 'stdout') {
        process.stdout.write(line);
    } else {
        process.stderr.write(line);
    }
}

export async function homeboyBenchResponsivenessPing(event = {}) {
    const file = process.env.HOMEBOY_BENCH_RESPONSIVENESS_FILE;
    if (!file) return;

    const ping = {
        at: new Date().toISOString(),
        t_ms: Date.now() - homeboyBenchProgressStart,
        ...event,
    };
    await mkdir(dirname(file), { recursive: true });
    await appendFile(file, `${JSON.stringify(ping)}\n`);
}

function homeboyBenchProgressEnabled() {
    const value = (process.env.HOMEBOY_BENCH_PROGRESS || '').trim().toLowerCase();
    return value === '1' || value === 'true' || value === 'yes' || value === 'on';
}

function homeboyFormatBenchElapsed(ms) {
    const totalSeconds = Math.max(0, Math.floor(ms / 1000));
    const minutes = Math.floor(totalSeconds / 60).toString().padStart(2, '0');
    const seconds = (totalSeconds % 60).toString().padStart(2, '0');
    return `${minutes}:${seconds}`;
}

function homeboyBenchProgressText(value) {
    return String(value).replace(/[\r\n\t]+/g, ' ').trim();
}
