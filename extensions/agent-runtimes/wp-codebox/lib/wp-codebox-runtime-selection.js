'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const { minimum_version: REQUIRED_WP_CODEBOX_VERSION } = require('../wp-codebox.json');

function selectWpCodeboxRuntime(options = {}) {
  const env = options.env || process.env;
  const settings = { ...settingsFromEnv(env), ...(options.settings || {}) };
  // A caller-provided executable is a pin. This matches the local runner's
  // explicit override contract: validate that candidate before falling back.
  const configured = firstValue(
    options.bin,
    options.runtimeBin,
    options.runtime_bin,
    options.wpCodeboxBin,
    options.wp_codebox_bin,
    env.HOMEBOY_WP_CODEBOX_BIN,
    env.WP_CODEBOX_BIN,
    env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN,
    settings.runtime_bin,
    settings.wp_codebox_bin,
    settings.wpCodeboxBin,
  );
  const managed = path.join(env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.join(env.HOME || os.homedir(), '.cache', 'homeboy', 'wp-codebox'), 'source', 'packages', 'cli', 'dist', 'index.js');
  const pathCandidate = resolvePathCommand('wp-codebox', env, options);
  const candidates = {
    configured: candidate(configured, 'configured', options),
    managed: candidate(managed, 'managed', options),
    path: candidate(pathCandidate, 'path', options),
  };
  // The managed runtime is the reproducible default. Explicit configuration is
  // used only when that managed build is unavailable.
  const selected = options.strictBin && configured
    ? candidates.configured
    : [candidates.configured, candidates.managed, candidates.path].find((entry) => entry.available) || emptyCandidate('');
  return { candidates, selected };
}

function preflightWpCodeboxRuntime(options = {}) {
  const selection = selectWpCodeboxRuntime(options);
  const requiredVersion = options.requiredVersion || REQUIRED_WP_CODEBOX_VERSION;
  if (!selection.selected.path) {
    return failure(selection, requiredVersion, '', 'wp_codebox_not_found', 'homeboy extension setup wordpress');
  }
  const invocation = wpCodeboxCommand(selection.selected.path);
  const result = (options.spawnSync || spawnSync)(invocation.command, [...invocation.args, '--version'], {
    encoding: 'utf8', env: options.env || process.env, timeout: options.timeoutMs || 5_000,
  });
  const version = parseVersion(`${result.stdout || ''}\n${result.stderr || ''}`);
  if (result.error || result.status !== 0 || !version) {
    return failure(selection, requiredVersion, version, 'wp_codebox_version_probe_failed', 'homeboy extension setup wordpress');
  }
  if (compareVersions(version, requiredVersion) < 0) {
    return failure(selection, requiredVersion, version, 'wp_codebox_version_too_old', 'homeboy extension setup wordpress');
  }
  return { ready: true, required_version: requiredVersion, selected: { ...selection.selected, version }, candidates: selection.candidates, remediation: '' };
}

// Consumers that received an already-resolved argv must validate that exact
// invocation rather than selecting a potentially different runtime again.
function preflightWpCodeboxCommand(command, options = {}) {
  const requiredVersion = options.requiredVersion || REQUIRED_WP_CODEBOX_VERSION;
  const [binary, ...args] = Array.isArray(command) ? command : [];
  const selected = { path: binary || '', source: 'resolved-command', available: Boolean(binary) };
  if (!binary) {
    return failure({ selected, candidates: {} }, requiredVersion, '', 'wp_codebox_not_found', 'homeboy extension setup wordpress');
  }
  const result = (options.spawnSync || spawnSync)(binary, [...args, '--version'], {
    encoding: 'utf8', env: options.env || process.env, timeout: options.timeoutMs || 5_000,
  });
  const version = parseVersion(`${result.stdout || ''}\n${result.stderr || ''}`);
  if (result.error || result.status !== 0 || !version) {
    return failure({ selected, candidates: {} }, requiredVersion, version, 'wp_codebox_version_probe_failed', 'homeboy extension setup wordpress');
  }
  if (compareVersions(version, requiredVersion) < 0) {
    return failure({ selected, candidates: {} }, requiredVersion, version, 'wp_codebox_version_too_old', 'homeboy extension setup wordpress');
  }
  return { ready: true, required_version: requiredVersion, selected: { ...selected, version }, candidates: {}, remediation: '' };
}

