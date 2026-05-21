import { execFile } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import { request as httpRequest } from 'node:http';
import { request as httpsRequest } from 'node:https';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);

export async function withObservationWindow(promise, timeoutMs, options = {}) {
    let timeout;
    const timeoutPromise = new Promise((resolve, reject) => {
        timeout = setTimeout(() => {
            const result = { status: 'timeout', timeout_ms: timeoutMs };
            emit(options.onTimeout || options.onEvent, 'observation', 'observation.timeout', result);
            if (options.rejectOnTimeout) {
                reject(new Error(`Observation timed out after ${timeoutMs}ms`));
                return;
            }
            resolve(result);
        }, timeoutMs);
    });

    try {
        return await Promise.race([promise, timeoutPromise]);
    } finally {
        clearTimeout(timeout);
    }
}

export async function pollHttp(url, options = {}) {
    const source = options.source || 'http';
    const intervalMs = options.intervalMs ?? 250;
    const timeoutMs = options.timeoutMs ?? 30000;
    const requestTimeoutMs = options.requestTimeoutMs ?? 5000;
    const readyStatus = normalizeReadyStatus(options.readyStatus ?? [200, 399]);
    const deadline = Date.now() + timeoutMs;
    const startedAt = Date.now();
    const statusHistory = [];
    let first = true;
    let lastStatus;
    let lastNonReadyStatus;
    let lastError;

    while (Date.now() <= deadline) {
        const response = await httpStatus(url, requestTimeoutMs);

        if (response.ok) {
            const elapsedMs = Date.now() - startedAt;
            const data = httpStatusEventData(url, response);
            recordHttpStatus(statusHistory, response.status, elapsedMs, response.location);
            if (first) await emit(options.onEvent, source, 'http.first_response', data);
            if (first || response.status !== lastStatus) await emit(options.onEvent, source, 'http.status', data);
            lastStatus = response.status;
            if (isReadyStatus(response.status, readyStatus)) {
                const summary = httpStatusSummary(url, statusHistory, lastNonReadyStatus, lastError);
                await emit(options.onEvent, source, 'http.status_summary', summary);
                await emit(options.onEvent, source, 'http.ready', { ...data, ...summary });
                return { status: 'ready', http_status: response.status, ...summary };
            }
            lastNonReadyStatus = response.status;
        } else {
            lastError = response.error;
            if (first) await emit(options.onEvent, source, 'http.first_error', { url, error: response.error });
        }

        first = false;
        await sleep(intervalMs);
    }

    const summary = httpStatusSummary(url, statusHistory, lastNonReadyStatus, lastError);
    await emit(options.onEvent, source, 'http.status_summary', summary);
    await emit(options.onEvent, source, 'http.timeout', { url, last_status: lastStatus ?? null, last_error: lastError ?? null, ...summary });
    return { status: 'timeout', http_status: lastStatus ?? null, error: lastError ?? null, ...summary };
}

export function createHttpStatusHistory() {
    const history = [];
    return {
        record(status, elapsedMs = undefined) {
            recordHttpStatus(history, status, elapsedMs);
            return this.summary();
        },
        summary(options = {}) {
            return httpStatusSummary(options.url, history, options.lastNonReadyStatus, options.lastError);
        },
        entries() {
            return history.map((entry) => ({ ...entry }));
        },
    };
}

