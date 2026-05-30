import { mkdir, appendFile, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';
import { performance } from 'node:perf_hooks';
import { artifactPath, artifactRelativePath, writeArtifact as writeArtifactFile } from './artifacts.mjs';

const VALID_ASSERTION_STATUSES = new Set(['pass', 'fail', 'skip', 'unknown']);
const VALID_ENVELOPE_STATUSES = new Set(['pass', 'fail', 'error', 'skip', 'unknown']);

export class TraceRecorder {
    constructor(options = {}) {
        this.componentId = options.componentId || process.env.HOMEBOY_COMPONENT_ID || 'unknown';
        this.scenarioId = options.scenarioId || process.env.HOMEBOY_TRACE_SCENARIO || 'unknown';
        this.resultsFile = options.resultsFile || process.env.HOMEBOY_TRACE_RESULTS_FILE;
        this.timelinePath = options.timelinePath || artifactPath('trace.jsonl');
        this.start = performance.now();
        this.timeline = [];
        this.assertions = [];
        this.artifacts = [];
    }

    timestampMs() {
        return Math.round((performance.now() - this.start) * 1000) / 1000;
    }

    async recordEvent(source, event, data = {}) {
        const entry = {
            t_ms: this.timestampMs(),
            source: source || 'scenario',
            event,
            data: data && typeof data === 'object' && !Array.isArray(data) ? data : { value: data },
        };

        this.timeline.push(entry);
        await mkdir(dirname(this.timelinePath), { recursive: true });
        await appendFile(this.timelinePath, `${JSON.stringify(entry)}\n`);
        this.addArtifact('timeline', this.timelinePath, 'jsonl');
        return entry;
    }

    recordAssertion(id, status, message, data = undefined) {
        const normalizedStatus = VALID_ASSERTION_STATUSES.has(status) ? status : 'unknown';
        const assertion = { id, status: normalizedStatus, message };
        if (data !== undefined) assertion.data = data;
        this.assertions.push(assertion);
        return assertion;
    }

    recordCheck(id, ok, message, data = undefined) {
        return this.recordAssertion(id, ok ? 'pass' : 'fail', message, data);
    }

    addArtifact(label, path, kind = undefined) {
        const artifact = { label, path: artifactRelativePath(path) };
        if (kind) artifact.kind = kind;

        if (!this.artifacts.some((existing) => existing.label === artifact.label && existing.path === artifact.path)) {
            this.artifacts.push(artifact);
        }

        return artifact;
    }

    async writeArtifact(label, name, content, kind = undefined) {
        const artifact = await writeArtifactFile(name, content);
        return this.addArtifact(label, artifact.path, kind);
    }

    async writeTraceResults(options = {}) {
        if (!this.resultsFile) {
            throw new Error('HOMEBOY_TRACE_RESULTS_FILE is required to write trace results');
        }

        const status = VALID_ENVELOPE_STATUSES.has(options.status) ? options.status : deriveStatus(this.assertions);
        const envelope = {
            component_id: this.componentId,
            scenario_id: this.scenarioId,
            status,
            summary: options.summary || defaultSummary(status),
            timeline: this.timeline,
            assertions: this.assertions,
            artifacts: this.artifacts,
        };

        if (options.failure) envelope.failure = options.failure;

        await mkdir(dirname(this.resultsFile), { recursive: true });
        await writeFile(this.resultsFile, JSON.stringify(envelope, null, 2));
        return envelope;
    }
}

export function createTraceRecorder(options = {}) {
    return new TraceRecorder(options);
}

function deriveStatus(assertions) {
    if (assertions.some((assertion) => assertion.status === 'fail')) return 'fail';
    if (assertions.length > 0 && assertions.every((assertion) => assertion.status === 'skip')) return 'skip';
    if (assertions.some((assertion) => assertion.status === 'unknown')) return 'unknown';
    return 'pass';
}

function defaultSummary(status) {
    switch (status) {
        case 'pass': return 'Trace passed';
        case 'fail': return 'Trace failed';
        case 'skip': return 'Trace skipped';
        case 'error': return 'Trace errored';
        default: return 'Trace completed with unknown evidence';
    }
}
