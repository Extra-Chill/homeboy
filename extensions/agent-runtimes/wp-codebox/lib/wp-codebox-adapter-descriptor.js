'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

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
    ...descriptor.settings.map((key) => settings[key]),
    env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN,
    ...descriptor.env.map((key) => env[key]),
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

function firstValue(...values) {
  return values.find((value) => value !== undefined && value !== null && value !== '');
}

module.exports = {
  DEFAULT_WP_CODEBOX_CLI_DESCRIPTOR,
  WP_CODEBOX_CLI_DESCRIPTOR_SCHEMA,
  homeboySettings,
  managedWpCodeboxBin,
  wpCodeboxBin,
  wpCodeboxBinFromRuntimeComponent,
  wpCodeboxCliDescriptor,
  wpCodeboxCommand,
};
