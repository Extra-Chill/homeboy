'use strict';

const fs = require('node:fs');
const path = require('node:path');

const DEFAULT_RUNTIME_ID = 'local-shell';
const RUNTIME_ID_ALIASES = {
	codebox: 'wp-codebox',
};

function repoRootFromHere() {
	return path.resolve(__dirname, '..', '..');
}

function readJson(filePath) {
	return JSON.parse(fs.readFileSync(filePath, 'utf8'));
}

function runtimeManifestPath(runtimeId, repoRoot = repoRootFromHere()) {
	return path.join(repoRoot, 'agent-runtimes', runtimeId, `${runtimeId}.json`);
}

function runtimeRegistry(options = {}) {
	const repoRoot = options.repoRoot || repoRootFromHere();
	const manifests = {};
	const runtimesRoot = path.join(repoRoot, 'agent-runtimes');
	for (const manifestPath of runtimeManifestPaths(runtimesRoot)) {
		let manifest;
		try {
			manifest = readJson(manifestPath);
		} catch {
			continue;
		}
		if (!isRuntimeManifest(manifest)) {
			continue;
		}
		if (!manifests[manifest.id]) {
			manifests[manifest.id] = manifest;
		}
	}
	return manifests;
}

function runtimeManifestPaths(runtimesRoot) {
	let entries;
	try {
		entries = fs.readdirSync(runtimesRoot, { withFileTypes: true });
	} catch {
		return [];
	}
	return entries
		.sort((a, b) => a.name.localeCompare(b.name))
		.filter((entry) => entry.isDirectory())
		.flatMap((entry) => {
			const runtimeDir = path.join(runtimesRoot, entry.name);
			try {
				return fs.readdirSync(runtimeDir, { withFileTypes: true })
					.filter((candidate) => candidate.isFile() && candidate.name.endsWith('.json'))
					.sort((a, b) => manifestFileSort(entry.name, a.name, b.name))
					.map((candidate) => path.join(runtimeDir, candidate.name));
			} catch {
				return [];
			}
		});
}

function manifestFileSort(runtimeId, a, b) {
	const exact = `${runtimeId}.json`;
	if (a === exact && b !== exact) {
		return -1;
	}
	if (b === exact && a !== exact) {
		return 1;
	}
	return a.localeCompare(b);
}

function isRuntimeManifest(manifest) {
	return Boolean(
		manifest &&
		typeof manifest === 'object' &&
		!Array.isArray(manifest) &&
		manifest.schema === 'homeboy/agent-runtime-manifest/v1' &&
		typeof manifest.id === 'string' &&
		manifest.id.trim() !== '' &&
		Array.isArray(manifest.agent_task_executors)
	);
}

function normalizeRuntimeId(runtimeId = DEFAULT_RUNTIME_ID) {
	const id = runtimeId || DEFAULT_RUNTIME_ID;
	return RUNTIME_ID_ALIASES[id] || id;
}

function resolveRuntimeProvider(runtimeId = DEFAULT_RUNTIME_ID, options = {}) {
	const id = normalizeRuntimeId(runtimeId);
	const registry = options.registry || runtimeRegistry(options);
	const manifest = registry[id];
	if (!manifest) {
		throw new Error(`Unsupported agent_runtime: ${id}. Registered runtimes: ${Object.keys(registry).sort().join(', ') || '(none)'}.`);
	}

	const materialization = manifest.ci_materialization || {};
	const workspace = options.workspace || process.env.GITHUB_WORKSPACE || process.cwd();
	const refEnv = materialization.checkout?.ref_env || 'AGENT_RUNTIME_REF';
	const checkoutTarget = materialization.checkout?.target || path.join('.ci', id);
	const checkout = {
		repo: materialization.checkout?.repo || '',
		ref: options.env?.[refEnv] || process.env[refEnv] || materialization.checkout?.default_ref || 'main',
		target: checkoutTarget,
		targetPath: path.join(workspace, checkoutTarget),
	};

	return {
		id,
		manifest,
		checkout,
		setupCommands: normalizeCommands(materialization.setup_commands || []),
		buildCommands: normalizeCommands(materialization.build_commands || []),
		paths: resolvePaths(materialization.paths || {}, workspace, options.env || process.env),
		executor: resolveExecutor(manifest, options.repoRoot || repoRootFromHere(), options),
	};
}

function normalizeCommands(commands) {
	if (!Array.isArray(commands)) {
		throw new Error('runtime ci_materialization commands must be an array');
	}
	return commands.map((command) => {
		if (!command || Array.isArray(command) || typeof command !== 'object') {
			throw new Error('runtime ci_materialization command entries must be objects');
		}
		if (!command.command || typeof command.command !== 'string') {
			throw new Error('runtime ci_materialization command entries require a command');
		}
		const args = command.args || [];
		if (!Array.isArray(args) || args.some((arg) => typeof arg !== 'string')) {
			throw new Error('runtime ci_materialization command args must be strings');
		}
		return {
			command: command.command,
			args,
			cwd: command.cwd || '.',
		};
	});
}

