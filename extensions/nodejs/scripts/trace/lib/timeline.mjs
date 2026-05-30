import { mkdir, appendFile, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';
import { performance } from 'node:perf_hooks';
import { artifactPath, artifactRelativePath, writeArtifact as writeArtifactFile } from './artifacts.mjs';
import {
    normalizeBrowserArtifact,
    normalizeTraceAssertion,
    normalizeTraceEnvelope,
    normalizeTraceEvent,
} from '../../../../scripts/lib/browser-result-shapes.mjs';

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
        const entry = normalizeTraceEvent(source || 'scenario', event, data, this.timestampMs());

        this.timeline.push(entry);
        await mkdir(dirname(this.timelinePath), { recursive: true });
        await appendFile(this.timelinePath, `${JSON.stringify(entry)}\n`);
        this.addArtifact('timeline', this.timelinePath, 'jsonl');
        return entry;
    }

    recordAssertion(id, status, message, data = undefined) {
        const assertion = normalizeTraceAssertion(id, status, message, data);
        this.assertions.push(assertion);
        return assertion;
    }

    recordCheck(id, ok, message, data = undefined) {
        return this.recordAssertion(id, ok ? 'pass' : 'fail', message, data);
    }

    addArtifact(label, path, kind = undefined) {
        const artifact = normalizeBrowserArtifact({ label, path: artifactRelativePath(path), kind });

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

        const status = options.status || deriveStatus(this.assertions);
        const envelope = normalizeTraceEnvelope({
            component_id: this.componentId,
            scenario_id: this.scenarioId,
            status,
            summary: options.summary || defaultSummary(status),
            timeline: this.timeline,
            assertions: this.assertions,
            artifacts: this.artifacts,
            failure: options.failure,
        });

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
