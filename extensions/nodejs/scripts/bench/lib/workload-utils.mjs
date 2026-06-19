import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
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

/**
 * Run a WordPress/Codebox visual-compare recipe and emit a normalized visual
 * parity artifact for Homeboy bench workloads.
 */
export async function runWordPressCodeboxVisualParityWorkload(options = {}) {
    if (!options || typeof options !== 'object' || Array.isArray(options)) {
        throw new Error('runWordPressCodeboxVisualParityWorkload requires an options object.');
    }

    const id = sanitizeSegment(options.id || 'visual-parity');
    const cwd = resolvePath(options.cwd || process.env.HOMEBOY_COMPONENT_PATH || process.cwd());
    const context = options.artifactContext || createArtifactContext({
        id,
        sharedState: options.sharedState,
        runId: options.runId,
        artifactsDir: options.artifactsDir,
    });
    const artifactDirectory = options.codeboxArtifactsDir
        ? resolvePath(options.codeboxArtifactsDir, { baseDir: cwd })
        : path.join(context.artifactDir, 'codebox');
    const source = normalizeVisualParitySource(options.source, { cwd });
    const candidate = normalizeVisualParityCandidate(options.candidate);
    const compare = normalizeVisualParityCompareOptions(options);
    const backend = normalizeWordPressCodeboxBackend(options.backend || { codeboxCli: options.codeboxCli });
    const recipe = buildVisualParityRecipe({ artifactDirectory, candidate, compare, source });
    const recipePath = context.artifactPath(`${id}-wp-codebox-recipe`, { kind: 'json', extension: 'json' });
    await writeJson(recipePath, recipe, { redact: false });

    const server = source.server ? createStaticFileServer(source.server.root) : undefined;
    if (server) await listen(server, source.server.port);

    let codeboxResult;
    try {
        const result = await runNode([backend.codeboxCli, 'recipe-run', '--recipe', recipePath, '--json'], {
            cwd,
            timeoutMs: options.timeoutMs,
            redact: false,
        });
        codeboxResult = parseJsonOutput(result.stdout, 'WP Codebox recipe output');
    } finally {
        if (server) server.close();
    }

    const visualDiffRef = findVisualCompareArtifactRef(codeboxResult) || 'files/browser/visual-compare/visual-diff.json';
    const visualDiffPath = path.join(artifactDirectory, visualDiffRef);
    const visualDiff = parseJsonOutput(await readFile(visualDiffPath, 'utf8'), visualDiffPath);
    const normalized = normalizeVisualParityArtifact({
        artifactDirectory,
        candidate,
        codeboxResult,
        compare,
        recipePath,
        source,
        visualDiff,
        visualDiffRef,
    });
    const artifact = await context.writeJson(options.artifactName || 'visual-parity-artifact', normalized, {
        label: options.artifactLabel || 'Visual parity artifact',
        kind: 'visual-parity-artifact',
        redact: false,
    });

    return {
        metrics: {
            visual_parity_pass: normalized.summary.pass ? 1 : 0,
            visual_parity_mismatch_ratio: metric(normalized.summary.mismatch_ratio),
            visual_parity_mismatch_pixels: metric(normalized.summary.mismatch_pixels),
            visual_parity_total_pixels: metric(normalized.summary.total_pixels),
            visual_parity_dimension_mismatch: normalized.summary.dimension_mismatch ? 1 : 0,
        },
        artifacts: {
            visualParity: artifact,
        },
        metadata: {
            visual_parity_schema: normalized.schema,
            visual_parity_status: normalized.summary.status,
            visual_parity_threshold: normalized.summary.threshold,
            codebox_recipe: recipePath,
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

function normalizeVisualParitySource(source, options = {}) {
    if (!source || typeof source !== 'object' || Array.isArray(source)) {
        throw new Error('runWordPressCodeboxVisualParityWorkload requires source to be an object.');
    }
    const label = String(source.label || source.ref || 'source');
    if (source.url) {
        return { label, ref: source.ref || null, url: String(source.url), path: source.path || null };
    }
    if (!source.path) {
        throw new Error('runWordPressCodeboxVisualParityWorkload source requires url or path.');
    }
    const root = resolvePath(source.path, { baseDir: options.cwd });
    const port = Number(source.port || source.serverPort || 4173);
    if (!Number.isInteger(port) || port <= 0) {
        throw new Error(`runWordPressCodeboxVisualParityWorkload source port must be a positive integer: ${source.port}`);
    }
    const entry = source.entry || 'index.html';
    return {
        label,
        ref: source.ref || null,
        path: root,
        url: `http://127.0.0.1:${port}/${String(entry).replace(/^\/+/, '')}`,
        server: { root, port },
    };
}

function normalizeVisualParityCandidate(candidate) {
    if (!candidate || typeof candidate !== 'object' || Array.isArray(candidate)) {
        throw new Error('runWordPressCodeboxVisualParityWorkload requires candidate to be an object.');
    }
    if (!candidate.url) {
        throw new Error('runWordPressCodeboxVisualParityWorkload candidate requires url.');
    }
    return {
        label: String(candidate.label || candidate.ref || 'candidate'),
        ref: candidate.ref || null,
        url: String(candidate.url),
        recipe: candidate.recipe && typeof candidate.recipe === 'object' && !Array.isArray(candidate.recipe) ? candidate.recipe : {},
        context: candidate.context && typeof candidate.context === 'object' && !Array.isArray(candidate.context) ? candidate.context : {},
    };
}

function normalizeVisualParityCompareOptions(options) {
    const viewport = normalizeViewport(options.viewport || { width: options.width, height: options.height });
    const threshold = Number(options.threshold ?? options.maxMismatchRatio ?? 0.015);
    if (!Number.isFinite(threshold) || threshold < 0 || threshold > 1) {
        throw new Error(`runWordPressCodeboxVisualParityWorkload threshold must be between 0 and 1: ${threshold}`);
    }
    return {
        viewport,
        threshold,
        pixelThreshold: Number(options.pixelThreshold ?? 0.1),
        fullPage: options.fullPage !== false,
        waitFor: String(options.waitFor || 'domcontentloaded'),
        includeAA: Boolean(options.includeAA),
        maxRegions: positiveInteger(options.maxRegions, 8),
    };
}

function normalizeViewport(viewport) {
    const width = Number(viewport?.width ?? 1280);
    const height = Number(viewport?.height ?? 1600);
    if (!Number.isInteger(width) || width <= 0 || !Number.isInteger(height) || height <= 0) {
        throw new Error(`runWordPressCodeboxVisualParityWorkload viewport must include positive integer width and height: ${JSON.stringify(viewport)}`);
    }
    return { width, height };
}

function normalizeWordPressCodeboxBackend(backend) {
    if (!backend || typeof backend !== 'object' || Array.isArray(backend)) {
        throw new Error('runWordPressCodeboxVisualParityWorkload requires backend.codeboxCli.');
    }
    const codeboxCli = backend.codeboxCli || backend.cli;
    if (!codeboxCli || typeof codeboxCli !== 'string') {
        throw new Error('runWordPressCodeboxVisualParityWorkload requires backend.codeboxCli.');
    }
    return { codeboxCli };
}

function positiveInteger(value, fallback) {
    const parsed = Number(value ?? fallback);
    return Number.isInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function buildVisualParityRecipe({ artifactDirectory, candidate, compare, source }) {
    const visualCompareStep = {
        command: 'wordpress.visual-compare',
        args: [
            `source-url=${source.url}`,
            `candidate-url=${candidate.url}`,
            `source-label=${source.label}`,
            `candidate-label=${candidate.label}`,
            `viewport=${compare.viewport.width}x${compare.viewport.height}`,
            `full-page=${compare.fullPage ? 'true' : 'false'}`,
            `wait-for=${compare.waitFor}`,
            `threshold=${compare.pixelThreshold}`,
            `include-aa=${compare.includeAA ? 'true' : 'false'}`,
            `max-regions=${compare.maxRegions}`,
        ],
    };
    const base = {
        schema: 'wp-codebox/workspace-recipe/v1',
        workflow: {},
        artifacts: { directory: artifactDirectory },
    };
    const recipe = deepMerge(base, candidate.recipe);
    const setupSteps = Array.isArray(candidate.recipe?.workflow?.steps) ? candidate.recipe.workflow.steps : [];
    return {
        ...recipe,
        workflow: {
            ...(recipe.workflow || {}),
            steps: [...setupSteps, visualCompareStep],
        },
        artifacts: {
            ...(recipe.artifacts || {}),
            directory: artifactDirectory,
        },
    };
}

function normalizeVisualParityArtifact({ artifactDirectory, candidate, codeboxResult, compare, recipePath, source, visualDiff, visualDiffRef }) {
    const comparison = visualDiff.comparison || {};
    const mismatchPixels = metric(comparison.mismatchPixels);
    const totalPixels = metric(comparison.totalPixels);
    const mismatchRatio = totalPixels > 0 ? metric(comparison.mismatchRatio, mismatchPixels / totalPixels) : metric(comparison.mismatchRatio);
    const dimensionMismatch = Boolean(comparison.dimensionMismatch);
    const status = mismatchRatio <= compare.threshold && !dimensionMismatch ? 'passed' : 'failed';
    const files = visualDiff.files || {};
    return {
        schema: 'homeboy/VisualParityArtifact/v1',
        source: {
            label: source.label,
            ref: source.ref,
            path: source.path,
            url: source.url,
        },
        candidate: {
            label: candidate.label,
            ref: candidate.ref,
            url: candidate.url,
            context: candidate.context,
        },
        summary: {
            status,
            pass: status === 'passed',
            threshold: compare.threshold,
            mismatch_ratio: mismatchRatio,
            mismatch_pixels: mismatchPixels,
            total_pixels: totalPixels,
            dimension_mismatch: dimensionMismatch,
            region_count: Array.isArray(comparison.regions) ? comparison.regions.length : 0,
        },
        viewport: visualDiff.viewport || compare.viewport,
        options: {
            wait_for: compare.waitFor,
            full_page: compare.fullPage,
            pixel_threshold: compare.pixelThreshold,
            include_aa: compare.includeAA,
            max_regions: compare.maxRegions,
        },
        artifacts: {
            directory: artifactDirectory,
            visual_diff: visualDiffRef,
            source_screenshot: files.sourceScreenshot || null,
            candidate_screenshot: files.candidateScreenshot || null,
            diff_screenshot: files.diffScreenshot || null,
            summary: files.summary || null,
            explanation: files.visualExplanation || null,
        },
        codebox: {
            schema: visualDiff.schema || null,
            status: visualDiff.status || null,
            recipe: recipePath,
            success: codeboxResult?.success === true,
        },
        raw: {
            comparison,
            limitations: visualDiff.limitations || [],
        },
    };
}

function findVisualCompareArtifactRef(codeboxResult) {
    const commands = Array.isArray(codeboxResult?.commands) ? codeboxResult.commands : [];
    for (const command of commands) {
        const artifact = command?.artifact || command?.result?.artifact;
        const ref = artifact?.files?.visualDiff;
        if (typeof ref === 'string' && ref) return ref;
    }
    return undefined;
}

function parseJsonOutput(value, label) {
    try {
        return JSON.parse(value);
    } catch (error) {
        throw new Error(`Unable to parse ${label} as JSON: ${error.message}`);
    }
}

function deepMerge(base, override) {
    if (!override || typeof override !== 'object' || Array.isArray(override)) return base;
    const merged = { ...base };
    for (const [key, value] of Object.entries(override)) {
        if (value && typeof value === 'object' && !Array.isArray(value) && base[key] && typeof base[key] === 'object' && !Array.isArray(base[key])) {
            merged[key] = deepMerge(base[key], value);
        } else {
            merged[key] = value;
        }
    }
    return merged;
}

function createStaticFileServer(root) {
    const contentTypes = new Map([
        ['.css', 'text/css; charset=utf-8'],
        ['.html', 'text/html; charset=utf-8'],
        ['.js', 'text/javascript; charset=utf-8'],
        ['.json', 'application/json; charset=utf-8'],
        ['.png', 'image/png'],
        ['.jpg', 'image/jpeg'],
        ['.jpeg', 'image/jpeg'],
        ['.svg', 'image/svg+xml'],
        ['.webp', 'image/webp'],
    ]);
    return createServer((request, response) => {
        const url = new URL(request.url || '/', `http://${request.headers.host || '127.0.0.1'}`);
        const requestedPath = decodeURIComponent(url.pathname === '/' ? '/index.html' : url.pathname);
        const resolved = path.normalize(path.join(root, requestedPath));
        if (!resolved.startsWith(root)) {
            response.writeHead(403);
            response.end('Forbidden');
            return;
        }
        createReadStream(resolved)
            .on('error', () => {
                response.writeHead(404);
                response.end('Not found');
            })
            .once('open', () => {
                response.writeHead(200, { 'content-type': contentTypes.get(path.extname(resolved).toLowerCase()) || 'application/octet-stream' });
            })
            .pipe(response);
    });
}

function listen(server, port) {
    return new Promise((resolve, reject) => {
        server.once('error', reject);
        server.listen(port, '127.0.0.1', resolve);
    });
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
