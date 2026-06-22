#!/usr/bin/env node
'use strict';

/**
 * External dependencies
 */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
	codeboxRunAgentTaskInvocation,
	codeboxTaskRequestFromAgentTaskRequest,
} = require('../../../agent-runtimes/wp-codebox');
const {
	buildWordPressFuzzRunnerResult,
	readWordPressFuzzRunnerEnv,
	runWordPressFuzzRunnerResult,
	writeHomeboyFuzzResultsFile,
} = require('../../lib/wordpress-fuzz-runner');
const { loadWpCodeboxCoreFunction } = require('../../lib/wp-codebox-core-loader');
const {
	wpCodeboxFuzzSuiteAbility,
	wpCodeboxRuntimeContractManifest,
} = require('../../lib/wp-codebox-fuzz-run');


(async () => {
	const env = readWordPressFuzzRunnerEnv();
	const result = await buildRunnerResult(env);
	writeHomeboyFuzzResultsFile(env.resultsFile, result.homeboy_fuzz_campaign);
	process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
})().catch((error) => {
	process.stderr.write(`${error.message}\n`);
	process.exit(1);
});

async function buildRunnerResult(env) {
	if (process.env.HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH === '0') {
		return buildWordPressFuzzRunnerResult({ env });
	}

	return runWordPressFuzzRunnerResult({
		env,
		runRuntimeTask: runWpCodeboxAgentTask,
	});
}

async function runWpCodeboxAgentTask(request) {
	const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-fuzz-'));
	const command = process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN || 'wp-codebox';
	const manifest = await discoverRuntimeContractManifest();
	const publicInvocation = wpCodeboxPublicRuntimeInvocation(request, { runtimeContractManifest: manifest });

	if (publicInvocation) {
		try {
			const publicResult = await runWpCodeboxPublicRuntimeCommand(command, publicInvocation, tempDir);
			return { json: normalizeWpCodeboxAgentTaskOutput(publicResult, request) };
		} catch (error) {
			if (!shouldFallbackToRunAgentTask(error)) {
				throw error;
			}
		}
	}

	const inputFile = path.join(tempDir, 'agent-task-request.json');
	const invocation = wpCodeboxRunAgentTaskInvocation(request);
	fs.writeFileSync(inputFile, `${JSON.stringify(invocation.input, null, 2)}\n`);

	const args = wpCodeboxInvocationArgs(invocation, inputFile);
	const result = await spawnJson(command, args, {
		cwd: process.cwd(),
		env: process.env,
	});

	return { json: normalizeWpCodeboxAgentTaskOutput(result, request) };
}

async function discoverRuntimeContractManifest() {
	try {
		const runtimeContractManifest = await loadWpCodeboxCoreFunction('runtimeContractManifest');
		if (typeof runtimeContractManifest === 'function') {
			return runtimeContractManifest();
		}
	} catch {
		// Older WP Codebox installs do not publish the core package yet.
	}
	return wpCodeboxRuntimeContractManifest();
}

function wpCodeboxPublicRuntimeInvocation(request, options = {}) {
	const runtimeTask = request.executor?.config?.runtime_task || {};
	const ability = runtimeTask.ability || wpCodeboxFuzzSuiteAbility(options);
	const command = wpCodeboxCommandFromPublicAbility(ability, options);
	if (!command) {
		return null;
	}
	return {
		ability,
		command,
		input: {
			...(runtimeTask.input || {}),
			metadata: {
				...(runtimeTask.input?.metadata || {}),
				homeboy_agent_task_request: request,
			},
		},
	};
}