function failure(selection, requiredVersion, version, reason, remediation) {
  return { ready: false, required_version: requiredVersion, selected: { ...selection.selected, version }, candidates: selection.candidates, reason, remediation };
}

function candidate(value, source, options) {
  const resolved = resolveCandidate(value, options);
  return { path: resolved || displayCandidate(value, options), source, available: Boolean(resolved) };
}

function emptyCandidate(source) {
  return { path: '', source, available: false };
}

function resolveCandidate(value, options) {
  if (!value) return '';
  const candidatePath = expandHome(String(value), options.env || process.env);
  if (!path.isAbsolute(candidatePath) && !candidatePath.includes(path.sep)) {
    return resolvePathCommand(candidatePath, options.env || process.env, options);
  }
  const filePath = path.resolve(candidatePath);
  return executableFile(filePath, options) ? filePath : '';
}

function displayCandidate(value, options) {
  if (!value) return '';
  return path.resolve(expandHome(String(value), options.env || process.env));
}

function resolvePathCommand(command, env, options) {
  for (const directory of String(env.PATH || '').split(path.delimiter)) {
    const resolved = path.resolve(directory || '.', command);
    if (executableFile(resolved, options)) return resolved;
  }
  return '';
}

function executableFile(filePath, options = {}) {
  try {
    const fileSystem = options.fs || fs;
    if (!fileSystem.statSync(filePath).isFile()) return false;
    if (/\.(?:js|cjs|mjs)$/.test(filePath)) return true;
    fileSystem.accessSync(filePath, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function wpCodeboxCommand(bin) {
  return /\.(?:js|cjs|mjs)$/.test(bin) ? { command: process.execPath, args: [bin] } : { command: bin, args: [] };
}

function parseVersion(output) {
  const match = String(output || '').match(/\bv?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\b/);
  const version = match ? `${match[1]}.${match[2]}.${match[3]}${match[4] ? `-${match[4]}` : ''}` : '';
  return semver(version) ? version : '';
}

function compareVersions(left, right) {
  const leftVersion = semver(left);
  const rightVersion = semver(right);
  if (!leftVersion || !rightVersion) return 0;
  const leftParts = leftVersion.core;
  const rightParts = rightVersion.core;
  for (let index = 0; index < 3; index += 1) {
    if (leftParts[index] !== rightParts[index]) return leftParts[index] - rightParts[index];
  }
  if (!leftVersion.prerelease.length || !rightVersion.prerelease.length) {
    return leftVersion.prerelease.length ? -1 : rightVersion.prerelease.length ? 1 : 0;
  }
  const length = Math.max(leftVersion.prerelease.length, rightVersion.prerelease.length);
  for (let index = 0; index < length; index += 1) {
    const leftPart = leftVersion.prerelease[index];
    const rightPart = rightVersion.prerelease[index];
    if (leftPart === undefined) return -1;
    if (rightPart === undefined) return 1;
    if (leftPart === rightPart) continue;
    const leftNumeric = /^\d+$/.test(leftPart);
    const rightNumeric = /^\d+$/.test(rightPart);
    if (leftNumeric && rightNumeric) return Number(leftPart) - Number(rightPart);
    if (leftNumeric) return -1;
    if (rightNumeric) return 1;
    return leftPart < rightPart ? -1 : 1;
  }
  return 0;
}

function semver(value) {
  const match = String(value).match(/^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/);
  if (!match) return null;
  const core = match.slice(1, 4);
  const prerelease = match[4] ? match[4].split('.') : [];
  if (!core.every((part) => /^(0|[1-9]\d*)$/.test(part)) || prerelease.some((part) => /^\d+$/.test(part) && !/^(0|[1-9]\d*)$/.test(part))) return null;
  return { core: core.map(Number), prerelease };
}

function firstValue(...values) {
  return values.find((value) => value !== undefined && value !== null && value !== '') || '';
}

function expandHome(value, env) {
  return value.startsWith('~/') ? path.join(env.HOME || '', value.slice(2)) : value;
}

function settingsFromEnv(env) {
  try {
    const parsed = JSON.parse(env.HOMEBOY_SETTINGS_JSON || '{}');
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

module.exports = {
  REQUIRED_WP_CODEBOX_VERSION,
  compareVersions,
  parseVersion,
  preflightWpCodeboxCommand,
  preflightWpCodeboxRuntime,
  selectWpCodeboxRuntime,
  wpCodeboxCommand,
};
