'use strict';

/**
 * External dependencies
 */
const { execFile } = require('node:child_process');
const fs = require('node:fs/promises');
const path = require('node:path');
const { promisify } = require('node:util');

const {
  homeboySettings,
  wpCodeboxBin,
  wpCodeboxCliDescriptor,
  wpCodeboxCommand,
} = require('../../agent-runtimes/wp-codebox/lib/wp-codebox-adapter-descriptor');

const execFileAsync = promisify(execFile);
const WP_CODEBOX_RECIPE_RUN_CLI_COMMAND = wpCodeboxCliDescriptor().commands.recipe_run;
const DEFAULT_MAX_BUFFER = 1024 * 1024 * 50;
const DEFAULT_EVENT_SOURCE = 'wp_codebox';
const DEFAULT_EVENT_PREFIX = 'recipe';

function recipeEventName(name, options = {}) {
  const prefix = options.eventPrefix || DEFAULT_EVENT_PREFIX;
  return prefix ? `${prefix}.${name}` : name;
}

function emitRecipeEvent(event, name, data = {}, options = {}) {
  if (typeof event !== 'function') {
    return;
  }

  event(options.eventSource || DEFAULT_EVENT_SOURCE, recipeEventName(name, options), data);
}

function parseWpCodeboxJson(stdout) {
  const trimmed = String(stdout || '').trim();
  if (!trimmed) {
    return null;
  }

  return JSON.parse(trimmed);
}

function normalizePluginStateList(value, field) {
  if (value === undefined || value === null || value === false) {
    return [];
  }
  const items = Array.isArray(value) ? value : [value];
  return items.map((item, index) => {
    if (typeof item === 'string') {
      const plugin = item.trim();
      if (!plugin) {
        throw new TypeError(`${field} plugin ${index + 1} must be non-empty.`);
      }
      return { plugin };
    }
    if (!item || typeof item !== 'object' || Array.isArray(item)) {
      throw new TypeError(`${field} plugin ${index + 1} must be a string or object.`);
    }
    let plugin = '';
    if (typeof item.plugin === 'string' && item.plugin.trim()) {
      plugin = item.plugin.trim();
    } else if (typeof item.slug === 'string' && item.slug.trim()) {
      plugin = item.slug.trim();
    }
    if (!plugin) {
      throw new TypeError(`${field} plugin ${index + 1} requires plugin or slug.`);
    }
    return { ...item, plugin };
  });
}

function wpCodeboxPluginStateStep(options = {}) {
  const state = {
    activate: normalizePluginStateList(options.activate, 'activate'),
    deactivate: normalizePluginStateList(options.deactivate, 'deactivate'),
    report: options.report !== false,
  };
  return {
    command: 'wordpress.plugin-state',
    args: [`plugin-state-json=${JSON.stringify(state)}`],
  };
}

async function runWpCodeboxRecipe({
  recipeFile,
  artifactsDir,
  outputFile,
  recipeRunArgs = [],
  event,
  maxBuffer = DEFAULT_MAX_BUFFER,
  wpCodeboxBin: explicitWpCodeboxBin,
  bin,
  env,
  cwd,
  eventSource,
  eventPrefix,
} = {}) {
  if (!recipeFile) {
    throw new Error('runWpCodeboxRecipe requires recipeFile.');
  }
  if (!artifactsDir) {
    throw new Error('runWpCodeboxRecipe requires artifactsDir.');
  }

  await fs.mkdir(artifactsDir, { recursive: true });
  if (outputFile) {
    await fs.mkdir(path.dirname(outputFile), { recursive: true });
  }

  const eventOptions = { eventSource, eventPrefix };
  const resolvedBin = wpCodeboxBin({ wpCodeboxBin: explicitWpCodeboxBin, bin, env });
  const { command, args } = wpCodeboxCommand(resolvedBin);
  const commandArgs = [
    ...args,
    WP_CODEBOX_RECIPE_RUN_CLI_COMMAND,
    '--recipe',
    recipeFile,
    '--artifacts',
    artifactsDir,
    ...recipeRunArgs,
    '--json',
  ];

  emitRecipeEvent(event, 'start', { recipe_file: recipeFile, artifacts_dir: artifactsDir }, eventOptions);

  try {
    const result = await execFileAsync(command, commandArgs, { cwd, env, maxBuffer });
    if (outputFile) {
      await fs.writeFile(outputFile, result.stdout);
    }
    const json = parseWpCodeboxJson(result.stdout);
    emitRecipeEvent(event, 'done', { output_file: outputFile || null, artifacts_dir: artifactsDir }, eventOptions);
    return { ...result, json };
  } catch (error) {
    if (outputFile && typeof error?.stdout === 'string' && error.stdout) {
      await fs.writeFile(outputFile, error.stdout);
    }

    emitRecipeEvent(event, 'failed', {
      output_file: outputFile || null,
      artifacts_dir: artifactsDir,
      exit_code: error?.code ?? null,
    }, eventOptions);
    throw error;
  }
}

module.exports = {
  DEFAULT_WP_CODEBOX_RECIPE_EVENT_PREFIX: DEFAULT_EVENT_PREFIX,
  DEFAULT_WP_CODEBOX_RECIPE_EVENT_SOURCE: DEFAULT_EVENT_SOURCE,
  emitRecipeEvent,
  parseWpCodeboxJson,
  recipeEventName,
  runWpCodeboxRecipe,
  wpCodeboxPluginStateStep,
  homeboySettings,
  wpCodeboxBin,
  wpCodeboxCommand,
};
