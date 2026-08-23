'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const {
  WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND,
} = require('./codebox-run-agent-task-contract');
const {
  WP_CODEBOX_RECIPE_RUN_CLI_COMMAND,
} = require('./wp-codebox-adapter-contract');

const WP_CODEBOX_CLI_DESCRIPTOR_SCHEMA = 'wp-codebox/cli-descriptor/v1';
const WP_CODEBOX_RUNTIME_PACKAGE_SOURCE_FIELDS = ['source', 'path', 'bundle_path', 'bundlePath'];

const DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR = {
  schema: WP_CODEBOX_CLI_DESCRIPTOR_SCHEMA,
  id: 'wp-codebox',
  env: ['HOMEBOY_WP_CODEBOX_BIN', 'WP_CODEBOX_BIN'],
  settings: ['runtime_bin', 'wp_codebox_bin', 'wpCodeboxBin'],
  executable: 'wp-codebox',
  commands: {
    run_agent_task: WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND,
    recipe_run: WP_CODEBOX_RECIPE_RUN_CLI_COMMAND,
  },
};

function wpCodeboxCliDescriptor(overrides = {}) {
  return {
    ...DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR,
    ...overrides,
    env: arrayOrDefault(overrides.env, DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR.env),
    settings: arrayOrDefault(overrides.settings, DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR.settings),
    commands: {
      ...DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR.commands,
      ...(overrides.commands || {}),
    },
  };
}

function wpCodeboxBin(options = {}) {
  const descriptor = wpCodeboxCliDescriptor(options.descriptor || options.cliDescriptor || {});
  const env = options.env || process.env;
  const settings = options.settings || homeboySettings(env);
  const explicitBinCandidates = [
    options.bin,
    options.runtimeBin,
    options.runtime_bin,
    options.wpCodeboxBin,
    options.wp_codebox_bin,
  ];
  const configuredCandidates = [
    ...explicitBinCandidates,
    ...descriptor.env.map((key) => env[key]),
    env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN,
    ...descriptor.settings.map((key) => settings[key]),
  ];
  // A configured value is an exact pin. Managed and PATH-like defaults only
  // participate when no caller configuration was supplied.
  const packagedRuntimeCandidates = [
    wpCodeboxBinFromRuntimeComponent(env),
    managedWpCodeboxBin(env),
  ];
  return firstValue(
    ...configuredCandidates,
    ...packagedRuntimeCandidates,
    options.executable === undefined ? descriptor.executable : options.executable,
  );
}

function wpCodeboxBinFromRuntimeComponent(env = process.env) {
  const component = firstValue(env.HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT, env.WP_CODEBOX_RUNTIME_COMPONENT);
  if (!component) {
    return '';
  }
  const resolved = path.resolve(component);
  const candidates = [
    path.resolve(resolved, '..', 'cli', 'dist', 'index.js'),
    path.resolve(resolved, '..', '..', 'packages', 'cli', 'dist', 'index.js'),
  ];
  return candidates.find((candidate) => isExecutableFile(candidate)) || '';
}

function managedWpCodeboxBin(env = process.env) {
  const installDir = env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.join(os.homedir(), '.cache', 'homeboy', 'wp-codebox');
  const candidate = path.join(installDir, 'source', 'packages', 'cli', 'dist', 'index.js');
  return isExecutableFile(candidate) ? candidate : '';
}

function isExecutableFile(filePath) {
  try {
    fs.accessSync(filePath, fs.constants.X_OK);
    return fs.statSync(filePath).isFile();
  } catch {
    return false;
  }
}

function wpCodeboxCommand(bin = wpCodeboxBin()) {
	if (/\.(?:js|cjs|mjs)$/.test(bin)) {
		return { command: process.execPath, args: [bin] };
	}
	return { command: bin, args: [] };
}

function wpCodeboxResolveCommand(bin = wpCodeboxBin(), args = []) {
	const command = wpCodeboxCommand(bin);
	return { command: command.command, args: [...command.args, ...args] };
}