function wpCodeboxCommandFromPublicAbility(ability, options = {}) {
	const contracts = wpCodeboxRuntimeContractManifest(options).abilities?.wordpressRuntime || {};
	const publicAbilities = new Set([
		contracts.runWorkload,
		contracts.runFuzzSuite,
		'wp-codebox/run-wordpress-workload',
		'wp-codebox/run-fuzz-suite',
	].filter(Boolean));
	if (!publicAbilities.has(ability)) {
		return '';
	}
	return String(ability).replace(/^wp-codebox\//, '');
}

async function runWpCodeboxPublicRuntimeCommand(command, invocation, tempDir) {
	const inputFile = path.join(tempDir, `${invocation.command}-request.json`);
	fs.writeFileSync(inputFile, `${JSON.stringify(invocation.input, null, 2)}\n`);
	return spawnJson(command, [invocation.command, '--input-file', inputFile, '--json'], {
		cwd: process.cwd(),
		env: process.env,
	});
}

function shouldFallbackToRunAgentTask(error) {
	const message = String(error?.message || '').toLowerCase();
	return error?.code === 'ENOENT'
		|| /unknown command|invalid command|not found|no such command|unrecognized command|unknown subcommand|unsupported command/.test(message);
}

function wpCodeboxRunAgentTaskInvocation(request) {
	const taskInput = codeboxTaskRequestFromAgentTaskRequest(request);
	return codeboxRunAgentTaskInvocation({
		taskInput,
		taskId: request.task_id,
	});
}

function wpCodeboxInvocationArgs(invocation, inputFile) {
	return invocation.args.flatMap((arg) => {
		if (arg === '--input-file={{input_file}}') {
			return ['--input-file', inputFile];
		}
		return [String(arg).replace('{{input_file}}', inputFile)];
	});
}

function spawnJson(command, args, options) {
	return new Promise((resolve, reject) => {
		const child = spawn(command, args, {
			cwd: options.cwd,
			env: options.env,
			stdio: ['ignore', 'pipe', 'pipe'],
		});
		let stdout = '';
		let stderr = '';
		child.stdout.setEncoding('utf8');
		child.stderr.setEncoding('utf8');
		child.stdout.on('data', (chunk) => { stdout += chunk; });
		child.stderr.on('data', (chunk) => { stderr += chunk; });
		child.on('error', reject);
		child.on('close', (code) => {
			const parsed = parseJsonOutput(stdout);
			if (code !== 0) {
				const details = [stderr.trim(), stdout.trim()].filter(Boolean).join('\n');
				const message = parsed?.error?.message || details || `${command} exited with ${code}`;
				const error = new Error(message);
				error.exitCode = code;
				error.stderr = stderr;
				error.stdout = stdout;
				reject(error);
				return;
			}
			resolve(parsed || { stdout, stderr, status: code });
		});
	});
}

function parseJsonOutput(stdout) {
	const text = String(stdout || '').trim();
	if (!text) {
		return null;
	}
	try {
		return JSON.parse(text);
	} catch {
		const start = text.indexOf('{');
		const end = text.lastIndexOf('}');
		if (start >= 0 && end > start) {
			return JSON.parse(text.slice(start, end + 1));
		}
		return null;
	}
}

function normalizeWpCodeboxAgentTaskOutput(output, request) {
	const candidates = [
		output?.json,
		output?.result,
		output?.output,
		output?.agent_task_run_result,
		output?.agent_runtime?.result,
		output?.recipe_run?.result,
		output,
	];

	for (const candidate of candidates) {
		const fuzzResult = findFuzzSuiteResult(candidate);
		if (fuzzResult) {
			return fuzzResult;
		}
	}

	return {
		schema: 'wp-codebox/fuzz-suite-result/v1',
		request_id: request.task_id,
		status: output?.success === false ? 'failed' : 'skipped',
		diagnostics: [{
			severity: 'error',
			code: 'wp_codebox_fuzz_suite_result_missing',
			message: 'WP Codebox completed without returning a wp-codebox/fuzz-suite-result/v1 envelope.',
		}],
		metadata: { output },
	};
}

function findFuzzSuiteResult(value) {
	if (!value || typeof value !== 'object') {
		return null;
	}
	if (value.schema === 'wp-codebox/fuzz-suite-result/v1') {
		return value;
	}
	for (const key of ['result', 'output', 'json', 'raw', 'agent_task_run_result', 'agentTaskRunResult', 'agent_task_result', 'agentTaskResult', 'agent_result', 'agentResult', 'agent_runtime', 'agentRuntime']) {
		const nested = findFuzzSuiteResult(value[key]);
		if (nested) {
			return nested;
		}
	}
	for (const item of Array.isArray(value.executions) ? value.executions : []) {
		for (const stream of ['stdout', 'stderr']) {
			const parsed = parseJsonOutput(item?.[stream]);
			const nested = findFuzzSuiteResult(parsed);
			if (nested) {
				return nested;
			}
		}
	}
	return null;
}
