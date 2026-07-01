'use strict';

const fs = require('node:fs');
const path = require('node:path');
const childProcess = require('node:child_process');

const DEFAULT_RUNTIME_ID = 'local-shell';
const RUNTIME_SOURCE_METADATA = Symbol('homeboy.runtimeSourceMetadata');

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
		const manifest = loadRuntimeManifest(manifestPath, {
			kind: 'repo',
			source_path: path.dirname(manifestPath),
		});
		if (!manifest) {
			continue;
		}
		if (!manifests[manifest.id]) {
			manifests[manifest.id] = manifest;
		}
	}
	for (const manifestPath of explicitRuntimeManifestPaths(options)) {
		const manifest = loadRuntimeManifest(manifestPath, {
			kind: 'manifest',
			source_path: path.dirname(manifestPath),
		});
		if (!manifest) {
			continue;
		}
		if (!manifests[manifest.id]) {
			manifests[manifest.id] = manifest;
		}
	}
	for (const runtimePackage of explicitRuntimePackages(options)) {
		const manifest = loadRuntimePackageManifest(runtimePackage, options);
		if (!manifests[manifest.id]) {
			manifests[manifest.id] = manifest;
		}
	}
	return manifests;
}

function loadRuntimeManifest(manifestPath, source = {}) {
	const resolvedManifestPath = path.resolve(manifestPath);
	let manifest;
	try {
		manifest = readJson(resolvedManifestPath);
	} catch {
		return null;
	}
	if (!isRuntimeManifest(manifest)) {
		return null;
	}
	return attachRuntimeSourceMetadata(manifest, {
		...source,
		manifest_path: resolvedManifestPath,
		source_path: source.source_path ? path.resolve(source.source_path) : path.dirname(resolvedManifestPath),
		...gitSourceRevision(source.source_path || path.dirname(resolvedManifestPath)),
	});
}

function loadRuntimePackageManifest(runtimePackage, options = {}) {
	const spec = typeof runtimePackage === 'string' ? runtimePackage : runtimePackage?.package || runtimePackage?.name;
	if (!spec || typeof spec !== 'string') {
		throw new Error('Runtime package entries require a package name or specifier.');
	}
	const packageRoot = resolveRuntimePackageRoot(spec, options);
	const packageJsonPath = path.join(packageRoot, 'package.json');
	const packageJson = readJson(packageJsonPath);
	const manifestRelativePath = firstString(
		typeof runtimePackage === 'object' ? runtimePackage.manifest : '',
		packageJson.homeboy?.agent_runtime_manifest,
		packageJson.homeboy?.runtime_manifest,
		packageJson.agent_runtime_manifest,
		packageJson.runtime_manifest,
		'agent-runtime.json',
		'runtime.json'
	);
	const manifestPath = path.resolve(packageRoot, manifestRelativePath);
	const manifest = loadRuntimeManifest(manifestPath, {
		kind: 'package',
		package: spec,
		source_path: packageRoot,
	});
	if (!manifest) {
		throw new Error(`Runtime package ${spec} does not expose a valid Homeboy agent runtime manifest at ${manifestPath}. Add package.json homeboy.agent_runtime_manifest or pass an explicit manifest path.`);
	}
	return manifest;
}

function resolveRuntimePackageRoot(spec, options = {}) {
	const basePaths = runtimePackageBasePaths(options);
	try {
		return path.dirname(require.resolve(`${spec}/package.json`, { paths: basePaths }));
	} catch (error) {
		throw new Error(`Runtime package ${spec} could not be resolved from ${basePaths.join(', ')}. Install the package or pass runtimeManifests/runtimeManifestPath for local iteration. Original error: ${error.message}`);
	}
}

function explicitRuntimeManifestPaths(options = {}) {
	return normalizeStringList(
		options.runtimeManifests,
		options.runtime_manifest_paths,
		options.runtimeManifestPaths,
		options.runtimeManifest,
		options.runtime_manifest,
		options.runtimeManifestPath,
		options.runtime_manifest_path,
		options.env?.AGENT_RUNTIME_MANIFESTS,
		options.env?.AGENT_RUNTIME_MANIFEST,
		process.env.AGENT_RUNTIME_MANIFESTS,
		process.env.AGENT_RUNTIME_MANIFEST
	).map((manifestPath) => path.resolve(manifestPath));
}

function explicitRuntimePackages(options = {}) {
	return [
		...normalizeList(options.runtimePackages, options.runtime_packages, options.runtimePackage, options.runtime_package),
		...normalizeStringList(options.env?.AGENT_RUNTIME_PACKAGES, options.env?.AGENT_RUNTIME_PACKAGE, process.env.AGENT_RUNTIME_PACKAGES, process.env.AGENT_RUNTIME_PACKAGE),
	];
}

function runtimePackageBasePaths(options = {}) {
	return normalizeStringList(options.packageBasePaths, options.package_base_paths, options.packageBasePath, options.package_base_path, process.cwd());
}

function normalizeStringList(...values) {
	return normalizeList(...values).flatMap((value) => typeof value === 'string' ? value.split(path.delimiter).map((entry) => entry.trim()).filter(Boolean) : []);
}

function normalizeList(...values) {
	const entries = [];
	for (const value of values) {
		if (Array.isArray(value)) {
			entries.push(...value.filter(Boolean));
			continue;
		}
		if (value) {
			entries.push(value);
		}
	}
	return entries;
}

function attachRuntimeSourceMetadata(manifest, source) {
	Object.defineProperty(manifest, RUNTIME_SOURCE_METADATA, {
		value: normalizeRuntimeSource(source),
		enumerable: false,
		configurable: true,
	});
	return manifest;
}

