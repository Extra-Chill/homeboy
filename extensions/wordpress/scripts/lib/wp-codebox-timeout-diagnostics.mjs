#!/usr/bin/env node

/**
 * External dependencies
 */

import { open, readFile } from 'node:fs/promises';
import { StringDecoder } from 'node:string_decoder';

export const WP_CODEBOX_TIMEOUT_DIAGNOSTICS_SCHEMA = 'homeboy/wp-codebox-timeout-diagnostics/v1';
export const WP_CODEBOX_TIMEOUT_DIAGNOSTICS_MAX_BYTES = 8192;

const TEXT_EXCERPT_BYTES = 1024;
const FIELD_BYTES = 512;
const INPUT_BYTES = 128 * 1024;
const MAX_REDACTED_RECORD_BYTES = 128 * 1024;

export function wpCodeboxTimeoutDiagnostics({
  phase,
  elapsedSeconds,
  budgetSeconds,
  selected = [],
  termination = {},
  artifacts = [],
  payload,
  stderr = '',
  runtimeCrash = null,
  secretValues = secretValuesFromEnvironment(process.env),
} = {}) {
  const recipeRun = asObject(payload);
  const executions = Array.isArray(recipeRun?.executions) ? recipeRun.executions : [];
  const completed = executions.filter((execution) => integerExitCode(execution) !== undefined).length;
  const last = executions.at(-1);
  const diagnostic = {
    schema: WP_CODEBOX_TIMEOUT_DIAGNOSTICS_SCHEMA,
    phase: boundedText(phase, FIELD_BYTES, secretValues) || 'wp-codebox-recipe-run',
    elapsed_seconds: finiteNonNegative(elapsedSeconds),
    budget_seconds: finiteNonNegative(budgetSeconds),
    selected: {
      count: selected.length,
      items: selected.slice(0, 20).map((value) => boundedText(value, FIELD_BYTES, secretValues)).filter(Boolean),
    },
    execution: {
      count: executions.length,
      count_complete: recipeRun?.execution_count_complete !== false,
      completed_count: completed,
      last: executionSummary(last, executions.length - 1, secretValues),
    },
    termination: {
      result: boundedText(termination.result, FIELD_BYTES, secretValues) || 'timeout',
      signal: boundedText(termination.signal, FIELD_BYTES, secretValues) || undefined,
      code: Number.isInteger(termination.code) ? termination.code : undefined,
    },
    artifact_refs: artifacts.slice(0, 20).map((artifact) => boundedText(artifact, FIELD_BYTES, secretValues)).filter(Boolean),
    // A fatal runtime crash is the cause; the elapsed budget is only how long
    // it took to notice. Project it as its own field so a consumer does not
    // have to grep the excerpts to find out which one it was reading.
    runtime_crash: asObject(runtimeCrash) ? withoutUndefined({
      id: boundedText(runtimeCrash.id, FIELD_BYTES, secretValues) || undefined,
      message: boundedText(runtimeCrash.message, FIELD_BYTES, secretValues) || undefined,
      wasm_frame: runtimeCrash.wasm_frame === true ? true : undefined,
    }) : undefined,
    excerpts: boundedText(stderr, TEXT_EXCERPT_BYTES, secretValues) || undefined,
  };
  removeUndefined(diagnostic.termination);
  removeUndefined(diagnostic);

  // Every projected field is independently bounded. This final guard keeps the
  // record safe if a future caller adds a larger field.
  let serialized = JSON.stringify(diagnostic);
  if (Buffer.byteLength(serialized) > WP_CODEBOX_TIMEOUT_DIAGNOSTICS_MAX_BYTES) {
    delete diagnostic.excerpts;
    diagnostic.selected.items = diagnostic.selected.items.slice(0, 5);
    diagnostic.artifact_refs = diagnostic.artifact_refs.slice(0, 5);
    serialized = JSON.stringify(diagnostic);
  }
  if (Buffer.byteLength(serialized) > WP_CODEBOX_TIMEOUT_DIAGNOSTICS_MAX_BYTES) {
    throw new Error('WP Codebox timeout diagnostic exceeded its publication byte ceiling.');
  }
  return diagnostic;
}

