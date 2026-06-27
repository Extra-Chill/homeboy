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
const legacyCore = require('./wp-codebox-core-loader');
const resolver = require('./wp-codebox-resolver');

const DEFAULT_PUBLIC_CLI_MAX_BUFFER_BYTES = 1024 * 1024 * 128;
const ARTIFACT_COMPATIBILITY_OPTIONS = {
	packageCandidates: [
		'@automattic/wp-codebox-core/artifacts',
		'wp-codebox-workspace/artifacts',
	],
	packageDistEntries: ['artifacts.js'],
};

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

	identityDiagnostics(identity = this.identity()) {
		return resolver.wpCodeboxIdentityMismatchDiagnostics(identity);
	}

	command(bin = resolver.DEFAULT_WP_CODEBOX_BIN) {
		return resolver.wpCodeboxCommand(bin);
	}

	publicCliBin(options = {}) {
		const merged = { ...this.options, ...options };
		if (merged.wpCliBin || merged.wp_cli_bin) {
			return merged.wpCliBin || merged.wp_cli_bin;
		}
		const env = { ...process.env, ...(merged.env || {}) };
		if (env.wpCliBin || env.wp_cli_bin || env.HOMEBOY_WP_CLI_BIN || env.WP_CLI_BIN) {
			return env.wpCliBin || env.wp_cli_bin || env.HOMEBOY_WP_CLI_BIN || env.WP_CLI_BIN;
		}
		const identity = this.identity(merged);
		return identity.selectionSource === 'default' ? resolver.DEFAULT_WP_CLI_BIN : identity.bin;
	}

	publicCliInvocation(options = {}) {
		const merged = { ...this.options, ...options };
		const identity = this.identity(merged);
		const bin = this.publicCliBin(merged);
		if (bin === identity.bin) {
			return identity.invocation;
		}
		const invocation = this.command(bin);
		const executable = path.basename(String(bin || '')).toLowerCase();
		const usesWpCliNamespace = executable === 'wp' || executable === 'wp-cli' || executable === 'wp-cli.phar';
		return {
			command: invocation.command,
			args: usesWpCliNamespace ? [...invocation.args, 'codebox'] : invocation.args,
		};
	}

	runPublicCliCommand(args, options = {}) {
		const merged = { ...this.options, ...options };
		if (typeof merged.runPublicCli === 'function') {
			return normalizeCliResult(merged.runPublicCli({ command: this.publicCliBin(merged), args, stdin: merged.stdin }, merged));
		}
		if (typeof merged.runCli === 'function') {
			return normalizeCliResult(merged.runCli({ command: this.publicCliBin(merged), args, stdin: merged.stdin }, merged));
		}

		const invocation = this.publicCliInvocation(merged);
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

	runArtifactApplyPreflight({ artifactId, artifactsPath, approvedFiles, cwd, env, wpCommand, wpCli } = {}) {
		const command = wpCommand || wpCli || process.env.HOMEBOY_WP_CLI || resolver.DEFAULT_WP_CLI_BIN;
		const args = [
			'codebox',
			'artifacts',
			'preflight-apply',
			artifactId,
			`--artifacts-path=${artifactsPath}`,
			`--approved-files=${JSON.stringify(approvedFiles)}`,
			'--format=json',
		];
		const result = spawnSync(command, args, { cwd, encoding: 'utf8', env: env || process.env });
		if (result.status !== 0) {
			const detail = [result.stderr, result.stdout].filter(Boolean).join('\n').trim();
			throw new Error(`${command} ${args.join(' ')} failed${detail ? `: ${detail}` : ''}`);
		}
		return JSON.parse((result.stdout || '').trim());
	}

	async loadCompatibilityExport(name, options = {}) {
		return legacyCore.loadWpCodeboxCoreExport(name, { ...this.options, ...options });
	}

	async loadCompatibilityFunction(name, options = {}) {
		const result = await this.loadCompatibilityExport(name, options);
		return result ? result.value : null;
	}

	async loadArtifactCompatibilityFunction(name, options = {}) {
		return this.loadCompatibilityFunction(name, {
			...ARTIFACT_COMPATIBILITY_OPTIONS,
			...options,
		});
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
	ARTIFACT_COMPATIBILITY_OPTIONS,
	createCodeboxClient,
	normalizeCliResult,
	publicJsonArgs,
};