function wpCodeboxBinaryDiagnostic(bin = wpCodeboxBin()) {
	if (!bin) {
		return {
			class: 'wp-codebox.config.missing_binary',
			message: 'WP Codebox binary is not configured. Set wp_codebox_bin or provide a wp-codebox executable through the public CLI descriptor.',
			data: { phase: 'codebox.config', wp_codebox_bin: '', reason: 'missing' },
		};
	}
	if (!path.isAbsolute(bin)) {
		return null;
	}
	if (!fs.existsSync(bin)) {
		return {
			class: 'wp-codebox.config.invalid_binary',
			message: `Configured WP Codebox binary does not exist: ${bin}`,
			data: { phase: 'codebox.config', wp_codebox_bin: bin, reason: 'missing' },
		};
	}
	if (/\.(?:js|cjs|mjs)$/.test(bin) || isExecutableFile(bin)) {
		return null;
	}
	return {
		class: 'wp-codebox.config.invalid_binary',
		message: `Configured WP Codebox binary is not executable: ${bin}`,
		data: { phase: 'codebox.config', wp_codebox_bin: bin, reason: 'not_executable' },
	};
}

function wpCodeboxSupportsRunAgentTaskCommand(options = {}) {
	const env = options.env || process.env;
	const mode = env.HOMEBOY_WP_CODEBOX_RUN_AGENT_TASK || '';
	if (/^(1|true|stable)$/i.test(mode)) {
		return true;
	}
  const descriptor = firstObject(options.runtimeDescriptor, options.runtime_descriptor)
		|| wpCodeboxRuntimeDescriptor(options);
	if (!descriptor) {
		return false;
	}
	return runtimeDescriptorSupportsCommand(descriptor, WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND);
}

function wpCodeboxRuntimeDescriptor(options = {}) {
	const env = options.env || process.env;
	const bin = firstValue(options.bin, options.wpCodeboxBin, options.wp_codebox_bin, wpCodeboxBin({ ...options, env }));
	if (!bin) {
		return null;
	}
	const resolved = wpCodeboxResolveCommand(bin, ['runtime', 'descriptor', '--json']);
	const spawn = options.spawnSync || spawnSync;
	const result = spawn(resolved.command, resolved.args, {
		encoding: 'utf8',
		env,
		maxBuffer: 1024 * 1024,
		timeout: options.timeoutMs || options.timeout_ms || 5000,
	});
	if (result.error || result.status !== 0) {
		return null;
	}
	return parseRuntimeDescriptorJson(result.stdout);
}

function parseRuntimeDescriptorJson(stdout) {
	try {
		const parsed = JSON.parse(stdout);
		return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : null;
	} catch {
		return null;
	}
}

function runtimeDescriptorSupportsCommand(descriptor, command) {
	const commands = new Set(runtimeDescriptorCommandNames(descriptor));
	if (commands.has(command)) {
		return true;
	}
	const capabilities = new Set(runtimeDescriptorCapabilityNames(descriptor));
	return capabilities.has(command)
		|| capabilities.has(command.replace(/-/g, '_'))
		|| capabilities.has(`wp-codebox/${command}`);
}

function runtimeDescriptorCommandNames(descriptor = {}) {
	return uniqueStrings([
		...commandNamesFromValue(descriptor.commands),
		...commandNamesFromValue(descriptor.cli_commands),
		...commandNamesFromValue(descriptor.cliCommands),
		...commandNamesFromValue(descriptor.tasks),
		...commandNamesFromValue(descriptor.abilities),
		...commandNamesFromValue(descriptor.runtime?.commands),
		...commandNamesFromValue(descriptor.runtime?.tasks),
		...commandNamesFromValue(descriptor.runtime?.abilities),
		...commandNamesFromValue(descriptor.agent_task?.commands),
		...commandNamesFromValue(descriptor.agentTask?.commands),
	]);
}

function runtimeDescriptorCapabilityNames(descriptor = {}) {
	return uniqueStrings([
		...commandNamesFromValue(descriptor.capabilities),
		...commandNamesFromValue(descriptor.runtime_capabilities),
		...commandNamesFromValue(descriptor.runtimeCapabilities),
		...commandNamesFromValue(descriptor.runtime?.capabilities),
	]);
}

