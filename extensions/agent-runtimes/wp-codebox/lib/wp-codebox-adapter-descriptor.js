'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const {
  WP_CODEBOX_LEGACY_AGENT_TASK_RUN_CLI_COMMAND,
  WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND,
} = require('./codebox-run-agent-task-contract');
const {
  WP_CODEBOX_RECIPE_RUN_CLI_COMMAND,
} = require('./wp-codebox-adapter-contract');

const WP_CODEBOX_CLI_DESCRIPTOR_SCHEMA = 'wp-codebox/cli-descriptor/v1';

const DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR = {
  schema: WP_CODEBOX_CLI_DESCRIPTOR_SCHEMA,
  id: 'wp-codebox',
  env: ['HOMEBOY_WP_CODEBOX_BIN', 'WP_CODEBOX_BIN'],
  settings: ['wp_codebox_bin', 'wpCodeboxBin'],
  executable: 'wp-codebox',
  commands: {
    run_agent_task: WP_CODEBOX_RUN_AGENT_TASK_CLI_COMMAND,
    legacy_agent_task_run: WP_CODEBOX_LEGACY_AGENT_TASK_RUN_CLI_COMMAND,
    recipe_run: WP_CODEBOX_RECIPE_RUN_CLI_COMMAND,
  },
  path_aliases: {
    runtime_bin: ['wp_codebox_bin', 'wpCodeboxBin'],
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
    path_aliases: {
      ...DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR.path_aliases,
      ...(overrides.path_aliases || overrides.pathAliases || {}),
    },
  };
}

function wpCodeboxBin(options = {}) {
  const descriptor = wpCodeboxCliDescriptor(options.descriptor || options.cliDescriptor || {});
  const env = options.env || process.env;
  const settings = options.settings || homeboySettings(env);
  const packagedRuntimeCandidates = [
    wpCodeboxBinFromRuntimeComponent(env),
    managedWpCodeboxBin(env),
  ];
  const explicitBinCandidates = [
    options.runtimeBin,
    options.runtime_bin,
    options.wpCodeboxBin,
    options.wp_codebox_bin,
  ];
  return firstValue(
    ...(options.preferPackagedRuntime ? packagedRuntimeCandidates : explicitBinCandidates),
    options.bin,
    ...descriptor.env.map((key) => env[key]),
    ...descriptor.settings.map((key) => settings[key]),
    env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN,
    ...(options.preferPackagedRuntime ? explicitBinCandidates : packagedRuntimeCandidates),
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
	if (/^(0|false|legacy)$/i.test(mode)) {
		return false;
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

function firstObject(...values) {
	return values.find((value) => value && typeof value === 'object' && !Array.isArray(value));
}

module.exports = {
  DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR,
  WP_CODEBOX_CLI_DESCRIPTOR_SCHEMA,
	parseRuntimeDescriptorJson,
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
	wpCodeboxRuntimeDescriptor,
	wpCodeboxSupportsRunAgentTaskCommand,
};
