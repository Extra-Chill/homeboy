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
	buildWordPressFuzzRunnerResult,
	readWordPressFuzzRunnerEnv,
	runWordPressFuzzRunnerResult,
	writeHomeboyFuzzArtifactFiles,
	writeHomeboyFuzzResultsFile,
} = require('../../lib/wordpress-fuzz-runner');
const {
	publicFuzzCliRunnerModeForRequest,
	wpCodeboxFuzzSuiteAbility,
	wpCodeboxFuzzSuiteCommand,
	wpCodeboxRuntimeContractManifest,
	wpCodeboxWordPressWorkloadRunAbility,
	wpCodeboxWordPressWorkloadRunCommand,
} = require('../../lib/wp-codebox-fuzz-run');

const WP_CODEBOX_FUZZ_EXECUTION_SCHEMA = 'homeboy/wp-codebox-fuzz-execution/v1';
if (require.main === module) {
	main().catch((error) => {
		process.stderr.write(`${error.message}\n`);
		process.exit(1);
	});
}

async function main() {
	const env = readWordPressFuzzRunnerEnv();
	const result = await buildRunnerResult(env);
	writeHomeboyFuzzResultsFile(env.resultsFile, result.homeboy_fuzz_campaign);
	writeHomeboyFuzzArtifactFiles(env.artifactRoot, result);
	process.stdout.write(`${JSON.stringify(fuzzRunnerStdoutSummary(result), null, 2)}\n`);
	if (!result.succeeded) {
		process.exitCode = 1;
	}
}

function fuzzRunnerStdoutSummary(result = {}) {
	return {
		schema: result.schema,
		run_id: result.run_id,
		status: result.status,
		succeeded: result.succeeded,
		result_schema: result.result_schema,
		wp_codebox_input: result.wp_codebox_input ? {
			schema: result.wp_codebox_input.schema,
			metadata: result.wp_codebox_input.metadata,
		} : undefined,
		wp_codebox_task_request: result.wp_codebox_task_request ? {
			executor: {
				config: {
					runtime_task: {
						ability: result.wp_codebox_task_request.executor?.config?.runtime_task?.ability,
						input: {
							schema: result.wp_codebox_task_request.executor?.config?.runtime_task?.input?.schema,
						},
					},
				},
			},
		} : undefined,
		wp_codebox_result: result.wp_codebox_result ? {
			request_id: result.wp_codebox_result.request_id,
			status: result.wp_codebox_result.status,
			result_schema: result.wp_codebox_result.result_schema,
			summary: result.wp_codebox_result.summary,
			wordpress_fuzz_result: result.wp_codebox_result.wordpress_fuzz_result ? {
				schema: result.wp_codebox_result.wordpress_fuzz_result.schema,
			} : undefined,
		} : undefined,
		homeboy_fuzz_campaign: result.homeboy_fuzz_campaign ? {
			schema: result.homeboy_fuzz_campaign.schema,
			id: result.homeboy_fuzz_campaign.id,
			artifacts: result.homeboy_fuzz_campaign.artifacts,
			metadata: {
				status: result.homeboy_fuzz_campaign.metadata?.status,
				success: result.homeboy_fuzz_campaign.metadata?.success,
				wp_codebox_result_schema: result.homeboy_fuzz_campaign.metadata?.wp_codebox_result_schema,
				artifact_refs: result.homeboy_fuzz_campaign.metadata?.artifact_refs,
			},
		} : undefined,
	};
}

async function buildRunnerResult(env) {
	if (process.env.HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH === '0') {
		return buildWordPressFuzzRunnerResult({ env });
	}

	if (process.env.HOMEBOY_WP_CODEBOX_FUZZ_DISPATCH === 'legacy-codebox-bin') {
		return runWordPressFuzzRunnerResult({
			env,
			runRuntimeTask: runWpCodeboxAgentTask,
		});
	}

	return runWordPressFuzzRunnerResult({ env });
}

