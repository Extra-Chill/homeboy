'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');

const { minimum_version: REQUIRED_WP_CODEBOX_VERSION } = require('../wordpress.json').wp_codebox;

const BROWSER_PREVIEW_SCHEMA = 'wp-codebox/browser-contained-site-open/v1';
const MANAGED_IDENTITY_SCHEMA = 'homeboy/wp-codebox-managed-runtime-identity/v1';
const DEFAULT_DESCRIPTOR_PROBE_TIMEOUT_MS = 30_000;

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
    installedExtensionSettingDefaults(options, env).wp_codebox_bin,
  );
  const installDir = env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.join(env.HOME || os.homedir(), '.cache', 'homeboy', 'wp-codebox');
  const managedSource = path.join(installDir, 'source');
  const managed = path.join(managedSource, 'packages', 'cli', 'dist', 'index.js');
  const updating = managedCacheUpdateInProgress(installDir, options);
  const packaged = packagedWpCodeboxBin(env, options);
  const pathCandidate = resolvePathCommand('wp-codebox', env, options);
  const candidates = {
    configured: candidate(configured, configured && sameFile(expandHome(String(configured), env), managed, options) ? 'managed' : configuredRuntimeSource(options, env, settings), options),
    packaged: candidate(packaged, 'packaged', options),
    managed: candidate(managed, 'managed', options),
    path: candidate(pathCandidate, 'path', options),
  };
  // A configured runtime is an exact pin. An incomplete managed source cache
  // is also a Homeboy-owned prerequisite, not permission to use PATH instead.
  const selected = configured
    ? candidates.configured
    : candidates.packaged.available
      ? candidates.packaged
      : updating
        ? { ...candidates.managed, source: 'managed-updating', available: false }
      : directoryExists(managedSource, options)
      ? candidates.managed
      : [candidates.managed, candidates.path].find((entry) => entry.available) || emptyCandidate('');
  return { candidates, selected, updating };
}