function executionSummary(execution, index, secretValues) {
  if (!asObject(execution)) {
    return undefined;
  }
  return withoutUndefined({
    index,
    command: boundedText(execution.command, FIELD_BYTES, secretValues),
    status: boundedText(execution.status, FIELD_BYTES, secretValues),
    exit_code: integerExitCode(execution),
    stdout: outputState(execution.stdout),
    stderr: outputState(execution.stderr),
  });
}

function outputState(value) {
  if (typeof value === 'string') {
    return { kind: 'text', bytes: Buffer.byteLength(value) };
  }
  if (isByteMap(value)) {
    return { kind: 'byte_map_omitted' };
  }
  if (value !== undefined && value !== null) {
    return { kind: 'non_text_omitted' };
  }
  return undefined;
}

function isByteMap(value) {
  if (!asObject(value) || Array.isArray(value)) {
    return false;
  }
  const entries = Object.entries(value);
  return entries.length > 0 && entries.every(([key, entry]) => /^\d+$/.test(key) && Number.isInteger(entry) && entry >= 0 && entry <= 255);
}

function boundedText(value, maxBytes, secretValues) {
  if (typeof value !== 'string' || value === '') {
    return '';
  }
  const text = redactTimeoutText(value, secretValues);
  const bytes = Buffer.from(text);
  return bytes.byteLength <= maxBytes ? text : `${bytes.subarray(0, Math.max(0, maxBytes - 14)).toString('utf8')}...[truncated]`;
}

export function redactTimeoutText(value, secretValues = secretValuesFromEnvironment(process.env)) {
  if (typeof value !== 'string' || value === '') {
    return '';
  }
  let text = value;
  for (const secret of secretValues) {
    text = text.replaceAll(secret, '[REDACTED]');
  }
  text = text
    .replace(/((?:authorization|proxy-authorization)\s*:\s*)(?:[a-z]+\s+)?[^"\r\n]+(?:\r?\n[ \t]+[^"\r\n]+)*/gi, '$1[REDACTED]')
    .replace(/((?:set-cookie|cookie)\s*:\s*)[^"\r\n]+(?:\r?\n[ \t]+[^"\r\n]+)*/gi, '$1[REDACTED]')
    .replace(/([a-z][a-z0-9+.-]*:\/\/)[^\s/@:]+(?::[^\s/@]*)?@/gi, '$1[REDACTED]@')
    .replace(/((?:api[_-]?key|secret|password|credential|session|cookie|auth(?:orization)?)\s*[=:]\s*|token\s*=\s*)(?!\[REDACTED\])[^"\r\n]*(?:\r?\n[ \t]+[^"\r\n]+)*/gi, '$1[REDACTED]');
  return text;
}

// Hold a complete logical record until it can be redacted. A record that exceeds
// the bounded buffer is omitted rather than publishing a prefix or continuation
// that could be part of a credential split across stream chunks.
export function createTimeoutLineRedactor(secretValues = secretValuesFromEnvironment(process.env), maxRecordBytes = MAX_REDACTED_RECORD_BYTES) {
  const decoder = new StringDecoder('utf8');
  let pending = '';
  let omitting = false;
  const omitted = () => '[REDACTED OVERLONG LINE]\n';
  const flushLine = (line) => redactTimeoutText(line, secretValues);
  const consume = (input, end = false) => {
    let output = '';
    pending += input;
    while (true) {
      const newline = pending.indexOf('\n');
      if (newline === -1) {
        if (!omitting && Buffer.byteLength(pending) > maxRecordBytes) {
          pending = '';
          omitting = true;
        }
        break;
      }
      const line = pending.slice(0, newline + 1);
      pending = pending.slice(newline + 1);
      if (omitting || Buffer.byteLength(line) > maxRecordBytes) {
        output += omitted();
      } else {
        output += flushLine(line);
      }
      omitting = false;
    }
    if (end) {
      if (omitting || Buffer.byteLength(pending) > maxRecordBytes) {
        output += omitted();
      } else if (pending) {
        output += flushLine(pending);
      }
      pending = '';
      omitting = false;
    }
    return output;
  };
  return {
    write(chunk) { return consume(decoder.write(chunk)); },
    end() { return consume(decoder.end(), true); },
  };
}

