'use strict';

const http = require('node:http');
const https = require('node:https');
const net = require('node:net');

const DEFAULT_READINESS_PATH = '/wp-json/';
const DEFAULT_DIAGNOSTIC_PATHS = ['/', '/wp-login.php', '/wp-json/', '/wp-admin/'];
const DEFAULT_INTERVAL_MS = 1000;
const DEFAULT_REQUEST_TIMEOUT_MS = 5000;
const DEFAULT_TIMEOUT_MS = 120000;
const DEFAULT_BODY_PREVIEW_BYTES = 4096;
const BODY_PREVIEW_CHAR_LIMIT = 500;
const RECENT_ATTEMPTS_LIMIT = 10;
const PLAYGROUND_OUTPUT_TAIL_LIMIT = 4000;

function normalizeBaseUrl(baseUrl) {
	if (typeof baseUrl !== 'string' || baseUrl.trim() === '') {
		throw new TypeError('baseUrl must be a non-empty string');
	}
	const trimmed = baseUrl.trim();
	const parsed = new URL(trimmed);
	const origin = `${parsed.protocol}//${parsed.host}`;
	return { origin, host: parsed.hostname, port: Number(parsed.port) || (parsed.protocol === 'https:' ? 443 : 80) };
}

function buildUrl(origin, path) {
	const cleanPath = path && path.startsWith('/') ? path : `/${path || ''}`;
	return `${origin}${cleanPath}`;
}

function normalizeReadyStatus(value) {
	if (Array.isArray(value)) {
		return new Set(value.map(Number));
	}
	return new Set([Number(value)]);
}

function isReadyStatus(status, readySet) {
	return readySet.has(Number(status));
}

function recordHttpStatus(history, status) {
	const normalized = Number(status);
	const last = history.at(-1);
	if (last && last.status === normalized) {
		last.count += 1;
		return;
	}
	history.push({ status: normalized, count: 1 });
}

function serializeError(err) {
	if (!err) return null;
	const out = {
		name: err.name || 'Error',
		message: err.message || String(err),
	};
	if (err.code) out.code = err.code;
	if (err.cause) {
		out.cause = err.cause instanceof Error ? serializeError(err.cause) : String(err.cause);
	}
	return out;
}

function probeTcp(host, port, timeoutMs) {
	return new Promise((resolve) => {
		const socket = net.createConnection({ host, port });
		let settled = false;
		const finish = (result) => {
			if (settled) return;
			settled = true;
			try {
				socket.destroy();
			} catch (_) {
				// best-effort cleanup
			}
			resolve(result);
		};
		socket.setTimeout(timeoutMs);
		socket.once('connect', () => finish({ open: true }));
		socket.once('timeout', () => finish({ open: false, error: `tcp connect timed out after ${timeoutMs}ms` }));
		socket.once('error', (err) => finish({ open: false, error: serializeError(err).message }));
	});
}

function httpProbeOnce(url, requestTimeoutMs, options = {}) {
	const captureBody = options.captureBody === true;
	const maxBodyBytes = Number.isFinite(options.maxBodyBytes) ? options.maxBodyBytes : DEFAULT_BODY_PREVIEW_BYTES;
	return new Promise((resolve) => {
		let parsed;
		try {
			parsed = new URL(url);
		} catch (err) {
			resolve({ ok: false, error: serializeError(err) });
			return;
		}
		const transport = parsed.protocol === 'https:' ? https : http;
		const startedAt = Date.now();
		let settled = false;
		const finish = (value) => {
			if (settled) return;
			settled = true;
			resolve(value);
		};

		const req = transport.request(parsed, { method: 'GET', timeout: requestTimeoutMs }, (res) => {
			const status = res.statusCode || 0;
			const headers = res.headers || {};
			if (!captureBody) {
				res.resume();
				res.once('end', () => finish({ ok: true, status, headers, elapsedMs: Date.now() - startedAt }));
				res.once('error', (err) => finish({ ok: false, error: serializeError(err), elapsedMs: Date.now() - startedAt }));
				return;
			}
			const chunks = [];
			let received = 0;
			res.on('data', (chunk) => {
				if (received >= maxBodyBytes) return;
				const room = maxBodyBytes - received;
				const slice = chunk.length <= room ? chunk : chunk.slice(0, room);
				chunks.push(slice);
				received += slice.length;
			});
			res.once('end', () => {
				let body;
				try {
					body = Buffer.concat(chunks).toString('utf8');
				} catch (_) {
					body = '';
				}
				finish({ ok: true, status, headers, body, elapsedMs: Date.now() - startedAt });
			});
			res.once('error', (err) => finish({ ok: false, error: serializeError(err), elapsedMs: Date.now() - startedAt }));
		});

		req.on('timeout', () => {
			req.destroy(new Error(`request timed out after ${requestTimeoutMs}ms`));
		});
		req.on('error', (err) => finish({ ok: false, error: serializeError(err), elapsedMs: Date.now() - startedAt }));
		req.end();
	});
}

