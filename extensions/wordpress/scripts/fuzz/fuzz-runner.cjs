#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');

/**
 * Internal dependencies
 */
const {
	buildWordPressFuzzRunnerResult,
	readWordPressFuzzRunnerEnv,
	runWordPressFuzzRunnerResult,
	writeHomeboyFuzzResultsFile,
} = require('../../lib/wordpress-fuzz-runner');


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
	const inputFile = path.join(tempDir, 'agent-task-request.json');
	const command = process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN || 'wp-codebox';
	fs.writeFileSync(inputFile, `${JSON.stringify(wpCodeboxRunAgentTaskInput(request, { wpCodeboxBin: command }), null, 2)}\n`);

	const args = ['run-agent-task', '--input-file', inputFile, '--json'];
	const result = await spawnJson(command, args, {
		cwd: process.cwd(),
		env: process.env,
	});

	return { json: normalizeWpCodeboxAgentTaskOutput(result, request) };
}

function wpCodeboxRunAgentTaskInput(request, options = {}) {
	return {
		schema: 'wp-codebox/run-agent-task/v1',
		id: request.task_id,
		goal: request.instructions || 'Run the WordPress fuzz suite and return the declared fuzz artifacts.',
		agent_workload: {
			schema: 'wp-codebox/agent-workload/v1',
			id: request.task_id,
			goal: request.instructions || 'Run the WordPress fuzz suite and return the declared fuzz artifacts.',
			agent_runtime: {
				runtime_task: request.executor?.config?.runtime_task,
			},
		},
		runtime_task: request.executor?.config?.runtime_task,
		extra_plugins: wpCodeboxRuntimePlugins(options.wpCodeboxBin),
		allowed_tools: ['homeboy/no-runtime-tools'],
		sandbox_tool_policy: denyAllSandboxToolPolicy(),
		artifact_declarations: request.artifact_declarations,
		expected_artifacts: request.expected_artifacts,
		metadata: {
			...(request.metadata || {}),
			homeboy_agent_task_request: request,
		},
	};
}

function wpCodeboxRuntimePlugins(wpCodeboxBin) {
	const source = wpCodeboxPluginSource(wpCodeboxBin);
	if (!source) {
		return [];
	}
	return [{
		source,
		slug: 'wp-codebox',
		pluginFile: 'wp-codebox/wp-codebox.php',
		activate: true,
		loadAs: 'plugin',
	}];
}

function wpCodeboxPluginSource(wpCodeboxBin) {
	const explicit = process.env.HOMEBOY_WP_CODEBOX_PLUGIN_PATH || process.env.WP_CODEBOX_PLUGIN_PATH;
	if (explicit && fs.existsSync(explicit)) {
		return explicit;
	}
	if (!wpCodeboxBin || wpCodeboxBin === 'wp-codebox') {
		return '';
	}
	const candidate = path.resolve(path.dirname(wpCodeboxBin), '../../wordpress-plugin');
	return fs.existsSync(candidate) ? candidate : '';
}

function denyAllSandboxToolPolicy() {
	return {
		schema: 'wp-codebox/sandbox-tool-policy/v1',
		version: 1,
		tools: [
			{
				id: 'homeboy/no-runtime-tools',
				runtime_tool_id: 'homeboy_no_runtime_tools',
				allowed: false,
				runtime: {
					environment: 'control_plane',
					capability_scope: 'control_plane',
				},
			},
		],
		metadata: {
			source: 'homeboy-extension-wordpress/fuzz-runner',
		},
	};
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
				reject(new Error(message));
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
	for (const key of ['result', 'output', 'json']) {
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