function preflightWpCodeboxRuntime(options = {}) {
  const selection = selectWpCodeboxRuntime(options);
  const requiredVersion = options.requiredVersion || REQUIRED_WP_CODEBOX_VERSION;
  if (!selection.selected.path) {
    return failure(selection, requiredVersion, '', 'wp_codebox_not_found', 'homeboy extension setup wordpress');
  }
  if (selection.updating && !configuredRuntime(options)) {
    return failure(selection, requiredVersion, '', 'wp_codebox_managed_updating', 'retry after the managed WP Codebox cache update completes');
  }
  if (!selection.selected.available) {
    return failure(selection, requiredVersion, '', `wp_codebox_${selection.selected.source}_binary_missing`, 'homeboy extension setup wordpress');
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
	const descriptorProbe = probeWpCodeboxRuntimeDescriptorResult(selection.selected.path, options);
	if (descriptorProbe.reason) {
		return failure(selection, requiredVersion, version, descriptorProbe.reason, 'homeboy extension setup wordpress');
	}
	if (!browserPreviewContractAvailable(descriptorProbe.descriptor)) {
    return failure(selection, requiredVersion, version, 'wp_codebox_browser_preview_capability_missing', 'homeboy extension setup wordpress');
  }
  if (selection.selected.source === 'managed' && !managedIdentityMatches(selection.selected.path, options)) {
    return failure(selection, requiredVersion, version, 'wp_codebox_managed_source_identity_invalid', 'homeboy extension setup wordpress');
  }
  return { ready: true, required_version: requiredVersion, selected: { ...selection.selected, version }, candidates: selection.candidates, remediation: '' };
}

// Consumers that received an already-resolved argv must validate that exact
// invocation rather than selecting a potentially different runtime again.
function preflightWpCodeboxCommand(command, options = {}) {
  const requiredVersion = options.requiredVersion || REQUIRED_WP_CODEBOX_VERSION;
  const [binary, ...args] = Array.isArray(command) ? command : [];
  const managed = managedCommandBinary(binary, args, options);
  const selected = { path: managed || binary || '', source: managed ? 'managed' : 'resolved-command', available: Boolean(binary) };
  if (!binary) {
    return failure({ selected, candidates: {} }, requiredVersion, '', 'wp_codebox_not_found', 'homeboy extension setup wordpress');
  }
  if (managed && managedCacheUpdateInProgress(managedInstallDir(options), options)) {
    return failure({ selected, candidates: {} }, requiredVersion, '', 'wp_codebox_managed_updating', 'retry after the managed WP Codebox cache update completes');
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
	const descriptorProbe = probeWpCodeboxRuntimeDescriptorResult(binary, options, args);
	if (descriptorProbe.reason) {
		return failure({ selected, candidates: {} }, requiredVersion, version, descriptorProbe.reason, 'homeboy extension setup wordpress');
	}
	if (!browserPreviewContractAvailable(descriptorProbe.descriptor)) {
    return failure({ selected, candidates: {} }, requiredVersion, version, 'wp_codebox_browser_preview_capability_missing', 'homeboy extension setup wordpress');
  }
  if (managed && !managedIdentityMatches(managed, options)) {
    return failure({ selected, candidates: {} }, requiredVersion, version, 'wp_codebox_managed_source_identity_invalid', 'homeboy extension setup wordpress');
  }
  return { ready: true, required_version: requiredVersion, selected: { ...selected, version }, candidates: {}, remediation: '' };
}

function managedCommandBinary(binary, args, options = {}) {
  const env = options.env || process.env;
  const installDir = env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.join(env.HOME || os.homedir(), '.cache', 'homeboy', 'wp-codebox');
  const managed = path.join(installDir, 'source', 'packages', 'cli', 'dist', 'index.js');
  return [binary, ...args].some((entry) => sameFile(entry, managed, options)) ? managed : '';
}

function managedInstallDir(options = {}) {
  const env = options.env || process.env;
  return env.HOMEBOY_WP_CODEBOX_INSTALL_DIR || path.join(env.HOME || os.homedir(), '.cache', 'homeboy', 'wp-codebox');
}

function managedCacheUpdateInProgress(installDir, options = {}) {
  try {
    return (options.fs || fs).statSync(path.join(installDir, 'source.update-lock')).isDirectory();
  } catch {
    return false;
  }
}

function configuredRuntime(options = {}) {
  const env = options.env || process.env;
  const settings = { ...settingsFromEnv(env), ...(options.settings || {}) };
  return Boolean(firstValue(options.bin, options.runtimeBin, options.runtime_bin, options.wpCodeboxBin, options.wp_codebox_bin, env.HOMEBOY_WP_CODEBOX_BIN, env.WP_CODEBOX_BIN, env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN, settings.runtime_bin, settings.wp_codebox_bin, settings.wpCodeboxBin, installedExtensionSettingDefaults(options, env).wp_codebox_bin));
}

function configuredRuntimeSource(options = {}, env = process.env, settings = settingsFromEnv(env)) {
  if (firstValue(options.bin, options.runtimeBin, options.runtime_bin, options.wpCodeboxBin, options.wp_codebox_bin)) return 'explicit';
  if (firstValue(env.HOMEBOY_WP_CODEBOX_BIN, env.WP_CODEBOX_BIN, env.HOMEBOY_SETTINGS_WP_CODEBOX_BIN)) return 'env';
  if (firstValue(settings.runtime_bin, settings.wp_codebox_bin, settings.wpCodeboxBin)) return 'settings';
  return 'manifest-default';
}

function sameFile(left, right, options = {}) {
  try {
    const fileSystem = options.fs || fs;
    const leftStat = fileSystem.statSync(left);
    const rightStat = fileSystem.statSync(right);
    if (leftStat.dev === rightStat.dev && leftStat.ino === rightStat.ino) return true;
    return fileSystem.realpathSync(left) === fileSystem.realpathSync(right);
  } catch {
    return false;
  }
}

function packagedWpCodeboxBin(env, options = {}) {
  const component = firstValue(env.HOMEBOY_WP_CODEBOX_RUNTIME_COMPONENT, env.WP_CODEBOX_RUNTIME_COMPONENT);
  if (!component) return '';
  const resolved = path.resolve(expandHome(component, env));
  const candidates = [
    path.resolve(resolved, '..', 'cli', 'dist', 'index.js'),
    path.resolve(resolved, '..', '..', 'packages', 'cli', 'dist', 'index.js'),
  ];
  return candidates.find((entry) => executableFile(entry, options)) || '';
}

// This is deliberately a low-level probe. Higher-level descriptor consumers
// must preflight their selected runtime and exact argv before calling it.
function probeWpCodeboxRuntimeDescriptor(bin, options = {}, prefixArgs = []) {
	return probeWpCodeboxRuntimeDescriptorResult(bin, options, prefixArgs).descriptor;
}

function probeWpCodeboxRuntimeDescriptorResult(bin, options = {}, prefixArgs = []) {
  const invocation = wpCodeboxCommand(bin);
  const result = (options.spawnSync || spawnSync)(invocation.command, [...invocation.args, ...prefixArgs, 'runtime', 'descriptor', '--json'], {
		encoding: 'utf8', env: options.env || process.env, timeout: options.descriptorTimeoutMs || options.timeoutMs || DEFAULT_DESCRIPTOR_PROBE_TIMEOUT_MS,
  });
	if (result.error?.code === 'ETIMEDOUT') return { descriptor: null, reason: 'wp_codebox_runtime_descriptor_probe_timed_out' };
	if (result.error || result.status !== 0) return { descriptor: null, reason: 'wp_codebox_runtime_descriptor_probe_failed' };
  try {
		return { descriptor: JSON.parse(result.stdout), reason: '' };
  } catch {
		return { descriptor: null, reason: 'wp_codebox_runtime_descriptor_invalid' };
  }
}

function browserPreviewContractAvailable(descriptor) {
  return descriptor?.schema === 'wp-codebox/runtime-descriptor/v1'
    && descriptor?.contractManifest?.schemas?.runtimeBoundary?.browserContainedSiteOpen === BROWSER_PREVIEW_SCHEMA;
}

function managedIdentityMatches(bin, options = {}) {
  const source = path.resolve(bin, '../../../../');
  const identityPath = path.join(source, '.homeboy-runtime-identity.json');
  let identity;
  try {
    identity = JSON.parse((options.fs || fs).readFileSync(identityPath, 'utf8'));
  } catch {
    return false;
  }
  if (identity?.schema !== MANAGED_IDENTITY_SCHEMA || !/^[0-9a-f]{40}$/i.test(identity?.source_sha || '') || !/^[0-9a-f]{64}$/i.test(identity?.cli_sha256 || '') || !Array.isArray(identity.required_capabilities) || !identity.required_capabilities.includes(BROWSER_PREVIEW_SCHEMA)) return false;
  const result = (options.spawnSync || spawnSync)('git', ['-C', source, 'rev-parse', 'HEAD'], { encoding: 'utf8', timeout: options.timeoutMs || 5_000 });
  if (result.status !== 0 || result.stdout.trim() !== identity.source_sha) return false;
  try {
    const executable = (options.fs || fs).readFileSync(bin);
    return crypto.createHash('sha256').update(executable).digest('hex') === identity.cli_sha256;
  } catch {
    return false;
  }
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

function directoryExists(directoryPath, options = {}) {
  try {
    return (options.fs || fs).statSync(directoryPath).isDirectory();
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

function installedExtensionSettingDefaults(options = {}, env = process.env) {
  const manifestPath = options.extension_manifest_path || env.HOMEBOY_EXTENSION_MANIFEST_PATH || path.resolve(__dirname, '..', 'wordpress.json');
  try {
    const manifest = JSON.parse((options.fs || fs).readFileSync(manifestPath, 'utf8'));
    return Object.fromEntries((manifest.settings || [])
      .filter((setting) => setting?.id && setting.default !== undefined && setting.default !== '')
      .map((setting) => [setting.id, setting.default]));
  } catch {
    return {};
  }
}

function homeboySettings(env = process.env) {
  return settingsFromEnv(env);
}

function resolveWpCodeboxIdentity(options = {}) {
  const selection = selectWpCodeboxRuntime(options).selected;
  if (!selection.path) {
    throw new Error('WP Codebox binary is not configured. Set wp_codebox_bin in the installed WordPress extension manifest or HOMEBOY_WP_CODEBOX_BIN.');
  }
  return { bin: selection.path, invocation: wpCodeboxCommand(selection.path), selectionSource: selection.source };
}

function resolveReadyWpCodeboxRuntime(options = {}) {
  const env = { ...process.env, ...(options.env || {}) };
  const settings = { ...settingsFromEnv(env), ...(options.settings || {}) };
  const runtime = preflightWpCodeboxRuntime({ ...options, env, settings });
  if (!runtime.ready) {
    throw new Error(`WP Codebox runtime preflight failed: ${runtime.reason}; required >=${runtime.required_version}, observed ${runtime.selected.version || 'unavailable'} at ${runtime.selected.path || 'no executable'}. Run ${runtime.remediation}.`);
  }
  const invocation = wpCodeboxCommand(runtime.selected.path);
  const command = preflightWpCodeboxCommand([invocation.command, ...invocation.args], { ...options, env, settings });
  if (!command.ready) {
    throw new Error(`WP Codebox command preflight failed: ${command.reason}; required >=${command.required_version}, observed ${command.selected.version || 'unavailable'} at ${command.selected.path || 'no executable'}. Run ${command.remediation}.`);
  }
  return { ...runtime, invocation };
}

module.exports = {
  REQUIRED_WP_CODEBOX_VERSION,
  BROWSER_PREVIEW_SCHEMA,
	DEFAULT_DESCRIPTOR_PROBE_TIMEOUT_MS,
  MANAGED_IDENTITY_SCHEMA,
  compareVersions,
  parseVersion,
	installedExtensionSettingDefaults,
	homeboySettings,
	preflightWpCodeboxCommand,
	preflightWpCodeboxRuntime,
	probeWpCodeboxRuntimeDescriptor,
	selectWpCodeboxRuntime,
	resolveWpCodeboxIdentity,
	resolveReadyWpCodeboxRuntime,
	wpCodeboxCommand,
};
