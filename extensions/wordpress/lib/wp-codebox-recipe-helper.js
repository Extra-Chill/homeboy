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
// Grace period between SIGTERM and the SIGKILL escalation when killing a wedged
// recipe-run child. Long enough for a cooperating child to flush + exit cleanly,
// short enough that a truly wedged child (the 28-min orphan scenario) is reaped
// promptly rather than abandoned.
const DEFAULT_KILL_GRACE_MS = 5000;

/**
 * Internal dependencies
 */
const {
	homeboySettings,
	wpCodeboxCommand,
} = require('./wp-codebox-resolver');
const { preflightWpCodeboxCommand, preflightWpCodeboxRuntime, selectWpCodeboxRuntime, wpCodeboxCommand: runtimeCommand } = require('./wp-codebox-runtime-selection');

function wpCodeboxBin(options = {}) {
  const env = { ...process.env, ...(options.env || {}) };
  const settings = { ...homeboySettings(env), ...(options.settings || {}) };
  const selected = selectWpCodeboxRuntime({ ...options, env, settings }).selected.path;
  if (!selected) throw new Error('WP Codebox binary is not configured. Set wp_codebox_bin or HOMEBOY_WP_CODEBOX_BIN.');
  return selected;
}

function canonicalWpCodeboxRuntime(options = {}) {
  const env = { ...process.env, ...(options.env || {}) };
  const settings = { ...homeboySettings(env), ...(options.settings || {}) };
  const runtime = preflightWpCodeboxRuntime({ ...options, env, settings });
  if (!runtime.ready) {
    throw new Error(`WP Codebox runtime preflight failed: ${runtime.reason}; required >=${runtime.required_version}, observed ${runtime.selected.version || 'unavailable'} at ${runtime.selected.path || 'no executable'}. Run ${runtime.remediation}.`);
  }
  const invocation = runtimeCommand(runtime.selected.path);
  const command = preflightWpCodeboxCommand([invocation.command, ...invocation.args], { ...options, env, settings });
  if (!command.ready) {
    throw new Error(`WP Codebox command preflight failed: ${command.reason}; required >=${command.required_version}, observed ${command.selected.version || 'unavailable'} at ${command.selected.path || 'no executable'}. Run ${command.remediation}.`);
  }
  return { ...runtime, invocation };
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

// Send a signal to the child's entire process group so any sub-processes it
// spawned (a wedged sandbox can fork WP/PHP/node helpers) die with it, not just
// the immediate child. The child is spawned `detached`, so it leads its own
// group and `process.kill(-pid, …)` targets the whole group. Falls back to a
// direct child kill if the group is already gone (ESRCH) or the platform refuses
// the negative-pid form.
function killChildGroup(child, signal) {
  if (!child || typeof child.pid !== 'number') {
    return;
  }
  try {
    process.kill(-child.pid, signal);
  } catch (groupError) {
    if (groupError && groupError.code === 'ESRCH') {
      return;
    }
    try {
      child.kill(signal);
    } catch {
      // Child already reaped — nothing left to signal.
    }
  }
}

async function runCommandStreaming(command, args, { cwd, env, maxBuffer, outputFile, signal, timeoutMs, killGraceMs = DEFAULT_KILL_GRACE_MS }) {
  return new Promise((resolve, reject) => {
    // `detached: true` puts the child in its own process group so a SIGTERM/
    // SIGKILL can reach the whole group (see killChildGroup). stdio stays piped
    // so output capture is unchanged.
    const child = spawn(command, args, { cwd, env, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
    const stdoutChunks = [];
    const stderrChunks = [];
    let stdoutSize = 0;
    let stderrSize = 0;
    let settled = false;
    let stdoutWriter = null;
    let killReason = null;
    let killGraceTimer = null;
    let watchdogTimer = null;

    // Note: the kill-grace (SIGKILL escalation) timer is intentionally NOT
    // cleared here. Once a kill is in progress the group SIGKILL must always run
    // to completion — the process-group leader can exit on SIGTERM while a
    // sub-process ignores it, and only the escalated group SIGKILL reaps that
    // straggler. The timer is unref'd, so leaving it pending never keeps the
    // process alive on its own, and a SIGKILL to an already-empty group is a
    // harmless ESRCH no-op.
    const clearTimers = () => {
      if (watchdogTimer) {
        clearTimeout(watchdogTimer);
        watchdogTimer = null;
      }
      if (signal) {
        signal.removeEventListener('abort', onAbort);
      }
    };

    // Begin the cooperative SIGTERM -> SIGKILL kill of the child (and its group).
    // The promise stays pending until the OS reaps the child and fires 'close',
    // so an aborted/timed-out run can never leave an orphaned recipe-run process.
    const killChild = (reason) => {
      if (killReason) {
        return;
      }
      killReason = reason;
      killChildGroup(child, 'SIGTERM');
      killGraceTimer = setTimeout(() => {
        killGraceTimer = null;
        killChildGroup(child, 'SIGKILL');
      }, killGraceMs);
      if (typeof killGraceTimer.unref === 'function') {
        killGraceTimer.unref();
      }
    };

    function onAbort() {
      killChild('abort');
    }

    const fail = (error) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimers();
      if (child.exitCode === null && !child.killed) {
        killChildGroup(child, 'SIGTERM');
      }
      reject(error);
    };

    if (signal) {
      if (signal.aborted) {
        // Already aborted before spawn — kill immediately; 'close' still reaps.
        killChild('abort');
      } else {
        signal.addEventListener('abort', onAbort, { once: true });
      }
    }

    if (Number.isFinite(timeoutMs) && timeoutMs > 0) {
      watchdogTimer = setTimeout(() => {
        killChild('timeout');
      }, timeoutMs);
      if (typeof watchdogTimer.unref === 'function') {
        watchdogTimer.unref();
      }
    }

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
    child.on('close', (code, closeSignal) => {
      const finish = () => {
        if (settled) {
          return;
        }
        settled = true;
        clearTimers();
        const stdout = outputFile ? '' : Buffer.concat(stdoutChunks).toString('utf8');
        const stderr = Buffer.concat(stderrChunks).toString('utf8');
        // The child has now exited and been reaped by the OS — when we initiated
        // the kill, surface a clear killed/timeout rejection instead of a bare
        // "Command failed" so the watchdog cause propagates.
        if (killReason) {
          const reasonLabel = killReason === 'timeout'
            ? `timed out after ${timeoutMs}ms`
            : 'was aborted';
          const error = new Error(`WP Codebox recipe-run ${reasonLabel} and was killed: ${command} ${args.join(' ')}`);
          error.code = code;
          error.signal = closeSignal;
          error.killed = true;
          error.killReason = killReason;
          if (killReason === 'timeout') {
            error.timeout_ms = timeoutMs;
          }
          error.stdout = stdout;
          error.stderr = stderr;
          reject(error);
          return;
        }
        if (code === 0) {
          resolve({ stdout, stderr });
          return;
        }
        const error = new Error(`Command failed: ${command} ${args.join(' ')}`);
        error.code = code;
        error.signal = closeSignal;
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
  signal,
  timeoutMs,
  killGraceMs,
  ...runtimeOptions
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
  const runtime = canonicalWpCodeboxRuntime({ ...runtimeOptions, wp_codebox_bin: explicitWpCodeboxBin || bin || runtimeOptions.wp_codebox_bin, env });
  const { command, args } = runtime.invocation;
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
    const result = await runCommandStreaming(command, commandArgs, { cwd, env, maxBuffer, outputFile, signal, timeoutMs, killGraceMs });
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
  canonicalWpCodeboxRuntime,
  wpCodeboxPluginStateStep,
  homeboySettings,
  wpCodeboxBin,
  wpCodeboxCommand,
};
