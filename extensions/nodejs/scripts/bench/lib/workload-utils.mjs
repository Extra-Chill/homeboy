import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { performance } from 'node:perf_hooks';

import { createBenchArtifactContext } from './artifact-context.mjs';
import { normalizeJsonValue, redactText, sanitizeArtifactValue } from './redaction.mjs';

export { createRunId } from './artifact-context.mjs';
export { redactText, sanitizeArtifactFile, sanitizeArtifactValue, sanitizeUrl } from './redaction.mjs';

const DEFAULT_SETTINGS_PREFIX = 'HOMEBOY_SETTINGS_';
const RUNNER_PROGRESS_LINE_PREFIX = 'HOMEBOY_RUNNER_PROGRESS ';
const RUNNER_PROGRESS_SCHEMA = 'homeboy/runner-progress/v1';
const RUNNER_PROGRESS_FIELDS = new Set(['schema', 'phase', 'current_item', 'completed', 'total', 'metadata']);

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

export function settingInt(key, fallback = 0, options = {}) {
    const value = settingValue(key, undefined, options);
    const parsed = typeof value === 'number'
        ? value
        : (/^[+-]?\d+$/.test(String(value ?? '').trim()) ? Number(String(value).trim()) : Number.NaN);
    if (!Number.isInteger(parsed)) return fallback;
    if (options.min !== undefined && parsed < options.min) return fallback;
    if (options.max !== undefined && parsed > options.max) return fallback;
    return parsed;
}

export function settingBool(key, fallback = false, options = {}) {
    const value = settingValue(key, undefined, options);
    if (typeof value === 'boolean') return value;
    if (typeof value === 'number') {
        if (value === 1) return true;
        if (value === 0) return false;
        return fallback;
    }

    const normalized = String(value ?? '').trim().toLowerCase();
    if (['1', 'true', 'yes', 'y', 'on'].includes(normalized)) return true;
    if (['0', 'false', 'no', 'n', 'off'].includes(normalized)) return false;
    return fallback;
}

export function settingList(key, fallback = [], options = {}) {
    const value = settingValue(key, undefined, options);
    if (Array.isArray(value)) return value.map((item) => String(item));
    if (value === undefined || value === null || value === '') return fallback;

    const separator = options.separator || ',';
    const parts = String(value).split(separator).map((item) => (options.trim === false ? item : item.trim()));
    return options.keepEmpty === true ? parts : parts.filter((item) => item !== '');
}

export function settingJson(key, fallback = undefined, options = {}) {
    const value = settingValue(key, undefined, options);
    if (value === undefined || value === null || value === '') return fallback;
    if (typeof value === 'object') return value;

    try {
        return JSON.parse(String(value));
    } catch {
        return fallback;
    }
}

export function expandHome(value, options = {}) {
    const input = String(value ?? '');
    if (input === '~') return options.homeDir || os.homedir();
    if (input.startsWith('~/')) return path.join(options.homeDir || os.homedir(), input.slice(2));
    return input;
}