export async function pollJsonFile(filePath, options = {}) {
    const source = options.source || 'json-file';
    const intervalMs = options.intervalMs ?? 250;
    const timeoutMs = options.timeoutMs ?? 30000;
    const select = options.select || ((json) => json);
    const eventSpecs = options.events || [];
    const terminalEvents = new Set(options.terminalEvents || eventSpecs.filter((event) => event.terminal).map((event) => event.name));
    const emitted = new Set();
    const deadline = Date.now() + timeoutMs;
    let lastValue;
    let sawFile = false;

    while (Date.now() <= deadline) {
        const parsed = await readJsonIfAvailable(filePath);
        if (parsed.status === 'ok') {
            if (!sawFile) {
                sawFile = true;
                await emit(options.onEvent, source, 'json.file_seen', { path: filePath });
            }
            lastValue = select(parsed.json);
            for (const spec of eventSpecs) {
                if (!spec || emitted.has(spec.name)) continue;
                if (spec.when(lastValue, parsed.json)) {
                    emitted.add(spec.name);
                    const data = typeof spec.data === 'function' ? spec.data(lastValue, parsed.json) : { value: lastValue };
                    await emit(options.onEvent, source, spec.name, data);
                    if (terminalEvents.has(spec.name)) return { status: 'matched', event: spec.name, value: lastValue };
                }
            }
        } else if (parsed.status === 'parse_error') {
            await emit(options.onEvent, source, 'json.parse_error', { path: filePath, error: parsed.error });
        }

        await sleep(intervalMs);
    }

    await emit(options.onEvent, source, 'json.timeout', { path: filePath, value: lastValue ?? null });
    return { status: 'timeout', value: lastValue ?? null };
}

export async function pollProcess(pattern, options = {}) {
    const source = options.source || 'process';
    const intervalMs = options.intervalMs ?? 250;
    const timeoutMs = options.timeoutMs ?? 30000;
    const matcher = makeMatcher(pattern);
    const deadline = Date.now() + timeoutMs;
    let seen = false;
    let lastMatch = null;

    while (Date.now() <= deadline) {
        let processes = [];
        try {
            processes = await listProcesses();
        } catch (err) {
            await emit(options.onEvent, source, 'process.error', { error: err.message });
        }
        const match = processes.find((row) => row.pid !== process.pid && matcher(row.command));

        if (match && !seen) {
            seen = true;
            lastMatch = match;
            await emit(options.onEvent, source, 'process.seen', match);
            if ((options.until || 'seen') === 'seen') return { status: 'seen', process: match };
        }

        if (!match && seen && (options.until || 'seen') === 'gone') {
            await emit(options.onEvent, source, 'process.gone', lastMatch || {});
            return { status: 'gone', process: lastMatch };
        }

        await sleep(intervalMs);
    }

    await emit(options.onEvent, source, 'process.timeout', { pattern: String(pattern), seen });
    return { status: 'timeout', process: lastMatch };
}

export async function parseLogLines(text, patterns, onEvent, options = {}) {
    const source = options.source || 'log';
    const lines = String(text || '').split(/\r?\n/).filter(Boolean);
    const emitted = [];

    for (const line of lines) {
        for (const pattern of patterns || []) {
            const match = line.match(pattern.match || pattern.regex || pattern.pattern);
            if (!match) continue;
            const data = typeof pattern.data === 'function' ? pattern.data(match, line) : { line };
            const event = pattern.event || pattern.name;
            emitted.push(await emit(onEvent, pattern.source || source, event, data));
            if (pattern.once) break;
        }
    }

    return emitted;
}

export function installConsoleBridge(page, options = {}) {
    const prefix = options.prefix || 'trace:';
    const source = options.source || 'browser';
    const handler = async (message) => {
        const text = typeof message.text === 'function' ? message.text() : String(message);
        if (!text.startsWith(prefix)) return;
        const raw = text.slice(prefix.length).trim();
        let data = { message: raw };
        try {
            const parsed = JSON.parse(raw);
            data = parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : { value: parsed };
        } catch {
            // Plain console messages are valid bridge payloads.
        }
        await emit(options.onEvent, source, options.event || 'console.bridge', data);
    };

    if (typeof page.on !== 'function') throw new Error('installConsoleBridge requires a page-like object with on(event, handler)');
    page.on('console', handler);
    return handler;
}

async function emit(onEvent, source, event, data = {}) {
    if (!onEvent) return { source, event, data };
    const normalized = data && typeof data === 'object' && !Array.isArray(data) ? data : { value: data };
    return await onEvent(source, event, normalized);
}