async function runWpCodeboxAgentTask(request) {
	const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'homeboy-wp-codebox-fuzz-'));
	const env = wpCodeboxRuntimeEnv(process.env);
	const command = wpCodeboxRuntimeCommand(env);
	const manifest = await discoverRuntimeContractManifest(env);
	const publicInvocation = wpCodeboxPublicRuntimeInvocation(request, { runtimeContractManifest: manifest });

	if (publicInvocation) {
		const publicResult = await runWpCodeboxPublicRuntimeCommand(command, publicInvocation, tempDir, { env });
		return { json: normalizeWpCodeboxAgentTaskOutput(publicResult, request) };
	}

	const inputFile = path.join(tempDir, 'agent-task-request.json');
	const invocation = wpCodeboxRunAgentTaskInvocation(request);
	fs.writeFileSync(inputFile, `${JSON.stringify(invocation.input, null, 2)}\n`);

	const args = wpCodeboxInvocationArgs(invocation, inputFile);
	const result = await spawnJson(command[0], [...command.slice(1), ...args], {
		cwd: process.cwd(),
		env,
	});

	return { json: normalizeWpCodeboxAgentTaskOutput(result, request) };
}

async function discoverRuntimeContractManifest() {
	return wpCodeboxRuntimeContractManifest();
}

function wpCodeboxPublicRuntimeInvocation(request, options = {}) {
	if (requiresCodeboxTaskAdapter(request)) {
		return null;
	}
	const runtimeTask = request.schema === WP_CODEBOX_FUZZ_EXECUTION_SCHEMA
		? { ability: request.ability, input: request.input }
		: request.executor?.config?.runtime_task || {};
	const ability = runtimeTask.ability || wpCodeboxFuzzSuiteAbility(options);
	const command = wpCodeboxCommandFromPublicAbility(ability, options);
	if (!command) {
		return null;
	}
	return {
		ability,
		command,
		runnerMode: publicFuzzCliRunnerModeForRequest(request),
		input: {
			...(runtimeTask.input || {}),
			metadata: {
				...(runtimeTask.input?.metadata || {}),
				runtime_requirements: request.runtime_requirements,
				homeboy_wp_codebox_fuzz_execution: request,
			},
		},
	};
}

function requiresCodeboxTaskAdapter(request) {
	if (request.schema === WP_CODEBOX_FUZZ_EXECUTION_SCHEMA) {
		return false;
	}
	const config = request.executor?.config || {};
	const runtimeRequirements = config.runtime_requirements || {};
	return [
		runtimeRequirements.extra_plugins,
		runtimeRequirements.runtime_mounts,
	].some((value) => Array.isArray(value) && value.length > 0);
}

function wpCodeboxCommandFromPublicAbility(ability, options = {}) {
	const contracts = wpCodeboxRuntimeContractManifest(options)?.abilities?.wordpressRuntime || {};
	if (ability === contracts.runFuzzSuite || ability === wpCodeboxFuzzSuiteAbility(options)) {
		return wpCodeboxFuzzSuiteCommand(options) || '';
	}
	if (ability === contracts.runWorkload || ability === wpCodeboxWordPressWorkloadRunAbility(options)) {
		return wpCodeboxWordPressWorkloadRunCommand(options) || '';
	}
	return '';
}

async function runWpCodeboxPublicRuntimeCommand(command, invocation, tempDir, options = {}) {
	const inputFile = path.join(tempDir, `${invocation.command}-request.json`);
	fs.writeFileSync(inputFile, `${JSON.stringify(invocation.input, null, 2)}\n`);
	return spawnJson(command[0], [...command.slice(1), ...wpCodeboxPublicRuntimeArgs(invocation, inputFile)], {
		cwd: process.cwd(),
		env: options.env || process.env,
	});
}

function wpCodeboxPublicRuntimeArgs(invocation, inputFile) {
	if (invocation.command === 'run-fuzz-suite' && invocation.runnerMode) {
		return [invocation.command, `--runner-mode=${invocation.runnerMode}`, '--input-file', inputFile, '--json'];
	}
	return [invocation.command, '--input-file', inputFile, '--format=json'];
}

