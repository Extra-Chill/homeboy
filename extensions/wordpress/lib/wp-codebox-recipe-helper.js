'use strict';

/**
 * External dependencies
 */
const { execFile } = require('node:child_process');
const fs = require('node:fs/promises');
const path = require('node:path');
const { promisify } = require('node:util');

const execFileAsync = promisify(execFile);
const DEFAULT_MAX_BUFFER = 1024 * 1024 * 50;
const DEFAULT_EVENT_SOURCE = 'wp_codebox';
const DEFAULT_EVENT_PREFIX = 'recipe';

function wpCodeboxBin(options = {}) {
  const env = options.env || process.env;
  const settings = homeboySettings(env);
  return options.wpCodeboxBin
    || options.bin
    || env.HOMEBOY_WP_CODEBOX_BIN
    || env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN
    || settings.wp_codebox_bin
    || env.WP_CODEBOX_BIN
    || 'wp-codebox';
}

function homeboySettings(env) {
  if (!env.HOMEBOY_SETTINGS_JSON) {
    return {};
  }

  try {
    const parsed = JSON.parse(env.HOMEBOY_SETTINGS_JSON);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
  } catch (_error) {
    return {};
  }
}

function wpCodeboxCommand(bin = wpCodeboxBin()) {
  if (/\.(?:js|cjs|mjs)$/.test(bin)) {
    return { command: process.execPath, args: [bin] };
  }

  return { command: bin, args: [] };
}

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
    'recipe-run',
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
  homeboySettings,
  wpCodeboxBin,
  wpCodeboxCommand,
};