function resolvePaths(paths, workspace, env = process.env) {
	return Object.fromEntries(Object.entries(paths).map(([key, value]) => [
		key,
		resolvePathValue(value, workspace, env),
	]));
}

function resolvePathValue(value, workspace, env = process.env) {
	if (typeof value === 'string') {
		return value.length > 0 && isWorkspaceRelativePath(value) ? path.join(workspace, value) : value;
	}
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return value;
	}
	if (value.type === 'executable') {
		for (const envName of Array.isArray(value.env) ? value.env : []) {
			if (typeof envName === 'string' && env[envName]) {
				return env[envName];
			}
		}
		return value.default || value.command || '';
	}
	if (value.type === 'path') {
		for (const envName of Array.isArray(value.env) ? value.env : []) {
			if (typeof envName === 'string' && env[envName]) {
				const envPath = env[envName];
				return envPath.length > 0 && isWorkspaceRelativePath(envPath) ? path.join(workspace, envPath) : envPath;
			}
		}
		const defaultPath = value.default || value.path || '';
		return defaultPath.length > 0 && isWorkspaceRelativePath(defaultPath) ? path.join(workspace, defaultPath) : defaultPath;
	}
	return value.value || value.path || '';
}

function isWorkspaceRelativePath(value) {
	return value.startsWith('.') || value.includes('/') || value.includes('\\');
}

function resolveExecutor(manifest, repoRoot, options = {}) {
	const provider = selectExecutor(manifest, executorSelectionFromOptions(options));
	const runtimePath = path.join(repoRoot, 'agent-runtimes', manifest.id);
	const invocation = resolveExecutorInvocation(provider, runtimePath, options);
	const scriptArg = invocation.argv.find((arg) => typeof arg === 'string' && arg.includes(runtimePath)) || executorScriptArg(provider);
	return {
		id: provider.id || '',
		backend: provider?.backend || '',
		status: provider?.status || '',
		capabilities: Array.isArray(provider?.capabilities) ? provider.capabilities : [],
		path: scriptArg ? scriptArg.replace('{{runtime_path}}', runtimePath) : '',
		invocation,
		capabilities: Array.isArray(provider?.capabilities) ? provider.capabilities.filter(Boolean) : [],
		runtime_execution_contracts: provider?.runtime_execution_contracts || provider?.execution_contracts || {},
		provider_metadata: provider?.provider_metadata || {},
		provider_defaults: provider?.provider_defaults || {},
		secret_env_requirements: Array.isArray(provider?.secret_env_requirements) ? provider.secret_env_requirements : [],
	};
}

function resolveExecutorInvocation(provider, runtimePath, options = {}) {
	const invocation = objectOption(provider?.invocation) || {};
	const argv = normalizeInvocationArgv(provider, invocation).map((arg) => replaceRuntimePath(arg, runtimePath));
	const command = replaceRuntimePath(firstString(invocation.command, argv[0]), runtimePath);
	if (!command) {
		throw new Error(`agent_task_executor ${provider?.id || '(unknown)'} requires invocation.command or invocation.argv.`);
	}
	const cwd = replaceRuntimePath(firstString(invocation.cwd, options.cwd, process.cwd()), runtimePath);
	return {
		schema: invocation.schema || 'homeboy/command-invocation/v1',
		command,
		argv: argv.length > 0 ? argv.slice(1) : [],
		cwd,
		env: normalizeInvocationEnv(invocation.env || {}),
		stdin: invocation.stdin || 'request_json',
		stdout: invocation.stdout || 'outcome_json',
		stderr: invocation.stderr || 'inherit_on_failure',
		artifacts: invocation.artifacts || provider?.artifact_contract || {},
		results: invocation.results || {},
		display: invocation.display || [command, ...(argv.length > 0 ? argv.slice(1) : [])].join(' '),
	};
}

function normalizeInvocationArgv(provider, invocation) {
	if (Array.isArray(invocation.argv)) {
		return invocation.argv.filter((arg) => typeof arg === 'string');
	}
	if (Array.isArray(invocation.args)) {
		return [firstString(invocation.command), ...invocation.args.filter((arg) => typeof arg === 'string')].filter(Boolean);
	}
	if (typeof provider?.command === 'string' && provider.command.trim() !== '') {
		return provider.command.trim().split(/\s+/);
	}
	return [];
}

function normalizeInvocationEnv(env) {
	if (!env || typeof env !== 'object' || Array.isArray(env)) {
		return {};
	}
	return Object.fromEntries(Object.entries(env).filter(([, value]) => typeof value === 'string'));
}

function replaceRuntimePath(value, runtimePath) {
	return typeof value === 'string' ? value.replaceAll('{{runtime_path}}', runtimePath) : value;
}