function commandNamesFromValue(value) {
	if (!value) {
		return [];
	}
	if (typeof value === 'string') {
		return [value];
	}
	if (Array.isArray(value)) {
		return value.flatMap((entry) => commandNamesFromValue(entry));
	}
	if (typeof value !== 'object') {
		return [];
	}
	return Object.entries(value).flatMap(([key, entry]) => {
		if (entry === true) {
			return [key];
		}
		if (typeof entry === 'string') {
			return [key, entry];
		}
		if (entry && typeof entry === 'object' && !Array.isArray(entry)) {
			return [key, entry.name, entry.command, entry.id, entry.capability].filter(Boolean);
		}
		return [key];
	});
}

function uniqueStrings(values) {
	return [...new Set(values.map((value) => String(value || '').trim()).filter(Boolean))];
}

function wpCodeboxProviderPluginPathsFromEnv(env = process.env) {
	return firstNonEmptyArray(
		envPathList(env.HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATHS),
		envPathList(env.HOMEBOY_AGENT_RUNTIME_PROVIDER_PLUGIN_PATH),
		envPathList(env.HOMEBOY_WP_CODEBOX_PROVIDER_PLUGIN_PATHS),
		envPathList(env.WP_CODEBOX_PROVIDER_PLUGIN_PATHS),
		envPathList(env.HOMEBOY_WP_CODEBOX_PROVIDER_PLUGIN_PATH),
		envPathList(env.WP_CODEBOX_PROVIDER_PLUGIN_PATH),
	);
}

function wpCodeboxRuntimePackageSourceDescriptor(value, options = {}) {
	if (typeof value === 'string') {
		const source = wpCodeboxRuntimePackageImportPath(value, options);
		const slug = wpCodeboxRuntimePackageIdentifier(value);
		return {
			descriptor: withoutUndefinedValues(relativeRuntimePackagePath(value) ? { slug, source } : { slug: source || slug }),
			source,
			slug,
		};
	}
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return { descriptor: null, source: '', slug: '' };
	}

	const descriptor = wpCodeboxRuntimePackageDescriptorForCodebox(value, options);
	const declaredSources = WP_CODEBOX_RUNTIME_PACKAGE_SOURCE_FIELDS
		.map((key) => descriptor[key])
		.filter((source) => typeof source === 'string' && source !== '');
	const uniqueSources = Array.from(new Set(declaredSources));
	if (uniqueSources.length > 1 && options.rejectDivergentSources !== false) {
		throw new Error(`WP Codebox runtime_package descriptor source fields cannot diverge: ${uniqueSources.join(', ')}`);
	}
	const source = uniqueSources[0] || '';
	const slug = firstValue(descriptor.slug, descriptor.id, descriptor.name, wpCodeboxRuntimePackageIdentifier(source));
	return { descriptor, source, slug };
}

function wpCodeboxRuntimePackagePackage(value, options = {}) {
	const sourceDescriptor = wpCodeboxRuntimePackageSourceDescriptor(value, options);
	if (!sourceDescriptor.descriptor) {
		return null;
	}
	if (typeof value === 'string') {
		return sourceDescriptor.descriptor;
	}
	return withoutUndefinedValues({
		...sourceDescriptor.descriptor,
		slug: sourceDescriptor.slug,
		...(sourceDescriptor.source ? { source: sourceDescriptor.source } : {}),
		path: undefined,
		bundle_path: undefined,
		bundlePath: undefined,
		id: undefined,
		name: undefined,
	});
}

function wpCodeboxRuntimePackageDescriptorForCodebox(descriptor, options = {}) {
	if (!descriptor || typeof descriptor !== 'object' || Array.isArray(descriptor)) {
		return descriptor;
	}
	const normalized = { ...descriptor };
	for (const key of WP_CODEBOX_RUNTIME_PACKAGE_SOURCE_FIELDS) {
		if (typeof normalized[key] === 'string' && normalized[key]) {
			normalized[key] = wpCodeboxRuntimePackagePathForSandbox(normalized[key], options);
		}
	}
	return normalized;
}