function normalizeReadyStatus(value) {
    if (Array.isArray(value) && value.length === 2 && value.every((item) => Number.isFinite(Number(item)))) {
        return { min: Number(value[0]), max: Number(value[1]) };
    }
    if (Array.isArray(value)) return new Set(value.map(Number));
    return new Set([Number(value)]);
}

function isReadyStatus(status, readyStatus) {
    if (readyStatus instanceof Set) return readyStatus.has(status);
    return status >= readyStatus.min && status <= readyStatus.max;
}

function httpStatus(url, timeoutMs) {
    return new Promise((resolve) => {
        const parsed = new URL(url);
        const transport = parsed.protocol === 'https:' ? httpsRequest : httpRequest;
        const req = transport(parsed, { method: 'GET', timeout: timeoutMs }, (res) => {
            res.resume();
            res.on('end', () => resolve({ ok: true, status: res.statusCode || 0, location: redirectLocation(res) }));
        });
        req.on('timeout', () => {
            req.destroy(new Error(`request timed out after ${timeoutMs}ms`));
        });
        req.on('error', (err) => resolve({ ok: false, error: err.message }));
        req.end();
    });
}

function redirectLocation(res) {
    const status = Number(res.statusCode || 0);
    if (status < 300 || status > 399) return undefined;
    const location = res.headers.location;
    return typeof location === 'string' && location ? location : undefined;
}

function httpStatusEventData(url, response) {
    const data = { url, status: response.status };
    if (response.location) data.location = response.location;
    return data;
}

function recordHttpStatus(history, status, elapsedMs = undefined, location = undefined) {
    const normalizedStatus = Number(status);
    const last = history.at(-1);
    if (last && last.status === normalizedStatus) {
        last.count += 1;
        if (elapsedMs !== undefined) last.last_seen_ms = elapsedMs;
        if (location) last.location = location;
        return last;
    }

    const entry = { status: normalizedStatus, count: 1 };
    if (location) entry.location = location;
    if (elapsedMs !== undefined) {
        entry.first_seen_ms = elapsedMs;
        entry.last_seen_ms = elapsedMs;
    }
    history.push(entry);
    return entry;
}

function httpStatusSummary(url, history, lastNonReadyStatus = undefined, lastError = undefined) {
    const statusHistory = history.map((entry) => ({ ...entry }));
    const summary = {
        status_history: statusHistory,
        status_transition_count: Math.max(0, statusHistory.length - 1),
        repeated_status_count: statusHistory.reduce((total, entry) => total + Math.max(0, entry.count - 1), 0),
        last_non_ready_status: lastNonReadyStatus ?? null,
    };
    if (url) summary.url = url;
    if (lastError) summary.last_error = lastError;
    return summary;
}

async function readJsonIfAvailable(filePath) {
    try {
        const text = await readFile(filePath, 'utf8');
        return { status: 'ok', json: JSON.parse(text) };
    } catch (err) {
        if (err.code === 'ENOENT') return { status: 'missing' };
        if (err instanceof SyntaxError) return { status: 'parse_error', error: err.message };
        return { status: 'error', error: err.message };
    }
}

async function listProcesses() {
    const { stdout } = await execFileAsync('ps', ['-axo', 'pid=,etime=,args='], { maxBuffer: 1024 * 1024 });
    return stdout.split(/\r?\n/).filter(Boolean).map(parsePsLine).filter(Boolean);
}

function parsePsLine(line) {
    const match = line.trim().match(/^(\d+)\s+(\S+)\s+(.+)$/);
    if (!match) return null;
    return { pid: Number(match[1]), elapsed: match[2], command: match[3] };
}

function makeMatcher(pattern) {
    if (pattern instanceof RegExp) {
        return (value) => {
            pattern.lastIndex = 0;
            return pattern.test(value);
        };
    }
    return (value) => value.includes(String(pattern));
}

function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
}