function executorSelectionFromOptions(options = {}) {
	const env = options.env || process.env;
	const executor = objectOption(options.executor) || objectOption(options.executorSelection) || {};
	return {
		id: firstString(
			executor.id,
			executor.executor_id,
			options.executorId,
			options.executor_id,
			env.EXECUTOR_ID,
			env.AGENT_TASK_EXECUTOR_ID
		),
		backend: firstString(
			executor.backend,
			options.executorBackend,
			options.executor_backend,
			env.EXECUTOR_BACKEND,
			env.AGENT_TASK_EXECUTOR_BACKEND
		),
		capabilities: normalizeCapabilities(
			executor.capabilities,
			executor.capability,
			options.executorCapabilities,
			options.executor_capabilities,
			options.executorCapability,
			options.executor_capability,
			env.EXECUTOR_CAPABILITIES,
			env.EXECUTOR_CAPABILITY,
			env.AGENT_TASK_EXECUTOR_CAPABILITIES,
			env.AGENT_TASK_EXECUTOR_CAPABILITY
		),
		status: firstString(
			executor.status,
			options.executorStatus,
			options.executor_status,
			env.EXECUTOR_STATUS,
			env.AGENT_TASK_EXECUTOR_STATUS
		),
	};
}

function objectOption(value) {
	return value && typeof value === 'object' && !Array.isArray(value) ? value : null;
}

function firstString(...values) {
	for (const value of values) {
		if (typeof value === 'string' && value.trim() !== '') {
			return value.trim();
		}
	}
	return '';
}

function normalizeCapabilities(...values) {
	const capabilities = [];
	for (const value of values) {
		if (Array.isArray(value)) {
			capabilities.push(...value.filter((entry) => typeof entry === 'string'));
			continue;
		}
		if (typeof value === 'string' && value.trim() !== '') {
			capabilities.push(...value.split(',').map((entry) => entry.trim()).filter(Boolean));
		}
	}
	return [...new Set(capabilities)];
}

function selectExecutor(manifest, selection) {
	const providers = Array.isArray(manifest.agent_task_executors)
		? manifest.agent_task_executors.filter((provider) => provider && typeof provider === 'object' && !Array.isArray(provider))
		: [];
	if (providers.length === 0) {
		throw new Error(`Runtime ${manifest.id} does not declare any agent_task_executors.`);
	}

	const requestedStatus = selection.status || 'active';
	const candidates = providers.filter((provider) => {
		if (selection.id && provider.id !== selection.id) {
			return false;
		}
		if (selection.backend && provider.backend !== selection.backend) {
			return false;
		}
		if (requestedStatus && !executorStatusMatches(provider, requestedStatus)) {
			return false;
		}
		if (!selection.capabilities.every((capability) => Array.isArray(provider.capabilities) && provider.capabilities.includes(capability))) {
			return false;
		}
		return true;
	});

	if (candidates.length === 1) {
		return candidates[0];
	}

	const criteria = executorSelectionDescription({ ...selection, status: requestedStatus });
	if (candidates.length === 0) {
		throw new Error(`No agent_task_executors for runtime ${manifest.id} match ${criteria}. Available executors: ${executorList(providers)}.`);
	}
	throw new Error(`Ambiguous agent_task_executors for runtime ${manifest.id} match ${criteria}: ${executorList(candidates)}. Select executor.id, backend, capability, or status explicitly.`);
}

function executorStatusMatches(provider, requestedStatus) {
	if (requestedStatus === 'active' && !provider.status) {
		return true;
	}
	if (requestedStatus === 'active' && provider.status === 'available') {
		return true;
	}
	return provider.status === requestedStatus;
}

function executorSelectionDescription(selection) {
	const parts = [];
	if (selection.id) {
		parts.push(`id=${selection.id}`);
	}
	if (selection.backend) {
		parts.push(`backend=${selection.backend}`);
	}
	if (selection.capabilities.length > 0) {
		parts.push(`capabilities=${selection.capabilities.join(',')}`);
	}
	if (selection.status) {
		parts.push(`status=${selection.status}`);
	}
	return parts.join(' ') || 'the default active executor selection';
}

function executorList(providers) {
	return providers.map((provider) => {
		const id = provider.id || '(missing id)';
		const backend = provider.backend || '(missing backend)';
		const status = provider.status || '(no status)';
		return `${id} [backend=${backend}, status=${status}]`;
	}).join(', ');
}

function executorScriptArg(provider) {
	const argv = provider?.invocation?.argv || [];
	const invocationArg = argv.find((arg) => typeof arg === 'string' && arg.includes('{{runtime_path}}')) || '';
	if (invocationArg) {
		return invocationArg;
	}
	const command = provider?.command || '';
	return command.split(/\s+/).find((arg) => arg.includes('{{runtime_path}}')) || '';
}

module.exports = {
	DEFAULT_RUNTIME_ID,
	normalizeRuntimeId,
	resolveRuntimeProvider,
	runtimeManifestPath,
	runtimeRegistry,
};
