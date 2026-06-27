'use strict';

/**
 * External dependencies
 */
const { spawn } = require('node:child_process');
const fsSync = require('node:fs');
const fs = require('node:fs/promises');
const path = require('node:path');

const WP_CODEBOX_RECIPE_RUN_CLI_COMMAND = 'recipe-run';
const DEFAULT_MAX_BUFFER = 1024 * 1024 * 50;
const DEFAULT_EVENT_SOURCE = 'wp_codebox';
const DEFAULT_EVENT_PREFIX = 'recipe';

/**
 * Internal dependencies
 */
const {
  homeboySettings,
  wpCodeboxCommand,
} = require('./wp-codebox-resolver');
const { createCodeboxClient } = require('./codebox-client');

function wpCodeboxBin(options = {}) {
  return createCodeboxClient(options).identity().bin;
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

function appendBounded(chunks, currentSize, chunk, maxBuffer, streamName) {
  const nextSize = currentSize + chunk.length;
  if (nextSize > maxBuffer) {
    const error = new Error(`${streamName} maxBuffer length exceeded`);
    error.code = 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER';
    error.stream = streamName;
    throw error;
  }
  chunks.push(chunk);
  return nextSize;
}

async function runCommandStreaming(command, args, { cwd, env, maxBuffer, outputFile }) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] });
    const stdoutChunks = [];
    const stderrChunks = [];
    let stdoutSize = 0;
    let stderrSize = 0;
    let settled = false;
    let stdoutWriter = null;

    const fail = (error) => {
      if (settled) {
        return;
      }
      settled = true;
      if (child.exitCode === null && !child.killed) {
        child.kill('SIGTERM');
      }
      reject(error);
    };

    if (outputFile) {
      stdoutWriter = fsSync.createWriteStream(outputFile, { encoding: 'utf8' });
      stdoutWriter.on('error', fail);
    }

    child.stdout.on('data', (chunk) => {
      try {
        if (stdoutWriter) {
          stdoutWriter.write(chunk);
          return;
        }
        stdoutSize = appendBounded(stdoutChunks, stdoutSize, chunk, maxBuffer, 'stdout');
      } catch (error) {
        error.stdout = Buffer.concat(stdoutChunks).toString('utf8');
        error.stderr = Buffer.concat(stderrChunks).toString('utf8');
        fail(error);
      }
    });

    child.stderr.on('data', (chunk) => {
      try {
        stderrSize = appendBounded(stderrChunks, stderrSize, chunk, maxBuffer, 'stderr');
      } catch (error) {
        error.stdout = Buffer.concat(stdoutChunks).toString('utf8');
        error.stderr = Buffer.concat(stderrChunks).toString('utf8');
        fail(error);
      }
    });

    child.on('error', fail);
    child.on('close', (code, signal) => {
      const finish = () => {
        if (settled) {
          return;
        }
        settled = true;
        const stdout = outputFile ? '' : Buffer.concat(stdoutChunks).toString('utf8');
        const stderr = Buffer.concat(stderrChunks).toString('utf8');
        if (code === 0) {
          resolve({ stdout, stderr });
          return;
        }
        const error = new Error(`Command failed: ${command} ${args.join(' ')}`);
        error.code = code;
        error.signal = signal;
        error.stdout = stdout;
        error.stderr = stderr;
        reject(error);
      };

      if (stdoutWriter) {
        stdoutWriter.end(finish);
      } else {
        finish();
      }
    });
  });
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
  const identity = createCodeboxClient({ wpCodeboxBin: explicitWpCodeboxBin, bin, env }).identity();
  const { command, args } = identity.invocation;
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
    const result = await runCommandStreaming(command, commandArgs, { cwd, env, maxBuffer, outputFile });
    const stdout = outputFile ? await fs.readFile(outputFile, 'utf8') : result.stdout;
    const json = parseWpCodeboxJson(stdout);
    emitRecipeEvent(event, 'done', { output_file: outputFile || null, artifacts_dir: artifactsDir }, eventOptions);
    return { ...result, stdout, json };
  } catch (error) {
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
