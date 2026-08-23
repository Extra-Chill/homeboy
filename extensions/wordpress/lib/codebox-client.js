'use strict';

/**
 * External dependencies
 */
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

/**
 * Internal dependencies
 */
const resolver = require('./wp-codebox-resolver');
const { canonicalWpCodeboxRuntime } = require('./wp-codebox-recipe-helper');

const DEFAULT_PUBLIC_CLI_MAX_BUFFER_BYTES = 1024 * 1024 * 128;

function createCodeboxClient(options = {}) {
	return new CodeboxClient(options);
}

class CodeboxClient {
	constructor(options = {}) {
		this.options = options;
	}

	identity(options = {}) {
		return resolver.resolveWpCodeboxIdentity({ ...this.options, ...options });
	}

	publicCliBin(options = {}) {
		return this.identity({ ...this.options, ...options }).bin;
	}

	publicCliInvocation(options = {}) {
		const merged = { ...this.options, ...options };
		return this.identity(merged).invocation;
	}

	runPublicCliCommand(args, options = {}) {
		const merged = { ...this.options, ...options };
		const runtime = canonicalWpCodeboxRuntime({
			...merged,
			wp_codebox_bin: merged.wp_codebox_bin || merged.wpCodeboxBin,
		});
		const command = runtime.selected.path;
		if (typeof merged.runPublicCli === 'function') {
			return normalizeCliResult(merged.runPublicCli({ command, args, stdin: merged.stdin }, merged));
		}
		if (typeof merged.runCli === 'function') {
			return normalizeCliResult(merged.runCli({ command, args, stdin: merged.stdin }, merged));
		}

		const invocation = runtime.invocation;
		const result = spawnSync(invocation.command, [...invocation.args, ...args], {
			input: merged.stdin,
			encoding: 'utf8',
			env: { ...process.env, ...(merged.env || {}) },
			cwd: merged.cwd,
			maxBuffer: merged.maxBuffer || merged.max_buffer || DEFAULT_PUBLIC_CLI_MAX_BUFFER_BYTES,
		});
		return normalizeCliResult(result);
	}

	runPublicJsonCommand(command, input, options = {}) {
		const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-'));
		const inputFile = path.join(tempDir, 'input.json');
		try {
			fs.writeFileSync(inputFile, `${JSON.stringify(input)}\n`, 'utf8');
			return this.runPublicCliCommand(publicJsonArgs(command, inputFile, options), options);
		} finally {
			fs.rmSync(tempDir, { recursive: true, force: true });
		}
	}

	runArtifactApplyPreflight({ bundlePath, approvedFiles, cwd, env } = {}) {
		if (!bundlePath) throw new Error('WP Codebox artifact apply preflight requires bundlePath.');
		const args = publicArtifactApplyPreflightArgs({ bundlePath, approvedFiles });
		const result = this.runPublicCliCommand(args, { cwd, env });
		if (result.status !== 0) throw new Error(`${this.publicCliBin({ env })} ${args.join(' ')} failed: ${[result.stderr, result.stdout].filter(Boolean).join('\n').trim()}`);
		return parseJsonCliOutput(result.stdout, args.join(' '));
	}

	runArtifactsDiscoverPartial({ artifactsRoot, sessionId, startedAt, finishedAt, cwd, env, wpCommand, wpCli } = {}) {
		const args = ['artifacts', 'discover-partial', '--artifacts', artifactsRoot, '--json'];
		if (sessionId) {
			args.push('--session-id', sessionId);
		}
		if (startedAt) {
			args.push('--started-at', startedAt);
		}
		if (finishedAt) {
			args.push('--finished-at', finishedAt);
		}

		const result = this.runPublicCliCommand(args, { cwd, env, wpCommand, wpCli });
		if (result.status !== 0) {
			const detail = [result.stderr, result.stdout].filter(Boolean).join('\n').trim();
			throw new Error(`${this.publicCliBin({ env, wpCommand, wpCli })} ${args.join(' ')} failed${detail ? `: ${detail}` : ''}`);
		}
		return parseJsonCliOutput(result.stdout, args.join(' '));
	}

}

function publicJsonArgs(command, inputFile, options = {}) {
	const runnerMode = typeof options.runnerMode === 'string' && options.runnerMode.trim()
		? options.runnerMode.trim()
		: typeof options.runner_mode === 'string' && options.runner_mode.trim()
			? options.runner_mode.trim()
			: '';
	if (command === 'run-fuzz-suite' && runnerMode) {
		return [command, `--runner-mode=${runnerMode}`, '--input-file', inputFile, '--json'];
	}
	return [command, '--input-file', inputFile, '--format=json'];
}

function publicArtifactApplyPreflightArgs({ bundlePath, approvedFiles } = {}) {
	const args = ['artifacts', 'apply-preflight', '--bundle', bundlePath, '--json'];
	for (const filePath of approvedFiles || []) {
		args.push('--approved-file', filePath);
	}
	return args;
}

function parseJsonCliOutput(stdout, label = 'wp-codebox') {
	const text = String(stdout || '').trim();
	try {
		return JSON.parse(text);
	} catch (error) {
		throw new Error(`${label} did not return valid JSON${text ? `: ${text}` : ''}`);
	}
}

function normalizeCliResult(result = {}) {
	let status = 0;
	if (Number.isInteger(result.status)) {
		status = result.status;
	} else if (result.error) {
		status = 1;
	}
	return {
		status,
		stdout: String(result.stdout || ''),
		stderr: String(result.stderr || result.error?.message || ''),
		...(result.error ? { error: result.error } : {}),
	};
}

module.exports = {
	CodeboxClient,
	createCodeboxClient,
	normalizeCliResult,
	parseJsonCliOutput,
	publicArtifactApplyPreflightArgs,
	publicJsonArgs,
};
