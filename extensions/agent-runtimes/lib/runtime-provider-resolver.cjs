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
	const manifests = {
		[DEFAULT_RUNTIME_ID]: readJson(runtimeManifestPath(DEFAULT_RUNTIME_ID, repoRoot)),
	};
	return manifests;
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
	const argv = provider?.invocation?.argv || [];
	const scriptArg = argv.find((arg) => typeof arg === 'string' && arg.includes('{{runtime_path}}')) || '';
	return {
		backend: provider?.backend || '',
		path: scriptArg ? scriptArg.replace('{{runtime_path}}', path.join(repoRoot, 'agent-runtimes', manifest.id)) : '',
	};
}

module.exports = {
	DEFAULT_RUNTIME_ID,
	resolveRuntimeProvider,
	runtimeRegistry,
};