async function emit(onEvent, source, event, data) {
	if (typeof onEvent !== 'function') return;
	try {
		await onEvent(source, event, data || {});
	} catch (_) {
		// trace bridge errors must not break readiness polling
	}
}

function sleep(ms) {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

function processExited(child) {
	if (!child) return false;
	return child.exitCode !== null && child.exitCode !== undefined
		|| child.signalCode !== null && child.signalCode !== undefined;
}

async function waitForWordPressReady(baseUrl, options = {}) {
	const { origin, host, port } = normalizeBaseUrl(baseUrl);
	const path = typeof options.path === 'string' && options.path !== '' ? options.path : DEFAULT_READINESS_PATH;
	const url = buildUrl(origin, path);
	const readySet = normalizeReadyStatus(options.readyStatus !== undefined ? options.readyStatus : 200);
	const intervalMs = Number.isFinite(options.intervalMs) ? options.intervalMs : DEFAULT_INTERVAL_MS;
	const requestTimeoutMs = Number.isFinite(options.requestTimeoutMs) ? options.requestTimeoutMs : DEFAULT_REQUEST_TIMEOUT_MS;
	const timeoutMs = Number.isFinite(options.timeoutMs) ? options.timeoutMs : DEFAULT_TIMEOUT_MS;
	const onEvent = options.onEvent;
	const child = options.playgroundProcess || null;
	const playgroundOutput = typeof options.playgroundOutput === 'function' ? options.playgroundOutput : null;

	const startedAt = Date.now();
	const deadline = startedAt + timeoutMs;
	const statusHistory = [];
	const redirectHistory = [];
	const recentAttempts = [];
	let attempts = 0;
	let lastStatus = null;
	let lastError = null;
	let firstResponseEmitted = false;

	while (Date.now() <= deadline) {
		if (processExited(child)) {
			const elapsedMs = Date.now() - startedAt;
			const playground = {
				pid: child.pid ?? null,
				exitCode: child.exitCode ?? null,
				signalCode: child.signalCode ?? null,
			};
			await emit(onEvent, 'http', 'process.exited', { url, ...playground, elapsedMs });
			return {
				status: 'process_exited',
				url,
				http_status: null,
				status_history: statusHistory,
				redirect_history: redirectHistory,
				elapsedMs,
				playground,
			};
		}

		attempts += 1;
		const probe = await httpProbeOnce(url, requestTimeoutMs);
		const attemptElapsed = Date.now() - startedAt;
		const attemptRecord = { at_ms: attemptElapsed };

		if (probe.ok) {
			const status = probe.status;
			recordHttpStatus(statusHistory, status);
			attemptRecord.status = status;
			if (!firstResponseEmitted) {
				firstResponseEmitted = true;
				await emit(onEvent, 'http', 'http.first_response', { url, status });
			}
			if (status !== lastStatus) {
				await emit(onEvent, 'http', 'http.status', { url, status });
			}
			lastStatus = status;

			const location = probe.headers && probe.headers.location;
			if (status >= 300 && status < 400 && typeof location === 'string' && location !== '') {
				redirectHistory.push({ from: url, status, location });
				attemptRecord.redirect_location = location;
				await emit(onEvent, 'http', 'http.redirect', { url, status, location });
			}

			if (isReadyStatus(status, readySet)) {
				const elapsedMs = Date.now() - startedAt;
				await emit(onEvent, 'http', 'http.ready', {
					url,
					status,
					elapsedMs,
					status_history: statusHistory,
					redirect_history: redirectHistory,
				});
				return {
					status: 'ready',
					url,
					http_status: status,
					status_history: statusHistory,
					redirect_history: redirectHistory,
					elapsedMs,
				};
			}
		} else {
			lastError = probe.error;
			attemptRecord.error = probe.error;
		}

		recentAttempts.push(attemptRecord);
		if (recentAttempts.length > RECENT_ATTEMPTS_LIMIT) {
			recentAttempts.splice(0, recentAttempts.length - RECENT_ATTEMPTS_LIMIT);
		}

		if (Date.now() + intervalMs > deadline) {
			break;
		}
		await sleep(intervalMs);
	}

	if (processExited(child)) {
		const elapsedMs = Date.now() - startedAt;
		const playground = {
			pid: child.pid ?? null,
			exitCode: child.exitCode ?? null,
			signalCode: child.signalCode ?? null,
		};
		await emit(onEvent, 'http', 'process.exited', { url, ...playground, elapsedMs });
		return {
			status: 'process_exited',
			url,
			http_status: null,
			status_history: statusHistory,
			redirect_history: redirectHistory,
			elapsedMs,
			playground,
		};
	}

	const elapsedMs = Date.now() - startedAt;
	const tcp = await probeTcp(host, port, 1000);
	const routes = await probeWordPressDiagnostics(baseUrl, { requestTimeoutMs });
	const playground = child
		? { pid: child.pid ?? null, exitCode: child.exitCode ?? null, signalCode: child.signalCode ?? null }
		: null;
	const playgroundOutputTail = playgroundOutput
		? String(playgroundOutput() || '').slice(-PLAYGROUND_OUTPUT_TAIL_LIMIT)
		: null;

	const diagnostics = {
		url,
		timeoutMs,
		attempts,
		lastError,
		recentAttempts,
		redirect_history: redirectHistory,
		status_history: statusHistory,
		routes,
		tcp,
		playground,
		playgroundOutputTail,
	};

	await emit(onEvent, 'http', 'http.timeout', { url, elapsedMs, ...diagnostics });

	const error = new Error(`Timed out waiting for ${url} after ${timeoutMs}ms`);
	error.diagnostics = diagnostics;
	throw error;
}

async function probeWordPressDiagnostics(baseUrl, options = {}) {
	const { origin, host, port } = normalizeBaseUrl(baseUrl);
	const paths = Array.isArray(options.paths) && options.paths.length > 0 ? options.paths : DEFAULT_DIAGNOSTIC_PATHS;
	const requestTimeoutMs = Number.isFinite(options.requestTimeoutMs) ? options.requestTimeoutMs : DEFAULT_REQUEST_TIMEOUT_MS;

	const entries = [];
	for (const probePath of paths) {
		const url = buildUrl(origin, probePath);
		const result = await httpProbeOnce(url, requestTimeoutMs, { captureBody: true });
		const entry = { path: probePath, url };
		if (result.ok) {
			const headers = result.headers || {};
			entry.status = result.status;
			entry.contentType = headers['content-type'] || null;
			entry.redirectLocation = (result.status >= 300 && result.status < 400 && typeof headers.location === 'string')
				? headers.location
				: null;
			entry.bodyPreview = typeof result.body === 'string'
				? result.body.slice(0, BODY_PREVIEW_CHAR_LIMIT)
				: '';
			entry.elapsedMs = result.elapsedMs ?? null;
		} else {
			entry.status = null;
			entry.contentType = null;
			entry.redirectLocation = null;
			entry.bodyPreview = '';
			entry.elapsedMs = result.elapsedMs ?? null;
			entry.error = result.error || null;
			entry.tcp = await probeTcp(host, port, 1000);
		}
		entries.push(entry);
	}

	return { origin, paths: entries };
}

module.exports = {
	DEFAULT_READINESS_PATH,
	DEFAULT_DIAGNOSTIC_PATHS,
	waitForWordPressReady,
	probeWordPressDiagnostics,
};
