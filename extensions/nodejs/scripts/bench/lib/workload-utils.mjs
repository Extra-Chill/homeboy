import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdir, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { performance } from 'node:perf_hooks';

import { createBenchArtifactContext } from './artifact-context.mjs';
import { normalizeJsonValue, redactText, sanitizeArtifactValue } from './redaction.mjs';

export { createRunId } from './artifact-context.mjs';
export { redactText, sanitizeArtifactFile, sanitizeArtifactValue, sanitizeUrl } from './redaction.mjs';

const DEFAULT_SETTINGS_PREFIX = 'HOMEBOY_SETTINGS_';

export function settings(env = process.env) {
    try {
        const parsed = JSON.parse(env.HOMEBOY_SETTINGS_JSON || '{}');
        return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
    } catch {
        return {};
    }
}

export function setting(key, fallback = '', options = {}) {
    const env = options.env || process.env;
    const resolved = settings(env);
    if (Object.hasOwn(resolved, key) && resolved[key] !== undefined && resolved[key] !== null) {
        return String(resolved[key]);
    }

    const envKey = `${options.prefix || DEFAULT_SETTINGS_PREFIX}${String(key).toUpperCase()}`;
    return env[envKey] !== undefined ? env[envKey] : fallback;
}

export function metric(value, fallback = 0) {
    const number = Number(value ?? fallback);
    return Number.isFinite(number) ? number : fallback;
}

export function artifactDir(name, options = {}) {
    return path.join(options.sharedState || process.env.HOMEBOY_BENCH_SHARED_STATE || os.tmpdir(), name);
}

export function runId(name = 'workload', options = {}) {
    const namespace = options.namespace || setting(options.namespaceSetting || 'namespace', path.basename(process.env.HOMEBOY_COMPONENT_PATH || 'node'));
    const nonce = options.nonce || randomUUID().slice(0, 8);
    const timestamp = options.timestamp || Date.now();
    return sanitizeSegment(`${namespace}-${name}-${process.pid}-${timestamp}-${nonce}`);
}

export function safeResult(result, options = {}) {
    if (!result) return result;
    return {
        ...result,
        ...(result.stdout !== undefined ? { stdout: redactText(String(result.stdout), options.redaction) } : {}),
        ...(result.stderr !== undefined ? { stderr: redactText(String(result.stderr), options.redaction) } : {}),
    };
}

export function runCommand(command, args = [], options = {}) {
    return new Promise((resolve, reject) => {
        const started = performance.now();
        let settled = false;
        let stdout = '';
        let stderr = '';
        const child = spawn(command, args, {
            cwd: options.cwd || process.env.HOMEBOY_COMPONENT_PATH || process.cwd(),
            env: { ...process.env, ...(options.env || {}) },
            shell: false,
        });

        const commandText = redactText([command, ...args].join(' '), options.redaction);
        const timer = options.timeoutMs
            ? setTimeout(() => {
                if (settled) return;
                settled = true;
                child.kill('SIGKILL');
                reject(new Error(`${commandText} timed out after ${options.timeoutMs}ms; stdout=${redactText(stdout, options.redaction).slice(-1200)}; stderr=${redactText(stderr, options.redaction).slice(-1200)}`));
            }, options.timeoutMs)
            : undefined;

        child.stdout?.on('data', (chunk) => {
            stdout += String(chunk);
        });
        child.stderr?.on('data', (chunk) => {
            stderr += String(chunk);
        });
        child.on('error', (error) => {
            if (timer) clearTimeout(timer);
            reject(error);
        });
        child.on('close', (code, signal) => {
            if (settled) return;
            settled = true;
            if (timer) clearTimeout(timer);

            const result = {
                code,
                signal,
                stdout,
                stderr,
                elapsedMs: performance.now() - started,
            };
            const returned = options.redact === false ? result : safeResult(result, options);

            if (code !== 0 && options.allowFailure !== true) {
                reject(new Error(`${commandText} exited ${code}; stderr=${redactText(stderr, options.redaction).slice(0, 2000)}`));
                return;
            }

            resolve(returned);
        });
    });
}

export function runNode(args, options = {}) {
    return runCommand(options.nodeBinary || process.env.HOMEBOY_NODE_BINARY || 'node', args, options);
}

export async function writeJson(file, data, options = {}) {
    await mkdir(path.dirname(file), { recursive: true });
    const value = options.redact === false
        ? normalizeJsonValue(data)
        : sanitizeArtifactValue(data, options.redaction);
    await writeFile(file, `${JSON.stringify(value, null, 2)}\n`);
    return file;
}

export async function writeText(file, data, options = {}) {
    await mkdir(path.dirname(file), { recursive: true });
    const value = options.redact === false ? String(data ?? '') : redactText(String(data ?? ''), options.redaction);
    await writeFile(file, value);
    return file;
}

export function createArtifactContext(options = {}) {
    return createBenchArtifactContext(options);
}

export function percentile(values, pct) {
    const sorted = values.filter((value) => Number.isFinite(value)).sort((a, b) => a - b);
    if (sorted.length === 0) return 0;
    if (sorted.length === 1) return sorted[0];

    const rank = (pct / 100) * (sorted.length - 1);
    const lower = Math.floor(rank);
    const upper = Math.ceil(rank);
    if (lower === upper) return sorted[lower];

    const weight = rank - lower;
    return sorted[lower] * (1 - weight) + sorted[upper] * weight;
}

function sanitizeSegment(value) {
    const segment = String(value || 'workload')
        .trim()
        .replace(/[^A-Za-z0-9._-]+/g, '-')
        .replace(/^-+|-+$/g, '');
    return segment || 'workload';
}