function wpCodeboxRunAgentTaskInvocation(request) {
	const {
		codeboxRunAgentTaskInvocation,
		codeboxTaskRequestFromAgentTaskRequest,
	} = requireWpCodeboxRuntime();
	const taskInput = codeboxTaskRequestFromAgentTaskRequest(request);
	return codeboxRunAgentTaskInvocation({
		taskInput,
		taskId: request.task_id,
	});
}

function requireWpCodeboxRuntime(options = {}) {
	return require(resolveWpCodeboxRuntimePath(options));
}

function resolveWpCodeboxRuntimePath(options = {}) {
	const env = options.env || process.env;
	const candidates = [];
	if (env.HOMEBOY_EXTENSION_PATH) {
		candidates.push(path.resolve(env.HOMEBOY_EXTENSION_PATH, '..', '..', 'agent-runtimes', 'wp-codebox'));
	}
	candidates.push(path.resolve(__dirname, '..', '..', '..', 'agent-runtimes', 'wp-codebox'));

	for (const candidate of candidates) {
		if (fs.existsSync(path.join(candidate, 'index.js'))) {
			return candidate;
		}
	}

	return candidates[0];
}

function wpCodeboxRuntimeEnv(env) {
	const nextEnv = { ...env };
	const manifestDefaults = installedExtensionSettingDefaults(nextEnv);
	if (!nextEnv.HOMEBOY_SETTINGS_WP_CODEBOX_BIN) {
		const settings = parseJsonObject(nextEnv.HOMEBOY_SETTINGS_JSON);
		nextEnv.HOMEBOY_SETTINGS_WP_CODEBOX_BIN = settings?.wp_codebox_bin || manifestDefaults.wp_codebox_bin || '';
	}
	if (!nextEnv.HOMEBOY_WP_CODEBOX_CORE_MODULE) {
		const settings = parseJsonObject(nextEnv.HOMEBOY_SETTINGS_JSON);
		if (settings?.wp_codebox_core_module) {
			nextEnv.HOMEBOY_WP_CODEBOX_CORE_MODULE = String(settings.wp_codebox_core_module);
		}
	}
	if (!nextEnv.HOMEBOY_WP_CODEBOX_CORE_MODULE && nextEnv.HOMEBOY_SETTINGS_WP_CODEBOX_CORE_MODULE) {
		nextEnv.HOMEBOY_WP_CODEBOX_CORE_MODULE = String(nextEnv.HOMEBOY_SETTINGS_WP_CODEBOX_CORE_MODULE);
	}
	if (!nextEnv.HOMEBOY_WP_CODEBOX_CORE_MODULE && manifestDefaults.wp_codebox_core_module) {
		nextEnv.HOMEBOY_WP_CODEBOX_CORE_MODULE = String(manifestDefaults.wp_codebox_core_module);
	}
	if (!nextEnv.HOMEBOY_WP_CODEBOX_CORE_MODULE) {
		const discoveredCoreModule = discoverWpCodeboxCoreModule(nextEnv);
		if (discoveredCoreModule) {
			nextEnv.HOMEBOY_WP_CODEBOX_CORE_MODULE = discoveredCoreModule;
		}
	}
	return nextEnv;
}

function discoverWpCodeboxCoreModule(env) {
	const installRoot = env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.join(os.homedir(), '.cache', 'homeboy', 'wp-codebox');
	for (const candidate of [
		path.join(installRoot, 'source', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'contracts.js'),
		path.join(installRoot, 'source', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'index.js'),
		path.join(installRoot, 'release', 'wp-codebox-cli', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'contracts.js'),
		path.join(installRoot, 'release', 'wp-codebox-cli', 'node_modules', '@automattic', 'wp-codebox-core', 'dist', 'index.js'),
	]) {
		if (fs.existsSync(candidate)) {
			return candidate;
		}
	}
	return '';
}