function runtimeSourceMetadata(manifest, fallback = {}) {
	return normalizeRuntimeSource(manifest?.[RUNTIME_SOURCE_METADATA] || fallback);
}

function normalizeRuntimeSource(source = {}) {
	const sourcePath = firstString(source.source_path, source.path);
	return {
		kind: firstString(source.kind, 'registry'),
		...(source.package ? { package: source.package } : {}),
		...(sourcePath ? { source_path: sourcePath } : {}),
		...(source.manifest_path ? { manifest_path: source.manifest_path } : {}),
		...(source.revision ? { revision: source.revision } : {}),
		...(source.ref ? { ref: source.ref } : {}),
	};
}

function gitSourceRevision(sourcePath) {
	try {
		const revision = childProcess.execFileSync('git', ['-C', sourcePath, 'rev-parse', 'HEAD'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
		const ref = childProcess.execFileSync('git', ['-C', sourcePath, 'rev-parse', '--abbrev-ref', 'HEAD'], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
		return {
			...(revision ? { revision } : {}),
			...(ref && ref !== 'HEAD' ? { ref } : {}),
		};
	} catch {
		return {};
	}
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

function normalizeRuntimeId(runtimeId = DEFAULT_RUNTIME_ID, options = {}) {
	return runtimeId || DEFAULT_RUNTIME_ID;
}

function runtimeIdFromOptions(options = {}, env = process.env) {
	// Mirror the env precedence used by setup-runtime.cjs
	// (RUNTIME || RUNTIME_PROVIDER || BACKEND). Consumers commonly set only
	// RUNTIME_PROVIDER; without honoring it here, materialize-dependencies
	// resolves DEFAULT_RUNTIME_ID instead of the real runtime and never checks
	// out the runtime provider repo, so its setup_commands later fail on a
	// missing cwd.
	return firstString(
		options.runtimeId,
		options.runtime_id,
		options.runtime,
		env.RUNTIME_ID,
		env.RUNTIME,
		env.RUNTIME_PROVIDER,
		env.BACKEND,
		DEFAULT_RUNTIME_ID
	);
}

function resolveRuntimeProvider(runtimeId = DEFAULT_RUNTIME_ID, options = {}) {
	const requestedId = runtimeId || DEFAULT_RUNTIME_ID;
	const registry = options.registry || runtimeRegistry(options);
	const id = normalizeRuntimeId(runtimeId, { ...options, registry });
	const manifest = registry[id];
	if (!manifest) {
		throw new Error(`Unsupported agent_runtime: ${id}. Registered runtimes: ${Object.keys(registry).sort().join(', ') || '(none)'}.`);
	}
	const source = runtimeSourceMetadata(manifest, {
		kind: 'registry',
		source_path: path.join(options.repoRoot || repoRootFromHere(), 'agent-runtimes', id),
		manifest_path: runtimeManifestPath(id, options.repoRoot || repoRootFromHere()),
	});

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
		requested_id: requestedId,
		source,
		manifest,
		checkout,
		setupCommands: normalizeCommands(materialization.setup_commands || []),
		buildCommands: normalizeCommands(materialization.build_commands || []),
		paths: resolvePaths(materialization.paths || {}, workspace, options.env || process.env),
		executor: resolveExecutor(manifest, source, options),
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
	const settings = homeboySettings(env);
	return Object.fromEntries(Object.entries(paths).map(([key, value]) => [
		key,
		resolvePathValue(value, workspace, env, settings),
	]));
}

function resolvePathValue(value, workspace, env = process.env, settings = homeboySettings(env)) {
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
		for (const settingName of Array.isArray(value.settings) ? value.settings : []) {
			if (typeof settingName === 'string' && typeof settings[settingName] === 'string' && settings[settingName].trim() !== '') {
				return settings[settingName].trim();
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
		for (const settingName of Array.isArray(value.settings) ? value.settings : []) {
			if (typeof settingName === 'string' && typeof settings[settingName] === 'string' && settings[settingName].trim() !== '') {
				const settingPath = settings[settingName].trim();
				return settingPath.length > 0 && isWorkspaceRelativePath(settingPath) ? path.join(workspace, settingPath) : settingPath;
			}
		}
		const defaultPath = value.default || value.path || '';
		return defaultPath.length > 0 && isWorkspaceRelativePath(defaultPath) ? path.join(workspace, defaultPath) : defaultPath;
	}
	return value.value || value.path || '';
}

function homeboySettings(env = process.env) {
	try {
		const settings = env.HOMEBOY_SETTINGS_JSON ? JSON.parse(env.HOMEBOY_SETTINGS_JSON) : {};
		return settings && typeof settings === 'object' && !Array.isArray(settings) ? settings : {};
	} catch {
		return {};
	}
}

function isWorkspaceRelativePath(value) {
	return !path.isAbsolute(value) && !value.startsWith('@') && (value.startsWith('.') || value.includes('/') || value.includes('\\'));
}

function resolveExecutor(manifest, source, options = {}) {
	const provider = selectExecutor(manifest, executorSelectionFromOptions(options));
	const runtimePath = source.source_path || path.join(options.repoRoot || repoRootFromHere(), 'agent-runtimes', manifest.id);
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
		runtime_execution_contracts: provider?.runtime_execution_contracts || {},
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
		env_allowlist: normalizeStringArray(invocation.env_allowlist || invocation.envAllowlist),
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

function normalizeStringArray(value) {
	return Array.isArray(value) ? value.filter((entry) => typeof entry === 'string' && entry.trim() !== '') : [];
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
	runtimeIdFromOptions,
	resolveRuntimeProvider,
	runtimeManifestPath,
	runtimeRegistry,
};
