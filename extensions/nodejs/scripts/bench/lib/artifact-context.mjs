import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { normalizeJsonValue, sanitizeArtifactValue } from './redaction.mjs';

export function createBenchArtifactContext(options = {}) {
    const id = sanitizeSegment(options.id || 'bench');
    const sharedState = options.sharedState || {};
    const runIdKey = options.runIdKey || `homeboyBenchArtifactRunId:${id}`;
    const runId = sharedState[runIdKey] || options.runId || createRunId(id, options);
    sharedState[runIdKey] = runId;

    const rootDir = resolve(options.artifactsDir || process.env.HOMEBOY_BENCH_ARTIFACTS_DIR || defaultArtifactsDir());
    const artifactDir = resolve(rootDir, runId);
    const artifacts = {};

    return {
        id,
        runId,
        rootDir,
        artifactDir,
        artifacts,
        artifactPath,
        artifactDescriptor,
        addArtifact,
        writeJson,
    };

    function artifactPath(name, pathOptions = {}) {
        const safeName = sanitizeSegment(name);
        const extension = normalizeExtension(pathOptions.extension || extensionForKind(pathOptions.kind));
        const prefix = pathOptions.prefix === undefined ? 'result' : pathOptions.prefix;
        const filename = [prefix, safeName].filter(Boolean).join('-') + extension;
        return join(artifactDir, filename);
    }

    function artifactDescriptor(name, file, descriptorOptions = {}) {
        return {
            path: file,
            ...(descriptorOptions.kind ? { kind: descriptorOptions.kind } : {}),
            ...(descriptorOptions.label ? { label: descriptorOptions.label } : {}),
        };
    }

    function addArtifact(name, file, descriptorOptions = {}) {
        artifacts[name] = artifactDescriptor(name, file, descriptorOptions);
        return artifacts[name];
    }

    async function writeJson(name, data, writeOptions = {}) {
        await mkdir(artifactDir, { recursive: true });
        const file = writeOptions.path || artifactPath(name, { ...writeOptions, kind: 'json', extension: 'json' });
        const value = writeOptions.redact === false
            ? normalizeJsonValue(data)
            : sanitizeArtifactValue(data, { profile: 'web', ...writeOptions.redaction });
        await writeFile(file, `${JSON.stringify(value, null, 2)}\n`);
        return addArtifact(name, file, {
            kind: writeOptions.kind || 'json',
            label: writeOptions.label,
        });
    }
}

export function createRunId(id = 'bench', options = {}) {
    const stamp = options.timestamp || new Date().toISOString().replace(/[:.]/g, '-');
    const nonce = options.nonce || randomUUID().slice(0, 8);
    return sanitizeSegment(`${id}-${stamp}-${nonce}`);
}

function defaultArtifactsDir() {
    const resultsFile = process.env.HOMEBOY_BENCH_RESULTS_FILE;
    if (resultsFile) return join(dirname(resultsFile), 'artifacts');
    return join(process.cwd(), '.homeboy-bench-artifacts');
}

function extensionForKind(kind) {
    if (kind === 'json') return 'json';
    if (kind === 'markdown') return 'md';
    if (kind === 'text') return 'txt';
    return '';
}

function normalizeExtension(extension) {
    if (!extension) return '';
    return extension.startsWith('.') ? extension : `.${extension}`;
}

function sanitizeSegment(value) {
    const segment = String(value || 'artifact')
        .trim()
        .replace(/[^A-Za-z0-9._-]+/g, '-')
        .replace(/^-+|-+$/g, '');
    return segment || 'artifact';
}