function wpCodeboxRuntimePackageImportPath(value, options = {}) {
	if (typeof value === 'string') {
		return wpCodeboxRuntimePackagePathForSandbox(value, options);
	}
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return '';
	}
	const importPath = firstValue(...WP_CODEBOX_RUNTIME_PACKAGE_SOURCE_FIELDS.map((key) => value[key]), '');
	if (importPath) {
		return wpCodeboxRuntimePackagePathForSandbox(importPath, options);
	}
	return firstValue(value.slug, value.id, value.name, '');
}

function wpCodeboxRuntimePackageIdentifier(value) {
	if (typeof value === 'string') {
		return path.basename(String(value).replace(/\/+$/, '')) || value;
	}
	if (!value || typeof value !== 'object' || Array.isArray(value)) {
		return '';
	}
	return firstValue(value.slug, value.id, value.name, value.source, '');
}

function wpCodeboxRuntimePackagePathForSandbox(value, options = {}) {
	const raw = String(value || '');
	if (!raw || !relativeRuntimePackagePath(raw)) {
		return raw;
	}
	const workspaceTarget = String(options.workspaceTarget || '').replace(/\/+$/, '');
	return workspaceTarget ? `${workspaceTarget}/${raw.replace(/^\.\//, '')}` : raw;
}

function relativeRuntimePackagePath(value) {
	const raw = String(value || '');
	return raw.includes('/')
		&& !raw.startsWith('/')
		&& !raw.startsWith('~/')
		&& !/^[A-Za-z]:[\\/]/.test(raw)
		&& !/^[a-z][a-z0-9+.-]*:/i.test(raw);
}

function homeboySettings(env = process.env) {
  if (!env.HOMEBOY_SETTINGS_JSON) {
    return {};
  }
  try {
    const parsed = JSON.parse(env.HOMEBOY_SETTINGS_JSON);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

function arrayOrDefault(value, fallback) {
	return Array.isArray(value) ? value : [...fallback];
}

function envPathList(value) {
	if (!value) {
		return [];
	}
	try {
		const parsed = JSON.parse(value);
		if (Array.isArray(parsed)) {
			return parsed.filter(Boolean);
		}
	} catch {
		// Fall through to PATH-style lists for simple environment configuration.
	}
	return String(value).split(path.delimiter).map((entry) => entry.trim()).filter(Boolean);
}

function firstNonEmptyArray(...values) {
	for (const value of values) {
		const normalized = Array.isArray(value) ? value.filter(Boolean) : (value ? [value] : []);
		if (normalized.length > 0) {
			return normalized;
		}
	}
	return [];
}

function firstValue(...values) {
  return values.find((value) => value !== undefined && value !== null && value !== '');
}

function withoutUndefinedValues(value) {
	return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function firstObject(...values) {
	return values.find((value) => value && typeof value === 'object' && !Array.isArray(value));
}

module.exports = {
  DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR,
  WP_CODEBOX_CLI_DESCRIPTOR_SCHEMA,
	WP_CODEBOX_RUNTIME_PACKAGE_SOURCE_FIELDS,
	parseRuntimeDescriptorJson,
	relativeRuntimePackagePath,
	runtimeDescriptorCommandNames,
	runtimeDescriptorSupportsCommand,
	homeboySettings,
	managedWpCodeboxBin,
	wpCodeboxBinaryDiagnostic,
	wpCodeboxBin,
	wpCodeboxBinFromRuntimeComponent,
	wpCodeboxCliDescriptor,
	wpCodeboxCommand,
	wpCodeboxProviderPluginPathsFromEnv,
	wpCodeboxResolveCommand,
	wpCodeboxRuntimePackageDescriptorForCodebox,
	wpCodeboxRuntimePackageIdentifier,
	wpCodeboxRuntimePackageImportPath,
	wpCodeboxRuntimePackagePackage,
	wpCodeboxRuntimePackagePathForSandbox,
	wpCodeboxRuntimePackageSourceDescriptor,
	wpCodeboxRuntimeDescriptor,
	wpCodeboxSupportsRunAgentTaskCommand,
};
