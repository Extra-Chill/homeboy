'use strict';

const fs = require('node:fs');
const path = require('node:path');

const DEFAULT_RUNTIME_ID = 'wp-codebox';

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
		manifests[manifest.id] = manifest;
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
		.filter((entry) => entry.isDirectory())
		.flatMap((entry) => {
			const runtimeDir = path.join(runtimesRoot, entry.name);
			try {
				return fs.readdirSync(runtimeDir, { withFileTypes: true })
					.filter((candidate) => candidate.isFile() && candidate.name.endsWith('.json'))
					.map((candidate) => path.join(runtimeDir, candidate.name));
			} catch {
				return [];
			}
		});
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

function resolveRuntimeProvider(runtimeId = DEFAULT_RUNTIME_ID, options = {}) {
	const id = runtimeId || DEFAULT_RUNTIME_ID;
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
		paths: resolvePaths(materialization.paths || {}, workspace),
		executor: resolveExecutor(manifest, options.repoRoot || repoRootFromHere()),
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

function resolvePaths(paths, workspace) {
	return Object.fromEntries(Object.entries(paths).map(([key, value]) => [
		key,
		typeof value === 'string' && value.length > 0 ? path.join(workspace, value) : value,
	]));
}

function resolveExecutor(manifest, repoRoot) {
	const provider = Array.isArray(manifest.agent_task_executors) ? manifest.agent_task_executors[0] : null;
	const scriptArg = executorScriptArg(provider);
	return {
		backend: provider?.backend || '',
		path: scriptArg ? scriptArg.replace('{{runtime_path}}', path.join(repoRoot, 'agent-runtimes', manifest.id)) : '',
	};
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
	resolveRuntimeProvider,
	runtimeRegistry,
};