export function resolvePath(value, options = {}) {
    const expanded = expandHome(value, options);
    if (path.isAbsolute(expanded)) return expanded;
    return path.resolve(options.baseDir || process.env.HOMEBOY_COMPONENT_PATH || process.cwd(), expanded);
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
        let stdoutLineRemainder = '';
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
            const output = String(chunk);
            stdout += output;
            stdoutLineRemainder += output;
            const lines = stdoutLineRemainder.split('\n');
            stdoutLineRemainder = lines.pop();
            for (const line of lines) {
                if (isCanonicalRunnerProgressLine(line)) {
                    process.stdout.write(`${line}\n`);
                }
            }
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

function isCanonicalRunnerProgressLine(line) {
    if (!line.startsWith(RUNNER_PROGRESS_LINE_PREFIX)) return false;

    let envelope;
    try {
        envelope = JSON.parse(line.slice(RUNNER_PROGRESS_LINE_PREFIX.length));
    } catch {
        return false;
    }

    if (!envelope || typeof envelope !== 'object' || Array.isArray(envelope)) return false;
    if (Object.keys(envelope).some((field) => !RUNNER_PROGRESS_FIELDS.has(field))) return false;
    if (envelope.schema !== RUNNER_PROGRESS_SCHEMA) return false;
    if (!isValidProgressString(envelope.phase, 2048)) return false;
    if (!isValidProgressString(envelope.current_item, 8192)) return false;
    if (!isValidProgressCount(envelope.completed) || !isValidProgressCount(envelope.total)) return false;
    if (
        Number.isSafeInteger(envelope.completed)
        && Number.isSafeInteger(envelope.total)
        && envelope.completed > envelope.total
    ) {
        return false;
    }

    return envelope.phase !== undefined && envelope.phase !== null
        || envelope.current_item !== undefined && envelope.current_item !== null
        || envelope.completed !== undefined && envelope.completed !== null
        || envelope.total !== undefined && envelope.total !== null
        || envelope.metadata !== undefined && envelope.metadata !== null;
}

function isValidProgressString(value, maxBytes) {
    return value === undefined
        || value === null
        || (typeof value === 'string' && value.trim() !== '' && Buffer.byteLength(value) <= maxBytes);
}

function isValidProgressCount(value) {
    return value === undefined
        || value === null
        || (Number.isSafeInteger(value) && value >= 0);
}

export function runNode(args, options = {}) {
    return runCommand(options.nodeBinary || process.env.HOMEBOY_NODE_BINARY || 'node', args, options);
}

/**
 * Run a package.json script as a benchmark workload and return the standard
 * `{ metrics, artifacts, metadata }` shape consumed by bench-runner.mjs.
 */
export async function runPackageScriptBench(options = {}) {
    if (!options || typeof options !== 'object' || Array.isArray(options)) {
        throw new Error('runPackageScriptBench requires an options object.');
    }
    if (!options.script || typeof options.script !== 'string') {
        throw new Error('runPackageScriptBench requires a package script name.');
    }

    const cwd = resolvePath(options.cwd || process.env.HOMEBOY_COMPONENT_PATH || process.cwd());
    const packageJson = await readPackageJson(cwd, options.packageJsonPath);
    if (!packageJson.scripts || typeof packageJson.scripts[options.script] !== 'string') {
        throw new Error(`package script "${options.script}" is not defined in ${packageJson.path}`);
    }

    const packageManager = options.packageManager || await detectPackageManager(cwd);
    const scriptArgs = normalizeStringList(options.args);
    const specs = normalizeStringList(options.specs || options.specFiles);
    const forwardedArgs = [...scriptArgs, ...specs];
    const command = packageScriptCommand(packageManager, options.script, forwardedArgs);
    const result = await runCommand(command.command, command.args, {
        cwd,
        env: options.env,
        timeoutMs: options.timeoutMs,
        allowFailure: options.allowFailure,
        redaction: options.redaction,
        redact: options.redact,
    });

    const context = options.artifactContext || createArtifactContext({
        id: options.id || options.script,
        sharedState: options.sharedState,
        runId: options.runId,
        artifactsDir: options.artifactsDir,
    });
    const artifactName = options.artifactName || 'package-script-result';
    const artifact = await context.writeJson(artifactName, {
        package_manager: packageManager,
        script: options.script,
        command: command.command,
        args: command.args,
        cwd,
        specs,
        code: result.code,
        signal: result.signal,
        elapsed_ms: result.elapsedMs,
        stdout: result.stdout,
        stderr: result.stderr,
    }, { label: options.artifactLabel || `Package script ${options.script} result` });

    return {
        metrics: {
            package_script_elapsed_ms: metric(result.elapsedMs),
            package_script_exit_code: metric(result.code),
            package_script_stdout_bytes: Buffer.byteLength(result.stdout || ''),
            package_script_stderr_bytes: Buffer.byteLength(result.stderr || ''),
            package_script_spec_count: specs.length,
        },
        artifacts: {
            [artifactName]: artifact,
        },
        metadata: {
            package_manager: packageManager,
            package_script: options.script,
            package_script_arg_count: forwardedArgs.length,
            package_script_spec_count: specs.length,
        },
    };
}

export async function detectPackageManager(cwd = process.env.HOMEBOY_COMPONENT_PATH || process.cwd()) {
    const root = resolvePath(cwd);
    if (await fileExists(path.join(root, 'pnpm-lock.yaml'))) return 'pnpm';
    if (await fileExists(path.join(root, 'yarn.lock'))) return 'yarn';
    return 'npm';
}

export function packageScriptCommand(packageManager, script, args = []) {
    const forwardedArgs = normalizeStringList(args);
    switch (packageManager) {
        case 'pnpm':
            return { command: 'pnpm', args: ['run', script, ...scriptSeparator(forwardedArgs), ...forwardedArgs] };
        case 'yarn':
            return { command: 'yarn', args: [script, ...forwardedArgs] };
        case 'npm':
            return { command: 'npm', args: ['run', script, ...scriptSeparator(forwardedArgs), ...forwardedArgs] };
        default:
            throw new Error(`Unsupported package manager "${packageManager}".`);
    }
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

async function readPackageJson(cwd, packageJsonPath) {
    const file = packageJsonPath ? resolvePath(packageJsonPath, { baseDir: cwd }) : path.join(cwd, 'package.json');
    try {
        const parsed = JSON.parse(await readFile(file, 'utf8'));
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
            throw new Error('package.json root must be an object');
        }
        return { ...parsed, path: file };
    } catch (error) {
        throw new Error(`Unable to read package.json at ${file}: ${error.message}`);
    }
}

async function fileExists(file) {
    try {
        await access(file);
        return true;
    } catch {
        return false;
    }
}

function normalizeStringList(value) {
    if (value === undefined || value === null || value === '') return [];
    const values = Array.isArray(value) ? value : [value];
    return values.map((item) => String(item)).filter((item) => item !== '');
}

function scriptSeparator(args) {
    return args.length > 0 ? ['--'] : [];
}

function settingValue(key, fallback = undefined, options = {}) {
    const env = options.env || process.env;
    const resolved = settings(env);
    if (Object.hasOwn(resolved, key) && resolved[key] !== undefined && resolved[key] !== null) {
        return resolved[key];
    }

    const envKey = `${options.prefix || DEFAULT_SETTINGS_PREFIX}${String(key).toUpperCase()}`;
    return env[envKey] !== undefined ? env[envKey] : fallback;
}