function wpCodeboxRuntimeCommand(env) {
	const {
		preflightWpCodeboxCommand,
		preflightWpCodeboxRuntime,
		wpCodeboxCommand,
	} = require(path.join(resolveWpCodeboxRuntimePath({ env }), 'lib', 'wp-codebox-runtime-selection.js'));
	const runtimePreflight = preflightWpCodeboxRuntime({ env });
	if (!runtimePreflight.ready) {
		throw wpCodeboxPreflightError(runtimePreflight);
	}
	const invocation = wpCodeboxCommand(runtimePreflight.selected.path);
	const command = [invocation.command, ...invocation.args];
	const preflight = preflightWpCodeboxCommand(command, { env });
	if (!preflight.ready) {
		throw wpCodeboxPreflightError(preflight);
	}
	return command;
}

function wpCodeboxPreflightError(preflight) {
	return new Error(`WP Codebox ${preflight.reason}: required >=${preflight.required_version}, observed ${preflight.selected.version || 'unavailable'} at ${preflight.selected.path || 'no executable'}. Run ${preflight.remediation}.`);
}

function installedExtensionSettingDefaults(env) {
	const manifest = readInstalledExtensionManifest(env);
	const settings = manifest?.settings;
	const defaults = {};
	if (Array.isArray(settings)) {
		for (const setting of settings) {
			if (!setting || typeof setting !== 'object' || typeof setting.id !== 'string' || setting.default === undefined || setting.default === '') {
				continue;
			}
			defaults[setting.id] = setting.default;
		}
	} else if (settings && typeof settings === 'object') {
		for (const [id, setting] of Object.entries(settings)) {
			const value = setting && typeof setting === 'object' && Object.hasOwn(setting, 'default') ? setting.default : undefined;
			if (value !== undefined && value !== '') {
				defaults[id] = value;
			}
		}
	}
	// Machine-scoped overrides persisted by setup into the untracked cache
	// install root (not the tracked manifest) carry the same precedence the
	// manifest default used to provide.
	for (const [id, value] of Object.entries(readMachineOverrides(env))) {
		if (value) {
			defaults[id] = value;
		}
	}
	return defaults;
}

function readMachineOverrides(env) {
	const installRoot = env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.join(os.homedir(), '.cache', 'homeboy', 'wp-codebox');
	const overrideFile = path.join(installRoot, 'wp-codebox-overrides.json');
	try {
		const parsed = JSON.parse(fs.readFileSync(overrideFile, 'utf8'));
		if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
			return {
				wp_codebox_bin: typeof parsed.wp_codebox_bin === 'string' ? parsed.wp_codebox_bin : '',
				wp_codebox_core_module: typeof parsed.wp_codebox_core_module === 'string' ? parsed.wp_codebox_core_module : '',
			};
		}
	} catch {
		// Missing or malformed machine override files contribute nothing.
	}
	return {};
}

function readInstalledExtensionManifest(env) {
	const candidates = [
		env.HOMEBOY_EXTENSION_MANIFEST_PATH,
		env.HOMEBOY_EXTENSION_PATH && path.resolve(env.HOMEBOY_EXTENSION_PATH, 'wordpress.json'),
		path.resolve(__dirname, '..', '..', 'wordpress.json'),
	];
	for (const candidate of candidates.filter(Boolean)) {
		try {
			const parsed = JSON.parse(fs.readFileSync(path.resolve(candidate), 'utf8'));
			if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
				return parsed;
			}
		} catch {
			// Missing or invalid manifests do not contribute defaults.
		}
	}
	return {};
}

function parseJsonObject(value) {
	if (!value) {
		return null;
	}
	try {
		const parsed = JSON.parse(value);
		return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : null;
	} catch {
		return null;
	}
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

module.exports = {
	resolveWpCodeboxRuntimePath,
	wpCodeboxRuntimeEnv,
	wpCodeboxRuntimeCommand,
	installedExtensionSettingDefaults,
};