function secretValuesFromEnvironment(environment) {
  return Object.entries(environment)
    .filter(([key, value]) => /(?:auth|cookie|credential|key|nonce|passw|secret|session|token)/i.test(key) && typeof value === 'string' && value.length >= 4)
    .map(([, value]) => value)
    .sort((left, right) => right.length - left.length);
}

function integerExitCode(execution) {
  const value = execution?.exitCode ?? execution?.exit_code;
  return Number.isInteger(value) ? value : undefined;
}

function finiteNonNegative(value) {
  return Number.isFinite(Number(value)) && Number(value) >= 0 ? Number(value) : undefined;
}

function asObject(value) {
  return value && typeof value === 'object' && !Array.isArray(value) ? value : null;
}

function withoutUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined));
}

function removeUndefined(value) {
  for (const key of Object.keys(value)) {
    if (value[key] === undefined) {
      delete value[key];
    }
  }
}

if (import.meta.filename === process.argv[1]) {
  const [phase, elapsedSeconds, budgetSeconds, selected, terminationPath, outputPath, stderrPath, artifactsDirectory] = process.argv.slice(2);
  const [payloadInput, stderr, terminationText] = await Promise.all([
    readBoundedText(outputPath),
    readBoundedText(stderrPath),
    readFile(terminationPath, 'utf8').catch(() => '{}'),
  ]);
  let payload;
  let termination;
  try { payload = payloadInput.truncated ? recipeRunProjection(payloadInput.text) : JSON.parse(payloadInput.text); } catch { payload = recipeRunProjection(payloadInput.text); }
  try { termination = JSON.parse(terminationText); } catch {}
  const artifacts = ['recipe-run.json', 'wp-codebox.stderr', 'recipe.json', 'termination.json']
    .map((name) => `artifact://${artifactsDirectory}/${name}`);
  process.stdout.write(`${JSON.stringify(wpCodeboxTimeoutDiagnostics({
    phase,
    elapsedSeconds,
    budgetSeconds,
    selected: selected ? [selected] : [],
    termination,
    artifacts,
    payload,
    stderr,
  }))}\n`);
}

export async function readBoundedText(filePath, maxBytes = INPUT_BYTES) {
  let handle;
  try {
    handle = await open(filePath, 'r');
    const stat = await handle.stat();
    const bytes = Math.min(stat.size, maxBytes);
    const buffer = Buffer.alloc(bytes);
    await handle.read(buffer, 0, bytes, 0);
    return { text: buffer.toString('utf8'), truncated: stat.size > maxBytes };
  } catch {
    return { text: '', truncated: false };
  } finally {
    await handle?.close();
  }
}

export function recipeRunProjection(text) {
  const execution = {};
  const command = jsonStringField(text, 'command');
  const status = jsonStringField(text, 'status');
  const exitCode = numberField(text, 'exitCode') ?? numberField(text, 'exit_code');
  if (command) { execution.command = command; }
  if (status) { execution.status = status; }
  if (exitCode !== undefined) { execution.exitCode = exitCode; }
  if (/"stdout"\s*:\s*\{\s*"\d+"\s*:/s.test(text)) { execution.stdout = { 0: 0 }; }
  else if (/"stdout"\s*:\s*"/s.test(text)) { execution.stdout = ''; }
  if (/"stderr"\s*:\s*"/s.test(text)) { execution.stderr = ''; }
  return {
    executions: Object.keys(execution).length > 0 ? [execution] : [],
    execution_count_complete: false,
  };
}

function jsonStringField(text, name) {
  const match = new RegExp(`"${name}"\\s*:\\s*("(?:\\\\.|[^"\\\\])*")`, 's').exec(text);
  if (!match) { return ''; }
  try { return JSON.parse(match[1]); } catch { return ''; }
}

function numberField(text, name) {
  const match = new RegExp(`"${name}"\\s*:\\s*(-?\\d+)`, 's').exec(text);
  return match ? Number(match[1]) : undefined;
}
