import { appendFile, mkdir, writeFile } from 'node:fs/promises';
import { execFileSync, spawn } from 'node:child_process';
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

export async function homeboyRunBenchPhase(phase, command, args = [], options = {}) {
    if (!phase || typeof phase !== 'string') throw new Error('homeboyRunBenchPhase requires a phase label');
    if (!command || typeof command !== 'string') throw new Error('homeboyRunBenchPhase requires a command');

    const started = Date.now();
    const startedAt = new Date().toISOString();
    const child = spawn(command, args, {
        cwd: options.cwd || process.cwd(),
        env: { ...process.env, ...(options.env || {}) },
        stdio: ['ignore', 'pipe', 'pipe'],
    });
    let settled = false;
    let stdout = '';
    let stderr = '';
    const samples = [];
    const warnings = [];
    const sampleIntervalMs = Number.isFinite(options.sampleIntervalMs)
        ? options.sampleIntervalMs
        : homeboyBenchIntEnv('HOMEBOY_BENCH_PHASE_MEMORY_SAMPLE_INTERVAL_MS', 1000);

    const sampleMemory = () => {
        const sample = homeboyBenchProcessTreeSample(child.pid, phase, started);
        if (sample) samples.push(sample);
    };
    sampleMemory();
    const memoryTimer = setInterval(sampleMemory, sampleIntervalMs);
    const timeoutMs = options.timeoutMs;
    const timer = timeoutMs
        ? setTimeout(() => {
            if (settled) return;
            settled = true;
            child.kill('SIGKILL');
        }, timeoutMs)
        : undefined;

    child.stdout.on('data', (chunk) => {
        stdout += String(chunk);
        if (options.stdout === 'inherit') process.stdout.write(chunk);
    });
    child.stderr.on('data', (chunk) => {
        stderr += String(chunk);
        if (options.stderr === 'inherit') process.stderr.write(chunk);
    });

    const result = await new Promise((resolve, reject) => {
        child.on('error', reject);
        child.on('close', (code, signal) => {
            if (timer) clearTimeout(timer);
            clearInterval(memoryTimer);
            sampleMemory();
            const durationMs = Date.now() - started;
            resolve({ code, signal, stdout, stderr, elapsedMs: durationMs });
        });
    });

    await homeboyWriteBenchPhaseResource({
        phase,
        commandLabel: [command, ...args].join(' '),
        rootPid: child.pid,
        startedAt,
        finishedAt: new Date().toISOString(),
        durationMs: result.elapsedMs,
        samples,
        warnings,
    });

    return {
        ...result,
        phase,
        phaseEvents: [
            { phase, status: 'started', t_ms: 0, started_at: startedAt },
            {
                phase,
                status: result.code === 0 ? 'completed' : 'failed',
                t_ms: result.elapsedMs,
                ended_at: new Date().toISOString(),
                duration_ms: result.elapsedMs,
                message: result.code === 0 ? undefined : `${command} exited with ${result.code ?? result.signal}`,
            },
        ],
    };
}

async function homeboyWriteBenchPhaseResource({
    phase,
    commandLabel,
    rootPid,
    startedAt,
    finishedAt,
    durationMs,
    samples,
    warnings,
}) {
    const runDir = process.env.HOMEBOY_RUN_DIR;
    if (!runDir || !rootPid) return;
    const dir = `${runDir}/extension-children`;
    await mkdir(dir, { recursive: true });
    const peak = samples.reduce((current, sample) => {
        if (!current || sample.rss_bytes > current.rss_bytes) return sample;
        return current;
    }, null);
    const summary = {
        root_pid: rootPid,
        command_label: commandLabel,
        phase,
        started_at: startedAt,
        finished_at: finishedAt,
        duration_ms: durationMs,
        sampled_peak_rss_bytes: peak?.rss_bytes,
        sampled_peak_cpu_percent: samples.reduce((value, sample) => Math.max(value, sample.cpu_percent || 0), 0),
        sampled_peak_at_ms: peak?.elapsed_ms,
        sampled_peak_child_count: peak?.child_count,
        samples,
        warnings,
    };
    const safePhase = phase.replace(/[^a-z0-9]+/gi, '-').replace(/^-+|-+$/g, '') || 'phase';
    await writeFile(`${dir}/${rootPid}-${Date.now()}-${safePhase}.json`, JSON.stringify(summary, null, 2));
}

function homeboyBenchProcessTreeSample(rootPid, phase, started) {
    if (!rootPid || process.platform === 'win32') return null;
    let rows = [];
    try {
        rows = execFileSync('ps', ['-axo', 'pid=,ppid=,rss=,pcpu=,comm='], { encoding: 'utf8' })
            .trim()
            .split('\n')
            .map((line) => {
                const match = line.trim().match(/^(\d+)\s+(\d+)\s+(\d+)\s+([0-9.]+)\s+(.+)$/);
                if (!match) return null;
                return {
                    pid: Number.parseInt(match[1], 10),
                    parent_pid: Number.parseInt(match[2], 10),
                    rss_bytes: Number.parseInt(match[3], 10) * 1024,
                    cpu_percent: Number.parseFloat(match[4]),
                    command: match[5],
                };
            })
            .filter(Boolean);
    } catch {
        return null;
    }
    const children = new Map();
    for (const row of rows) {
        const list = children.get(row.parent_pid) || [];
        list.push(row.pid);
        children.set(row.parent_pid, list);
    }
    const seen = new Set([rootPid]);
    const queue = [rootPid];
    while (queue.length) {
        const pid = queue.shift();
        for (const childPid of children.get(pid) || []) {
            if (!seen.has(childPid)) {
                seen.add(childPid);
                queue.push(childPid);
            }
        }
    }
    const processes = rows.filter((row) => seen.has(row.pid)).sort((a, b) => a.pid - b.pid);
    if (!processes.length) return null;
    return {
        elapsed_ms: Date.now() - started,
        timestamp: new Date().toISOString(),
        root_pid: rootPid,
        phase,
        rss_bytes: processes.reduce((sum, row) => sum + row.rss_bytes, 0),
        cpu_percent: processes.reduce((sum, row) => sum + row.cpu_percent, 0),
        child_count: Math.max(0, processes.length - 1),
        processes,
    };
}

function homeboyBenchIntEnv(name, defaultValue) {
    const value = Number.parseInt(process.env[name] || '', 10);
    return Number.isFinite(value) && value > 0 ? value : defaultValue;
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
