'use strict';

/**
 * External dependencies
 */
const { spawn } = require('node:child_process');
const path = require('node:path');

/**
 * Internal dependencies
 */
const { isPlainObject } = require('./shared');

const DEFAULT_TIMEOUT_MS = 30000;
const MAX_TIMEOUT_MS = 600000;

function normalizeTimeoutMs(value, fallback = DEFAULT_TIMEOUT_MS) {
	const number = Number(value ?? fallback);

	if (!Number.isFinite(number) || number < 1) {
		return fallback;
	}

	return Math.min(Math.floor(number), MAX_TIMEOUT_MS);
}

function resolveContainedCwd(cwd, root = process.cwd()) {
	const base = path.resolve(root);
	const resolved = path.resolve(base, cwd || '.');
	const relative = path.relative(base, resolved);

	if (relative.startsWith('..') || path.isAbsolute(relative)) {
		throw new Error(`Terminal action cwd must stay inside the runtime root: ${cwd}`);
	}

	return resolved;
}

function normalizeTerminalAction(action, options = {}) {
	if (!isPlainObject(action)) {
		throw new TypeError('Terminal action must be an object');
	}

	const type = typeof action.type === 'string' ? action.type.trim() : '';
	const command = typeof action.command === 'string' ? action.command.trim() : '';

	if (type !== 'terminal' && type !== 'wp_cli') {
		throw new Error(`Unsupported terminal action type: ${type || '(missing)'}`);
	}
	if (command === '') {
		throw new Error('Terminal action command must be a non-empty string');
	}

	const runtimeRoot = options.runtimeRoot || process.cwd();
	const cwd = resolveContainedCwd(action.cwd || options.cwd || '.', runtimeRoot);
	const timeoutMs = normalizeTimeoutMs(action.timeout_ms ?? action.timeoutMs, options.timeoutMs);
	const shell = action.shell || options.shell || 'bash';
	const shellArgs = Array.isArray(action.shell_args || action.shellArgs)
		? action.shell_args || action.shellArgs
		: ['-lc'];
	const env = {
		...process.env,
		...(options.env || {}),
		...(isPlainObject(action.env) ? action.env : {}),
	};

	if (type === 'wp_cli') {
		const trimmed = command.startsWith('wp ') ? command : `wp ${command}`;

		return {
			type,
			command: trimmed,
			cwd,
			timeoutMs,
			shell,
			shellArgs,
			env,
		};
	}

	return { type, command, cwd, timeoutMs, shell, shellArgs, env };
}

function executeTerminalAction(action, options = {}) {
	const normalized = normalizeTerminalAction(action, options);
	const startedAt = new Date().toISOString();
	const started = Date.now();

	return new Promise((resolve) => {
		const child = spawn(normalized.shell, [...normalized.shellArgs, normalized.command], {
			cwd: normalized.cwd,
			env: normalized.env,
			stdio: ['ignore', 'pipe', 'pipe'],
		});
		let stdout = '';
		let stderr = '';
		let timedOut = false;
		const timeout = setTimeout(() => {
			timedOut = true;
			child.kill('SIGTERM');
		}, normalized.timeoutMs);

		child.stdout.on('data', (chunk) => {
			stdout += chunk.toString();
		});
		child.stderr.on('data', (chunk) => {
			stderr += chunk.toString();
		});
		child.on('error', (error) => {
			clearTimeout(timeout);
			resolve({
				type: normalized.type,
				command: normalized.command,
				cwd: normalized.cwd,
				startedAt,
				durationMs: Date.now() - started,
				exitCode: 1,
				stdout,
				stderr: stderr || error.message,
				success: false,
				timedOut,
				error: error.message,
			});
		});
		child.on('close', (code, signal) => {
			clearTimeout(timeout);
			const exitCode = timedOut ? 124 : (code ?? 1);
			resolve({
				type: normalized.type,
				command: normalized.command,
				cwd: normalized.cwd,
				startedAt,
				durationMs: Date.now() - started,
				exitCode,
				signal: signal || null,
				stdout,
				stderr,
				success: exitCode === 0,
				timedOut,
				error: exitCode === 0 ? '' : (stderr.trim() || stdout.trim() || (timedOut ? 'Command timed out' : 'Command failed')),
			});
		});
	});
}

module.exports = {
	executeTerminalAction,
	normalizeTerminalAction,
};
