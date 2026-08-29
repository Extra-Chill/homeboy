#!/usr/bin/env node
/**
 * External dependencies
 */
import { createWriteStream } from 'node:fs';
import { access, copyFile, lstat, mkdir, mkdtemp, readdir, readFile, realpath, rename, rm, unlink, writeFile } from 'node:fs/promises';
import { hostname, tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
/**
 * Internal dependencies
 */
import { createTimeoutLineRedactor, recipeRunProjection, readBoundedText, wpCodeboxTimeoutDiagnostics } from '../lib/wp-codebox-timeout-diagnostics.mjs';
import { configuredWpCodeboxPhpunitTimeoutSeconds } from '../lib/wp-codebox-phpunit-timeout.mjs';
import { configuredWpCodeboxRuntimeCrashGraceSeconds, createWpCodeboxRuntimeCrashDetector } from '../lib/wp-codebox-runtime-crash.mjs';

const require = createRequire(import.meta.url);
const { preflightWpCodeboxCommand } = require('../../lib/wp-codebox-runtime-selection.js');

const settings = parseSettings(process.env.HOMEBOY_SETTINGS_JSON);
const discoveryOnly = process.env.HOMEBOY_WORDPRESS_PHPUNIT_DISCOVERY_ONLY === '1';
const phpunitTimeoutSeconds = configuredWpCodeboxPhpunitTimeoutSeconds(process.env, settings);
const runtimeCrashGraceSeconds = configuredWpCodeboxRuntimeCrashGraceSeconds(process.env, settings);
const componentPath = required(process.env.HOMEBOY_COMPONENT_PATH, 'HOMEBOY_COMPONENT_PATH');
const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const extensionRoot = path.resolve(scriptDirectory, '../..');
const harnessSource = path.join(extensionRoot, 'vendor');
// Resolved WP Codebox invocation as an argv array, e.g.
// ['node', '/abs/packages/cli/dist/index.js'] or ['/abs/bin/wp-codebox'].
// Declared with the module's other top-level bindings: the statements below run
// before any later declaration is initialized.
let wpCodeboxCommandCache;
const NATIVE_MARIADB_CAPABILITY = 'runtime-service:mysql:native:mariadb';
const RUNTIME_SERVICE_CAPABILITIES_SCHEMA = 'wp-codebox/runtime-service-capabilities/v1';
const slug = process.env.HOMEBOY_COMPONENT_ID || process.env.COMPONENT_ID || path.basename(componentPath);
const root = settings.wp_codebox_source_root || componentPath;
const subpath = settings.wp_codebox_source_subpath || undefined;
const pluginSourceDirectory = subpath ? path.join(root, subpath) : root;
const phpunitProfile = await resolvePhpunitProfile(settings, pluginSourceDirectory, slug);
const phpunitBootstrap = resolvePhpunitBootstrap(settings, phpunitProfile);
const topology = await resolveWordPressTopology(settings, pluginSourceDirectory);
const databaseService = resolveDatabaseService(settings, process.env);
if (!discoveryOnly) {
  requireDatabaseServiceCapability(databaseService);
  runPrepareSteps(settings.wp_codebox_prepare_steps, pluginSourceDirectory);
}
const directory = await mkdtemp(path.join(tmpdir(), 'homeboy-wp-codebox-phpunit-'));
const optionsPath = path.join(directory, 'options.json');
const recipePath = path.join(directory, 'recipe.json');
const artifacts = process.env.HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR || path.join(directory, 'artifacts');
const runArtifacts = path.join(artifacts, `wp-codebox-phpunit.${process.pid}`);
// Validation dependencies are external checkouts nobody built, so the recipe
// declares that they may need Composer preparation. It does not decide whether
// they do: WP Codebox owns the detection (no composer.json, or an existing
// vendor/autoload.php, are both no-ops) and owns running composer. This runner
// never inspects a dependency's vendor state and never shells out to composer.
//
// The component under review is deliberately absent from this: its vendor/ is
// produced by the declared dependency-materialization phase before the runner
// is ever invoked.
const dependencies = (discoveryOnly ? [] : await dependencyPaths(settings, [componentPath, pluginSourceDirectory])).map((source) => {
  const dependencySlug = path.basename(source).replace(/@[^/]+$/, '');
  return { source, slug: dependencySlug, sandboxDirectory: sandboxPluginDirectory(dependencySlug), composer: 'install' };
});
const selectedTestFile = (process.env.HOMEBOY_WORDPRESS_PHPUNIT_TEST_FILE || '').trim();
// Host-relative form is what an operator reads in the diagnosis and provenance
// artifacts. The sandbox-absolute form is what WP Codebox can actually match.
// Keep both, deliberately, and hand each consumer the one it can use.
const changedTestFiles = phpunitChangedTestFiles();
const changedTestFileScope = resolveChangedTestFileScope(changedTestFiles);
// Validation dependencies activate before the plugin under review so the
// target's own activation hooks observe the topology it declares. The target
// is activated last and never omitted: an inactive target is excluded from WP
// Codebox's activation phase and from Composer autoloader preloading, which
// yields a sandbox that reports zero executed tests.
const activationPlan = [
  ...dependencies.map(({ source, slug: dependencySlug, composer }) => ({ role: 'validation-dependency', source, slug: dependencySlug, composer })),
  { role: 'target', source: root, sourceSubpath: subpath, slug },
];
if (!discoveryOnly) {
  await requireHarness(harnessSource);
}
const options = clean({
  wordpressVersion: settings.wordpress_runtime_version,
  phpVersion: settings.wordpress_runtime_php_version,
  databaseType: discoveryOnly ? 'sqlite' : settings.database_type,
  services: !discoveryOnly && databaseService ? [databaseService.service] : undefined,
  pluginSlug: slug,
  extra_plugins: activationPlan.map(({ role, ...plugin }) => clean({ ...plugin, activate: true })),
  dependencyMounts: [...new Set([sandboxPluginDirectory(slug), ...dependencies.map(({ sandboxDirectory }) => sandboxDirectory)])],
  selectedTestFile: discoveryOnly ? '' : selectedTestFile,
  changedTestFiles: discoveryOnly ? [] : changedTestFileScope.sandbox,
  discoveryOnly,
  testRoot: phpunitProfile.testRoot,
  phpunitXml: phpunitProfile.config,
  cwd: phpunitProfile.cwd,
  phpunitArgs: discoveryOnly ? [] : process.argv.slice(2),
  env: settings.bench_env,
  wpConfigDefines: settings.wp_config_defines,
  autoloadFile: '/wp-codebox-vendor/autoload.php',
  bootstrapMode: phpunitBootstrap.mode,
  projectBootstrap: phpunitBootstrap.projectBootstrap,
  multisite: discoveryOnly ? false : topology.multisite,
  preloadFiles: settings.wp_codebox_phpunit_preload_files,
  mounts: [...canonicalMounts(settings.wp_codebox_phpunit_mounts), ...(!discoveryOnly ? [{ source: harnessSource, target: '/wp-codebox-vendor', mode: 'readonly' }] : [])],
});

try {
  const runtimePreflight = preflightWpCodeboxCommand(wpCodeboxCommand());
  if (!runtimePreflight.ready) {
    throw new Error(`WP Codebox runtime preflight failed: ${runtimePreflight.reason}; required >=${runtimePreflight.required_version}, observed ${runtimePreflight.selected.version || 'unavailable'} at ${runtimePreflight.selected.path || 'no executable'}. Run homeboy extension setup wordpress.`);
  }
  // Every other WordPress test backend announces itself (standalone-php,
  // node-test, package-script, and the core-dev wp-codebox runner). ca924281
  // replaced this runner's shell implementation with the Node adapter and the
  // banner went with it, leaving the PHPUnit path as the only one whose run
  // logs do not say what executed them.
  if (!discoveryOnly) {
    process.stdout.write('Running PHPUnit tests via WP Codebox...\n');
    process.stdout.write(`  Plugin: ${slug} (${pluginSourceDirectory})\n`);
    process.stdout.write('  Backend: wp-codebox\n');
  }
  await writeFile(optionsPath, `${JSON.stringify(options)}\n`);
  run(['recipe', 'build', 'phpunit', '--options', optionsPath, '--output', recipePath]);
  if (!discoveryOnly) {
    await applyDatabaseServiceAuthorization(recipePath);
  }
  await mkdir(runArtifacts, { recursive: true });
  if (!discoveryOnly) {
    await persistRecipeEvidence(runArtifacts, options, recipePath, phpunitProfile, dependencies);
  }
  const executionArgs = ['recipe-run', '--recipe', recipePath, '--artifacts', runArtifacts, '--json'];
  if (!discoveryOnly && databaseService?.boundary) {
    executionArgs.push('--approve-external-service-writes', '--policy', JSON.stringify(databaseService.policy));
  }
  const execution = await runCaptured(executionArgs, phpunitTimeoutSeconds);
  if (discoveryOnly) {
    try {
      if (execution.status !== 0) {
        const stdout = (await readBoundedText(execution.stdoutPath, 8 * 1024)).text.trim();
        const stderr = (await readBoundedText(execution.stderrPath, 8 * 1024)).text.trim();
        const diagnostic = [stdout, stderr].filter(Boolean).join('\n');
        throw new Error(`WP Codebox PHPUnit discovery failed with exit ${execution.status}${diagnostic ? `: ${diagnostic}` : '.'}`);
      }
      process.stdout.write(`${JSON.stringify(await readDiscoveryResult(execution.stdoutPath))}\n`);
    } finally {
      await rm(runArtifacts, { recursive: true, force: true });
    }
  } else {
    const artifactStatus = await handoffArtifacts(runArtifacts, execution);
    if (execution.status !== 0 || !['passed', 'skipped'].includes(artifactStatus)) {
      process.exitCode = execution.timedOut ? 124 : (execution.status || 1);
    } else {
      process.stdout.write('WP Codebox test run complete.\n');
    }
  }
} finally {
  await rm(directory, { recursive: true, force: true });
}
async function readDiscoveryResult(stdoutPath) {
  const payload = JSON.parse(await readFile(stdoutPath, 'utf8'));
  const execution = Array.isArray(payload?.executions)
    ? payload.executions.find((entry) => entry?.command === 'wordpress.phpunit')
    : undefined;
  const result = JSON.parse(execution?.stdout || 'null');
  if (result?.schema !== 'wp-codebox/phpunit-discovery/v1'
    || result.plugin_slug !== slug
    || !Array.isArray(result.files)
    || result.files.length === 0
    || result.files.some((file) => typeof file !== 'string' || !file.startsWith('/'))) {
    throw new Error('WP Codebox returned an invalid PHPUnit discovery result.');
  }
  return result;
}
async function runCaptured(args, timeoutSeconds) {
  const logsDirectory = path.join(runArtifacts, 'logs');
  const stdoutPath = path.join(logsDirectory, 'recipe-run.stdout.log');
  const stderrPath = path.join(logsDirectory, 'recipe-run.stderr.log');
  await mkdir(logsDirectory, { recursive: true });
  const secretValues = configuredSecretValues();

  return new Promise((resolve, reject) => {
    const [command, ...prefix] = wpCodeboxCommand();
    const child = spawn(command, [...prefix, ...args], { cwd: componentPath, stdio: ['ignore', 'pipe', 'pipe'], detached: true });
    const stdoutFile = createWriteStream(stdoutPath);
    const stderrFile = createWriteStream(stderrPath);
    const stdoutRedactor = createTimeoutLineRedactor(secretValues);
    const stderrRedactor = createTimeoutLineRedactor(secretValues);
    const crashDetector = createWpCodeboxRuntimeCrashDetector();
    let childError;
    let timedOut = false;
    let crashTimer;
    const startedAt = Date.now();
    let stdoutPreview = Buffer.alloc(0);
    const terminate = () => {
      killChildGroup(child, 'SIGTERM');
      // Do not clear or unref this timer when the leader exits: a descendant
      // can ignore SIGTERM after the CLI has been reaped.
      setTimeout(() => killChildGroup(child, 'SIGKILL'), 5000);
    };
    const timer = setTimeout(() => {
      timedOut = true;
      terminate();
    }, timeoutSeconds * 1000);

    // A wasm trap the runtime never claimed will not resolve on its own, so
    // stop paying the full budget for it. The grace window is what keeps this
    // safe: a run that recovers and finishes inside it is left alone.
    const armCrashDeadline = (chunk) => {
      if (crashTimer || runtimeCrashGraceSeconds === 0 || !crashDetector.write(chunk)) {
        return;
      }
      const crash = crashDetector.crash();
      process.stdout.write(`WP Codebox runtime crash detected (${crash.id}): ${crash.message}\n`);
      process.stdout.write(`Terminating the recipe-run in ${runtimeCrashGraceSeconds}s unless it completes first.\n`);
      crashTimer = setTimeout(() => {
        timedOut = true;
        terminate();
      }, runtimeCrashGraceSeconds * 1000);
    };

    child.stdout.on('data', (chunk) => {
      if (stdoutPreview.byteLength < 128 * 1024) {
        stdoutPreview = Buffer.concat([stdoutPreview, chunk.subarray(0, (128 * 1024) - stdoutPreview.byteLength)]);
      }
      armCrashDeadline(chunk);
      chunk = stdoutRedactor.write(chunk);
      stdoutFile.write(chunk);
    });
    child.stderr.on('data', (chunk) => {
      armCrashDeadline(chunk);
      chunk = stderrRedactor.write(chunk);
      stderrFile.write(chunk);
    });
    child.on('error', (error) => { childError = error; });
    child.on('close', (status, signal) => {
      clearTimeout(timer);
      clearTimeout(crashTimer);
      const stdoutTail = stdoutRedactor.end();
      const stderrTail = stderrRedactor.end();
      stdoutFile.write(stdoutTail);
      stderrFile.write(stderrTail);
      stdoutFile.end();
      stderrFile.end();
      Promise.all([
        new Promise((done) => stdoutFile.on('finish', done)),
        new Promise((done) => stderrFile.on('finish', done)),
      ]).then(() => {
        if (childError) {
          reject(childError);
          return;
        }
        const runtimeCrash = crashDetector.crash();
        resolve({
          status: status ?? 1,
          stdoutPath,
          stderrPath,
          stdoutPreview: stdoutPreview.toString('utf8'),
          timedOut,
          runtimeCrash,
          elapsedSeconds: Math.ceil((Date.now() - startedAt) / 1000),
          // A crash that we terminated on is reported as such: naming the
          // budget would describe the symptom and hide the cause.
          termination: { result: terminationResult(runtimeCrash, timedOut), signal, code: status },
        });
      }, reject);
    });
  });
}
function terminationResult(runtimeCrash, timedOut) {
  if (runtimeCrash) {
    return 'runtime_crash';
  }
  return timedOut ? 'timeout' : 'exited';
}
function killChildGroup(child, signal) {
  if (!child || typeof child.pid !== 'number') {
    return;
  }
  try {
    process.kill(-child.pid, signal);
  } catch (error) {
    if (error?.code !== 'ESRCH') {
      try { child.kill(signal); } catch {}
    }
  }
}
function configuredSecretValues() {
  const configured = settings.bench_env && typeof settings.bench_env === 'object' ? settings.bench_env : {};
  const databaseSecretNames = databaseService?.secretEnv || [];
  return Object.entries({ ...process.env, ...configured })
    .filter(([name, value]) => typeof value === 'string' && value.length > 0 && (databaseSecretNames.includes(name) || (/(?:auth|cookie|credential|key|nonce|passw|secret|session|token)/i.test(name) && value.length >= 8)))
    .map(([, value]) => value)
    .sort((left, right) => right.length - left.length);
}
function redactSecrets(text, secretValues = configuredSecretValues()) {
  for (const secret of secretValues) {
    text = text.replaceAll(secret, '[REDACTED]');
  }
  return text;
}

function run(args) {
  const [command, ...prefix] = wpCodeboxCommand();
  const result = spawnSync(command, [...prefix, ...args], { cwd: componentPath, env: environmentWithoutDatabaseAdministration(), encoding: 'utf8', stdio: 'inherit' });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}
function wpCodeboxCommand() {
  if (wpCodeboxCommandCache === undefined) {
    wpCodeboxCommandCache = resolveWpCodeboxCommand();
  }
  return wpCodeboxCommandCache;
}

function resolveWpCodeboxCommand() {
  const exported = parseCommandArgv(process.env.HOMEBOY_WP_CODEBOX_COMMAND_JSON);
  if (exported) {
    return exported;
  }

  const library = path.join(scriptDirectory, '../lib/wp-codebox-paths.sh');
  const result = spawnSync('bash', ['-c', `source "$1" && homeboy_wp_codebox_export_command "\${HOMEBOY_SETTINGS_JSON:-}" && printf '%s' "$HOMEBOY_WP_CODEBOX_COMMAND_JSON"`, 'wp-codebox-resolve', library], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'inherit'] });

  const resolved = result.status === 0 ? parseCommandArgv(result.stdout) : undefined;
  if (!resolved) {
    throw new Error('WP Codebox CLI could not be resolved; see the resolver diagnostics above.');
  }
  return resolved;
}

function parseCommandArgv(value) {
  const parsed = json(value, undefined);
  if (!Array.isArray(parsed) || parsed.length === 0) {
    return undefined;
  }
  if (!parsed.every((entry) => typeof entry === 'string' && entry !== '')) {
    return undefined;
  }
  return parsed;
}

function required(value, name) { if (!value) { throw new Error(`${name} is required`); } return value; }
async function resolveWordPressTopology(configuration, pluginDirectory) {
  const fromEnv = process.env.HOMEBOY_WORDPRESS_MULTISITE;
  if (fromEnv !== undefined && fromEnv !== '') {
    return { multisite: truthy(fromEnv), source: 'environment' };
  }
  if (configuration.wp_codebox_multisite !== undefined) {
    return { multisite: truthy(configuration.wp_codebox_multisite), source: 'setting' };
  }
  return { multisite: await pluginRequiresNetwork(pluginDirectory), source: 'plugin-header' };
}
async function pluginRequiresNetwork(pluginDirectory) {
  if (typeof pluginDirectory !== 'string' || pluginDirectory === '') {
    return false;
  }
  let entries;
  try {
    entries = await readdir(pluginDirectory, { withFileTypes: true });
  } catch {
    return false;
  }
  const candidates = entries
    .filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith('.php'))
    .map((entry) => entry.name);
  for (const candidate of candidates) {
    let header;
    try {
      header = await readFile(path.join(pluginDirectory, candidate), 'utf8');
    } catch {
      continue;
    }
    const block = header.slice(0, 8192);
    if (!/^[\s*#\/]*Plugin Name\s*:/im.test(block)) {
      continue;
    }
    if (/^[\s*#\/]*Network\s*:\s*(true|1|yes|on)\b/im.test(block)) {
      return true;
    }
  }
  return false;
}
function truthy(value) {
  if (typeof value === 'boolean') { return value; }
  if (typeof value === 'number') { return value === 1; }
  if (typeof value !== 'string') { return false; }
  return ['1', 'true', 'yes', 'on'].includes(value.trim().toLowerCase());
}
function parseSettings(value) {
  if (value === undefined || value === '') {
    return {};
  }
  let parsed;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw new Error('HOMEBOY_SETTINGS_JSON must contain a valid JSON object');
  }
  if (!isObject(parsed)) {
    throw new Error('HOMEBOY_SETTINGS_JSON must contain a valid JSON object');
  }
  return parsed;
}
function json(value, fallback) { try { return value ? JSON.parse(value) : fallback; } catch { return fallback; } }
function clean(value) { return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined && entry !== '' && !(Array.isArray(entry) && entry.length === 0))); }
async function dependencyPaths(configuration, primarySources) {
  const configured = Array.isArray(configuration.validation_dependencies) ? configuration.validation_dependencies : [];
  const canonical = (process.env.HOMEBOY_WORDPRESS_DEPENDENCY_PATHS || '').split('\n');
  // The shell resolver owns slug-to-path resolution and exports its final paths
  // here. Settings can still contribute explicit paths when this adapter is run
  // directly, but metadata slugs must not be resolved or rejected a second time.
  const explicit = configured.map((value) => {
    if (typeof value === 'string') { return value; }
    if (isObject(value)) { return value.path || value.local_path || value.source || ''; }
    return '';
  }).filter((value) => path.isAbsolute(value));
  // Preserve explicitly declared paths in recipe provenance while canonicalizing
  // aliases solely for identity and duplicate detection.
  const sources = [...new Set([...explicit, ...canonical].filter((value) => typeof value === 'string' && path.isAbsolute(value)))];
  const resolved = await Promise.all(sources.map(async (source) => {
    try { return { source, canonicalSource: await realpath(source) }; } catch { throw new Error(`Declared WordPress validation dependency sources are unavailable: ${source}`); }
  }));
  const primary = new Set(await Promise.all(primarySources.map(async (source) => {
    try { return await realpath(source); } catch { return path.resolve(source); }
  })));
  const seen = new Set();
  return resolved
    .filter(({ canonicalSource }) => !primary.has(canonicalSource) && !seen.has(canonicalSource) && Boolean(seen.add(canonicalSource)))
    .map(({ source }) => source);
}
function sandboxPluginDirectory(pluginSlug) {
  return `/wordpress/wp-content/plugins/${pluginSlug}`;
}
// The router publishes the PHPUnit-shaped subset of the changed-file scope.
// Fall back to filtering the raw scope so a directly invoked adapter still
// narrows to the selection instead of silently widening to the full suite.
function phpunitChangedTestFiles() {
  const scoped = process.env.HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES;
  const raw = scoped === undefined || scoped === '' ? process.env.HOMEBOY_CHANGED_TEST_FILES || '' : scoped;
  const entries = raw.split('\n').map((entry) => entry.trim()).filter(Boolean);
  const selected = entries.filter((entry) => /(?:Test\.php|\/test-[^/]*\.php)$/.test(`/${entry}`));
  if (scoped !== undefined && scoped !== '') {
    const rejected = entries.filter((entry) => !selected.includes(entry));
    if (rejected.length > 0) {
      throw new Error(`HOMEBOY_WORDPRESS_PHPUNIT_CHANGED_TEST_FILES contains non-PHPUnit paths: ${rejected.join(', ')}`);
    }
  }
  return [...new Set(selected)];
}
function canonicalMounts(value) {
  return Array.isArray(value) ? value : [];
}
// WP Codebox normalizes the changed-file scope and the discovered test files
// against the SAME root, and that root is the PHPUnit test root — not the
// plugin root its parameter name suggests. Its relative-path helper strips that
// root prefix, and its only fallback looks for a literal '/tests/', which a
// component-relative path such as `tests/Unit/FooTest.php` does not contain
// because it has no leading slash.
//
// So a discovered `/wordpress/wp-content/plugins/<slug>/tests/Unit/FooTest.php`
// normalizes to `Unit/FooTest.php` while the requested `tests/Unit/FooTest.php`
// normalizes to itself, and nothing ever matches: `requested=N matched=0` for
// every plugin, on every changed-scope run.
//
// Sending the sandbox-absolute path is the representation that survives, because
// the root prefix strip applies to it whatever the configured test root is.
function resolveChangedTestFileScope(hostRelativeFiles) {
  const mounts = canonicalMounts(settings.wp_codebox_phpunit_mounts)
    .filter((mount) => isObject(mount) && typeof mount.source === 'string' && typeof mount.target === 'string')
    // Longest source first: a nested mount must win over the checkout that
    // contains it.
    .sort((left, right) => right.source.length - left.source.length);

  const sandbox = [];
  const untranslated = [];
  for (const hostRelative of hostRelativeFiles) {
    const sandboxPath = sandboxTestPath(hostRelative, mounts);
    if (sandboxPath === null) {
      // Never drop the entry: an empty scope is WP Codebox's "run everything"
      // signal, and a partial scope would silently skip a selected test.
      // Falling back to the original path keeps it visible to the mismatch
      // diagnosis instead.
      untranslated.push(hostRelative);
      sandbox.push(hostRelative);
      continue;
    }
    sandbox.push(sandboxPath);
  }
  return { hostRelative: hostRelativeFiles, sandbox, untranslated };
}
function sandboxTestPath(hostRelative, mounts) {
  const hostAbsolute = path.resolve(componentPath, hostRelative);
  for (const mount of mounts) {
    const relativeToMount = path.relative(path.resolve(mount.source), hostAbsolute);
    if (relativeToMount !== '' && !relativeToMount.startsWith('..') && !path.isAbsolute(relativeToMount)) {
      return `${mount.target.replace(/\/+$/, '')}/${relativeToMount.split(path.sep).join('/')}`;
    }
  }
  // The plugin mount is implicit: wp_codebox_source_subpath means the component
  // root and the mounted plugin root are not the same directory, so the subpath
  // prefix must come off before the sandbox root goes on.
  const relativeToPlugin = path.relative(pluginSourceDirectory, hostAbsolute);
  if (relativeToPlugin === '' || relativeToPlugin.startsWith('..') || path.isAbsolute(relativeToPlugin)) {
    return null;
  }
  return `${sandboxPluginDirectory(slug)}/${relativeToPlugin.split(path.sep).join('/')}`;
}
function resolveDatabaseService(configuration, environment) {
  const value = configuration.wp_codebox_database_service;
  if (value === undefined || value === null || (isObject(value) && Object.keys(value).length === 0)) {
    return undefined;
  }
  if (!isObject(value)) {
    throw new Error('wp_codebox_database_service must be an object');
  }
  const unknownKeys = Object.keys(value).filter((key) => !['provider', 'engine', 'allowed_hosts', 'secret_env'].includes(key));
  if (unknownKeys.length > 0) {
    throw new Error(`wp_codebox_database_service contains unsupported fields: ${unknownKeys.join(', ')}`);
  }
  if (configuration.database_type !== 'mysql') {
    throw new Error('wp_codebox_database_service requires database_type=mysql');
  }
  if (!['external', 'native'].includes(value.provider)) {
    throw new Error('wp_codebox_database_service.provider must be external or native');
  }
  if (value.provider === 'native') {
    const nativeUnknownKeys = Object.keys(value).filter((key) => !['provider', 'engine'].includes(key));
    if (nativeUnknownKeys.length > 0) {
      throw new Error(`wp_codebox_database_service native provider contains unsupported fields: ${nativeUnknownKeys.join(', ')}`);
    }
    if (value.engine !== 'mariadb') {
      throw new Error('wp_codebox_database_service native provider requires engine=mariadb');
    }
    return {
      service: {
        id: 'wordpress-database',
        kind: 'mysql',
        configuration: { provider: 'native', engine: 'mariadb' },
        outputs: { host: 'DB_HOST', port: 'DB_PORT', username: 'DB_USER', password: 'DB_PASSWORD', database: 'DB_NAME' },
      },
      secretEnv: [],
      requiredCapability: NATIVE_MARIADB_CAPABILITY,
    };
  }
  if (value.engine !== undefined && !['mysql', 'mariadb'].includes(value.engine)) {
    throw new Error('wp_codebox_database_service.engine must be mysql or mariadb');
  }
  if (!Array.isArray(value.allowed_hosts) || value.allowed_hosts.length === 0 || !value.allowed_hosts.every((host) => typeof host === 'string' && /^[a-z0-9.-]+(?::\d+)?$/i.test(host))) {
    throw new Error('wp_codebox_database_service.allowed_hosts must contain hostnames with optional ports');
  }
  if (!isObject(value.secret_env)) {
    throw new Error('wp_codebox_database_service.secret_env must contain provider secret environment references');
  }
  const unknownSecretFields = Object.keys(value.secret_env).filter((key) => !['host', 'port', 'username', 'password'].includes(key));
  if (unknownSecretFields.length > 0) {
    throw new Error(`wp_codebox_database_service.secret_env contains unsupported fields: ${unknownSecretFields.join(', ')}`);
  }
  const requiredSecretFields = ['host', 'username', 'password'];
  const missingSecretFields = requiredSecretFields.filter((field) => value.secret_env[field] === undefined);
  if (missingSecretFields.length > 0) {
    throw new Error(`wp_codebox_database_service.secret_env is missing required references: ${missingSecretFields.join(', ')}`);
  }
  for (const [field, name] of Object.entries(value.secret_env)) {
    if (typeof name !== 'string' || !/^[A-Z_][A-Z0-9_]*$/.test(name)) {
      throw new Error('wp_codebox_database_service.secret_env must map provider fields to environment variable names');
    }
    if (typeof environment[name] !== 'string' || (field !== 'password' && environment[name].trim() === '')) {
      throw new Error(`wp_codebox_database_service secret environment variable is unavailable: ${name}. Export the configured variable, or refresh the installed WordPress extension/rig package with homeboy init; inspect the active install with homeboy extension show wordpress and readlink ~/.config/homeboy/extensions/wordpress.`);
    }
  }
  const secretEnv = [...new Set(Object.values(value.secret_env))];
  const allowedHosts = [...new Set(value.allowed_hosts.map((host) => host.toLowerCase()))];
  const benchEnv = isObject(configuration.bench_env) ? configuration.bench_env : {};
  const collisions = secretEnv.filter((name) => Object.hasOwn(benchEnv, name));
  if (collisions.length > 0) {
    throw new Error(`bench_env must not expose database service administration environment: ${collisions.join(', ')}`);
  }
  return {
    service: {
      id: 'wordpress-database',
      kind: 'mysql',
      configuration: {
        provider: value.provider,
        externalService: 'wordpress-database-administration',
        ...(value.engine ? { engine: value.engine } : {}),
        hostEnv: value.secret_env.host,
        ...(value.secret_env.port ? { portEnv: value.secret_env.port } : {}),
        usernameEnv: value.secret_env.username,
        passwordEnv: value.secret_env.password,
      },
      outputs: { host: 'DB_HOST', port: 'DB_PORT', username: 'DB_USER', password: 'DB_PASSWORD', database: 'DB_NAME' },
    },
    secretEnv,
    boundary: {
      id: 'wordpress-database-administration',
      environment: 'external',
      allowedHosts,
      writes: 'allowed-with-approval',
    },
    policy: {
      network: { allowHosts: allowedHosts },
      filesystem: 'readwrite-mounts',
      commands: ['inspect-mounted-inputs', 'wordpress.run-php', 'wordpress.phpunit'],
      secrets: 'connector-scoped',
      approvals: 'on-write',
    },
  };
}
function requireDatabaseServiceCapability(service) {
  if (!service?.requiredCapability) {
    return;
  }
  const [command, ...prefix] = wpCodeboxCommand();
  const result = spawnSync(command, [...prefix, 'runtime', 'descriptor', '--json'], {
    cwd: componentPath,
    env: environmentWithoutDatabaseAdministration(),
    encoding: 'utf8',
  });
  let descriptor;
  try {
    descriptor = result.status === 0 ? JSON.parse(result.stdout) : null;
  } catch {
    descriptor = null;
  }
  const runtimeServices = descriptor?.contractManifest?.capabilities?.runtimeServices;
  if (
    descriptor?.schema !== 'wp-codebox/runtime-descriptor/v1'
    || !Array.isArray(descriptor.capabilities)
    || !descriptor.capabilities.includes(service.requiredCapability)
    || runtimeServices?.schema !== RUNTIME_SERVICE_CAPABILITIES_SCHEMA
    || !Array.isArray(runtimeServices.capabilities)
    || !runtimeServices.capabilities.includes(service.requiredCapability)
  ) {
    throw new Error('WP Codebox runtime does not advertise the required native MariaDB service capability');
  }
}
function isObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function environmentWithoutDatabaseAdministration() {
  const environment = { ...process.env };
  for (const name of databaseService?.secretEnv || []) {
    delete environment[name];
  }
  return environment;
}
async function applyDatabaseServiceAuthorization(target) {
  if (!databaseService?.boundary) {
    return;
  }
  let recipe;
  try {
    recipe = JSON.parse(await readFile(target, 'utf8'));
  } catch {
    throw new Error('WP Codebox produced an invalid PHPUnit recipe');
  }
  if (!isObject(recipe) || !isObject(recipe.inputs) || !Array.isArray(recipe.inputs.services)) {
    throw new Error('WP Codebox PHPUnit recipe omitted configured runtime services');
  }
  const service = recipe.inputs.services.find((candidate) => candidate?.id === databaseService.service.id);
  if (!isObject(service) || !isObject(service.configuration)) {
    throw new Error('WP Codebox PHPUnit recipe omitted the configured database service');
  }
  service.configuration.externalService = databaseService.boundary.id;
  const boundaries = Array.isArray(recipe.inputs.externalServices) ? recipe.inputs.externalServices : [];
  recipe.inputs.externalServices = [...boundaries.filter((boundary) => boundary?.id !== databaseService.boundary.id), databaseService.boundary];
  await writeFile(target, `${JSON.stringify(recipe)}\n`);
}
function runPrepareSteps(steps, sourceRoot) {
  if (!Array.isArray(steps)) {
    return;
  }
  for (const step of steps) {
    if (!step || typeof step.command !== 'string' || step.command.trim() === '') {
      throw new Error('wp_codebox_prepare_steps entries require a non-empty command');
    }
    const args = Array.isArray(step.args) && step.args.every((value) => typeof value === 'string') ? step.args : [];
    const cwd = path.resolve(sourceRoot, typeof step.cwd === 'string' ? step.cwd : '.');
    const relative = path.relative(sourceRoot, cwd);
    if (relative.startsWith('..') || path.isAbsolute(relative)) {
      throw new Error(`wp_codebox_prepare_steps cwd escapes the source root: ${step.cwd}`);
    }
    const result = spawnSync(step.command, args, { cwd, env: environmentWithoutDatabaseAdministration(), encoding: 'utf8', stdio: 'inherit' });
    if (result.error) {
      throw result.error;
    }
    if (result.status !== 0) {
      throw new Error(`wp_codebox_prepare_steps command failed (${step.command}, exit ${result.status ?? 1})`);
    }
  }
}
async function requireHarness(source) {
  try {
    await Promise.all([
      access(path.join(source, 'autoload.php')),
      access(path.join(source, 'wp-phpunit', 'wp-phpunit')),
    ]);
  } catch {
    throw new Error(`WP Codebox PHPUnit harness is required at ${source}. Run composer install in ${extensionRoot}.`);
  }
}
async function handoffArtifacts(artifactRoot, execution) {
  let pointer;
  try { pointer = json(await readFile(path.join(artifactRoot, 'latest-runtime.json'), 'utf8'), null); } catch {
    // A crashed workload may never write a runtime pointer. Preserve and parse
    // the captured aggregate at the stable artifact root rather than reporting
    // a zero-test run.
    return parsePublishedArtifacts(await publishPhpunitArtifacts(
      artifactRoot,
      await preservePhpunitOutput(artifactRoot, execution, []),
    ));
  }
  const runtime = pointer?.paths?.runtimeDirectory;
  if (typeof runtime !== 'string' || !/^runtime-[A-Za-z0-9][A-Za-z0-9-]*$/.test(runtime)) {
    return parsePublishedArtifacts(await publishPhpunitArtifacts(
      artifactRoot,
      await preservePhpunitOutput(artifactRoot, execution, []),
    ));
  }
  const artifactDirectory = path.join(artifactRoot, runtime);
  const managedRuntimeServices = Array.isArray(pointer.managedRuntimeServices) ? pointer.managedRuntimeServices : [];
  return parsePublishedArtifacts(await publishPhpunitArtifacts(
    artifactDirectory,
    await preservePhpunitOutput(artifactDirectory, execution, managedRuntimeServices),
  ));
}
async function publishPhpunitArtifacts(artifactDirectory, status) {
  const invocationArtifacts = process.env.HOMEBOY_INVOCATION_ARTIFACT_DIR;
  if (!invocationArtifacts) {
    return { directory: artifactDirectory, status, failuresPath: '' };
  }

  const controllerRunDirectory = process.env.HOMEBOY_RUN_DIR;
  const publishedDirectory = path.join(invocationArtifacts, 'wp-codebox-phpunit');
  const publishedFilesDirectory = path.join(publishedDirectory, 'files');
  const files = [
    { name: 'test-results.json', kind: 'test-results', role: 'structured-test-results', semantic_key: 'test_results', content_type: 'application/json', copy: true },
    { name: 'phpunit-output.log', kind: 'phpunit-output', role: 'raw-test-output', semantic_key: 'phpunit_output', content_type: 'text/plain', copy: true },
    { name: 'phpunit-execution-diagnosis.json', kind: 'phpunit-execution-diagnosis', role: 'test-execution-diagnosis', semantic_key: 'phpunit_execution_diagnosis', content_type: 'application/json', copy: true },
    { name: 'recipe-run-steps.json', kind: 'recipe-run-steps', role: 'recipe-run-step-ledger', semantic_key: 'recipe_run_steps', content_type: 'application/json', copy: true },
    { name: 'test-failures.json', kind: 'test-failures', role: 'structured-test-failures', semantic_key: 'test_failures', content_type: 'application/json' },
  ];
  for (const file of [
    { name: 'wp-codebox-timeout-diagnostics.json', kind: 'wp-codebox-timeout-diagnostics', role: 'timeout-diagnostics', semantic_key: 'timeout_diagnostics', content_type: 'application/json', copy: true },
    { name: 'recipe-run.json', kind: 'recipe-run-payload', role: 'raw-recipe-run-payload', semantic_key: 'recipe_run_payload', content_type: 'application/json', copy: true },
  ]) {
    try {
      await access(path.join(artifactDirectory, 'files', file.name));
      files.push(file);
    } catch {}
  }
  await withInvocationArtifactLock(invocationArtifacts, async () => {
    await assertDirectoryTree(invocationArtifacts, ['wp-codebox-phpunit', 'files']);
    await assertDirectoryTree(artifactDirectory, ['files']);
    for (const file of files.filter((entry) => entry.copy)) {
      await atomicCopy(path.join(artifactDirectory, 'files', file.name), path.join(publishedFilesDirectory, file.name));
    }
    // artifact://files locators resolve from HOMEBOY_RUN_DIR, not from the
    // invocation tree that Homeboy preserves after the extension exits.
    if (controllerRunDirectory) {
      await assertDirectoryTree(controllerRunDirectory, ['files']);
      for (const file of files.filter((entry) => entry.copy)) {
        await atomicCopy(path.join(publishedFilesDirectory, file.name), path.join(controllerRunDirectory, 'files', file.name));
      }
    }
    // The parser replaces this valid fallback atomically. Its registration and
    // aggregate counts survive a parser crash without poisoning Homeboy's sidecar.
    await atomicWrite(
      path.join(publishedFilesDirectory, 'test-failures.json'),
      await fallbackTestFailures(publishedFilesDirectory),
    );
    await registerInvocationArtifacts(invocationArtifacts, files.map(({ copy, ...file }) => ({
      ...file,
      path: path.join('wp-codebox-phpunit', 'files', file.name).replaceAll(path.sep, '/'),
    })));
  });
  return { directory: publishedDirectory, status, failuresPath: path.join(publishedFilesDirectory, 'test-failures.json') };
}
async function parsePublishedArtifacts(published) {
  if (process.env.HOMEBOY_TEST_RESULTS_FILE) {
    runScript('parse-test-results.sh', [published.directory]);
  }
  if (process.env.HOMEBOY_TEST_FAILURES_FILE || published.failuresPath) {
    const requestedFailuresPath = process.env.HOMEBOY_TEST_FAILURES_FILE;
    const temporaryFailuresPath = published.failuresPath ? `${published.failuresPath}.${process.pid}.parse` : requestedFailuresPath;
    if (published.failuresPath) {
      process.env.HOMEBOY_TEST_FAILURES_FILE = temporaryFailuresPath;
    }
    try {
      runScript('parse-test-failures.sh', [published.directory, componentPath]);
      if (published.failuresPath) {
        await withInvocationArtifactLock(process.env.HOMEBOY_INVOCATION_ARTIFACT_DIR, async () => {
          await assertDirectoryTree(process.env.HOMEBOY_INVOCATION_ARTIFACT_DIR, ['wp-codebox-phpunit', 'files']);
          await atomicCopy(temporaryFailuresPath, published.failuresPath);
        });
        if (requestedFailuresPath && requestedFailuresPath !== published.failuresPath) {
          await atomicCopy(published.failuresPath, requestedFailuresPath);
        }
      }
    } catch (error) {
      if (published.failuresPath) {
        const fallback = await fallbackTestFailures(path.dirname(published.failuresPath), error);
        await withInvocationArtifactLock(process.env.HOMEBOY_INVOCATION_ARTIFACT_DIR, async () => {
          await assertDirectoryTree(process.env.HOMEBOY_INVOCATION_ARTIFACT_DIR, ['wp-codebox-phpunit', 'files']);
          await atomicWrite(published.failuresPath, fallback);
        });
        if (requestedFailuresPath && requestedFailuresPath !== published.failuresPath) {
          await atomicCopy(published.failuresPath, requestedFailuresPath);
        }
      }
      throw error;
    } finally {
      if (published.failuresPath) { await unlink(temporaryFailuresPath).catch(() => {}); }
      if (requestedFailuresPath) {
        process.env.HOMEBOY_TEST_FAILURES_FILE = requestedFailuresPath;
      } else {
        delete process.env.HOMEBOY_TEST_FAILURES_FILE;
      }
    }
  }
  return published.status;
}
async function fallbackTestFailures(publishedFilesDirectory, error = null) {
  let summary = {};
  try {
    const results = JSON.parse(await readFile(path.join(publishedFilesDirectory, 'test-results.json'), 'utf8'));
    summary = isObject(results?.summary) ? results.summary : {};
  } catch {}

  const failures = [];
  if (error) {
    const testId = 'wp-codebox-phpunit-failure-parser';
    const errorType = 'WPCodeboxFailureParserError';
    const message = `Unable to parse preserved WP Codebox PHPUnit failure evidence: ${error.message}`;
    failures.push({
      test_name: testId,
      test_file: null,
      error_type: errorType,
      message,
      source_file: null,
      source_line: 0,
      test_id: testId,
      suite: 'wp-codebox-harness',
      file: null,
      line: 0,
      failure_type: errorType,
      fingerprint: createHash('sha256').update(`${testId}|${errorType}|${message}`).digest('hex'),
      stdout_excerpt: '',
      stderr_excerpt: '',
    });
  }

  return `${JSON.stringify({
    failures,
    total: Number(summary.total) || 0,
    passed: Number(summary.passed) || 0,
    metadata: {
      assertions: 0,
      failures: Number(summary.failed) || 0,
      errors: 0,
      skipped: Number(summary.skipped) || 0,
    },
  }, null, 2)}\n`;
}
async function registerInvocationArtifacts(invocationRoot, publishedArtifacts) {
  const manifestPath = path.join(invocationRoot, 'homeboy-artifact-manifest.json');
  await assertRegularOrMissing(manifestPath);
  let manifest = { schema: 'homeboy/artifact-manifest/v1', artifacts: [] };
  try {
    manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  } catch (error) {
    if (error.code !== 'ENOENT') { throw error; }
  }
  if (!isObject(manifest) || manifest.schema !== 'homeboy/artifact-manifest/v1' || (Object.hasOwn(manifest, 'artifacts') && !Array.isArray(manifest.artifacts))) {
    throw new Error('HOMEBOY_INVOCATION_ARTIFACT_DIR contains an invalid artifact manifest');
  }
  const existing = Array.isArray(manifest.artifacts) ? manifest.artifacts : [];
  manifest.artifacts = [
    ...existing.filter((entry) => !publishedArtifacts.some((artifact) => artifact.path === entry?.path)),
    ...publishedArtifacts.map(({ name, ...artifact }) => ({ id: name, label: name, ...artifact })),
  ];
  await atomicWrite(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
}
async function withInvocationArtifactLock(invocationRoot, action) {
  await assertDirectoryTree(invocationRoot, []);
  const lockPath = path.join(invocationRoot, '.wp-codebox-phpunit-publication.lock');
  let lease;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const startToken = await processStartToken(process.pid);
      if (!startToken) { throw new Error('Cannot establish WP Codebox publication lock process identity'); }
      await mkdir(lockPath, { mode: 0o700 });
      const metadata = { schema: 'homeboy/wp-codebox-publication-lease/v1', pid: process.pid, hostname: hostname(), start_token: startToken, acquired_at: new Date().toISOString(), token: `${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}` };
      await atomicWrite(path.join(lockPath, 'owner.json'), `${JSON.stringify(metadata)}\n`);
      const readyFile = process.env.HOMEBOY_WP_CODEBOX_PUBLICATION_LOCK_READY_FILE;
      if (readyFile) { await atomicWrite(readyFile, `${JSON.stringify(metadata)}\n`); }
      const holdMilliseconds = Number(process.env.HOMEBOY_WP_CODEBOX_PUBLICATION_LOCK_HOLD_MS || 0);
      if (Number.isFinite(holdMilliseconds) && holdMilliseconds > 0) { await new Promise((resolve) => setTimeout(resolve, holdMilliseconds)); }
      lease = { path: lockPath, token: metadata.token };
      break;
    } catch (error) {
      if (error.code !== 'EEXIST') { throw error; }
      const reclaimed = await reclaimDeadInvocationArtifactLock(lockPath);
      if (reclaimed) { continue; }
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
  }
  if (!lease) { throw new Error('Timed out waiting to publish WP Codebox test artifacts'); }
  try {
    return await action();
  } finally {
    const metadata = await readInvocationArtifactLease(path.join(lease.path, 'owner.json'));
    if (metadata?.token === lease.token) {
      await rm(lease.path, { recursive: true, force: true });
    }
  }
}
async function reclaimDeadInvocationArtifactLock(lockPath) {
  let stat;
  try { stat = await lstat(lockPath); } catch (error) {
    if (error.code === 'ENOENT') { return true; }
    throw error;
  }
  if (!stat.isDirectory() || stat.isSymbolicLink()) {
    throw new Error('WP Codebox publication lock has unknown ownership');
  }
  const metadata = await readInvocationArtifactLease(path.join(lockPath, 'owner.json'));
  // A contender can observe the directory between atomic mkdir acquisition and
  // atomic owner metadata publication. Wait out that short setup window; a
  // crashed owner without a lease remains fail-closed when the bounded wait ends.
  if (!metadata) { return false; }
  const liveness = await invocationArtifactLeaseLiveness(metadata);
  if (liveness === 'live') { return false; }
  if (liveness !== 'dead') { throw new Error('WP Codebox publication lock has unknown ownership'); }

  const quarantine = `${lockPath}.reclaimed-${process.pid}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  try {
    await rename(lockPath, quarantine);
  } catch (error) {
    if (error.code === 'ENOENT' || error.code === 'EEXIST') { return true; }
    throw error;
  }
  const quarantinedMetadata = await readInvocationArtifactLease(path.join(quarantine, 'owner.json'));
  if (await invocationArtifactLeaseLiveness(quarantinedMetadata) !== 'dead') {
    throw new Error('WP Codebox publication lock ownership changed during reclaim');
  }
  await rm(quarantine, { recursive: true, force: true });
  return true;
}
async function readInvocationArtifactLease(leasePath) {
  try {
    await assertRegularOrMissing(leasePath);
    const metadata = JSON.parse(await readFile(leasePath, 'utf8'));
    return isObject(metadata) && metadata.schema === 'homeboy/wp-codebox-publication-lease/v1' && Number.isInteger(metadata.pid) && metadata.pid > 0 && typeof metadata.hostname === 'string' && metadata.hostname !== '' && typeof metadata.start_token === 'string' && metadata.start_token !== '' && typeof metadata.token === 'string' && metadata.token !== '' ? metadata : null;
  } catch {
    return null;
  }
}
async function invocationArtifactLeaseLiveness(metadata) {
  if (!metadata || metadata.hostname !== hostname()) { return 'unknown'; }
  try {
    process.kill(metadata.pid, 0);
  } catch (error) {
    if (error.code === 'ESRCH') { return 'dead'; }
    return 'unknown';
  }
  const startToken = await processStartToken(metadata.pid);
  if (!startToken) { return 'unknown'; }
  return startToken === metadata.start_token ? 'live' : 'dead';
}
async function processStartToken(pid) {
  if (process.platform === 'linux') {
    try {
      const stat = await readFile(`/proc/${pid}/stat`, 'utf8');
      const close = stat.lastIndexOf(')');
      const fields = close === -1 ? [] : stat.slice(close + 1).trim().split(/\s+/);
      const startTime = fields[19];
      if (/^\d+$/.test(startTime || '')) { return `linux:${startTime}`; }
    } catch {}
  }
  if (['darwin', 'linux', 'freebsd'].includes(process.platform)) {
    const result = spawnSync('ps', ['-o', 'lstart=', '-p', String(pid)], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'], timeout: 1000 });
    const startTime = result.status === 0 ? result.stdout.trim().replace(/\s+/g, ' ') : '';
    if (startTime) { return `ps:${startTime}`; }
  }
  return '';
}
async function assertDirectoryTree(directoryRoot, segments) {
  const rootStat = await lstat(directoryRoot);
  if (!rootStat.isDirectory() || rootStat.isSymbolicLink()) {
    throw new Error(`WP Codebox artifact directory must be a non-symlink directory: ${directoryRoot}`);
  }
  const canonicalRoot = await realpath(directoryRoot);
  let current = directoryRoot;
  for (const segment of segments) {
    current = path.join(current, segment);
    try {
      const stat = await lstat(current);
      if (!stat.isDirectory() || stat.isSymbolicLink()) { throw new Error(`WP Codebox artifact directory contains a symlink or non-directory: ${current}`); }
    } catch (error) {
      if (error.code !== 'ENOENT') { throw error; }
      await mkdir(current);
    }
    const canonicalCurrent = await realpath(current);
    if (canonicalCurrent !== canonicalRoot && !canonicalCurrent.startsWith(`${canonicalRoot}${path.sep}`)) {
      throw new Error(`WP Codebox artifact directory escapes its invocation root: ${current}`);
    }
  }
}
async function assertRegularOrMissing(target) {
  try {
    const stat = await lstat(target);
    if (!stat.isFile() || stat.isSymbolicLink()) { throw new Error(`WP Codebox artifact target must be a non-symlink file: ${target}`); }
  } catch (error) {
    if (error.code !== 'ENOENT') { throw error; }
  }
}
async function atomicCopy(source, target) {
  await assertRegularOrMissing(source);
  await assertRegularOrMissing(target);
  const temporary = `${target}.${process.pid}.${Date.now()}.tmp`;
  await copyFile(source, temporary);
  await rename(temporary, target);
}
async function atomicWrite(target, content) {
  await assertRegularOrMissing(target);
  const temporary = `${target}.${process.pid}.${Date.now()}.tmp`;
  await writeFile(temporary, content);
  await rename(temporary, target);
}
async function preservePhpunitOutput(artifactDirectory, execution, managedRuntimeServices) {
  const filesDirectory = path.join(artifactDirectory, 'files');
  const logsDirectory = path.join(artifactDirectory, 'logs');
  const testResultsPath = path.join(filesDirectory, 'test-results.json');
  const phpunitOutputPath = path.join(filesDirectory, 'phpunit-output.log');
  const recipeRunStepsPath = path.join(filesDirectory, 'recipe-run-steps.json');
  const managedRuntimeServicesPath = path.join(filesDirectory, 'managed-runtime-services.json');
  await mkdir(filesDirectory, { recursive: true });
  await mkdir(logsDirectory, { recursive: true });
  await copyFile(execution.stdoutPath, path.join(logsDirectory, 'recipe-run.stdout.log'));
  await copyFile(execution.stderrPath, path.join(logsDirectory, 'recipe-run.stderr.log'));

  const stdoutInput = execution.timedOut ? { text: execution.stdoutPreview || (await readBoundedText(execution.stdoutPath)).text, truncated: true } : { text: await readFile(execution.stdoutPath, 'utf8'), truncated: false };
  const stderrInput = execution.timedOut ? await readBoundedText(execution.stderrPath) : { text: await readFile(execution.stderrPath, 'utf8'), truncated: false };
  const stdout = stdoutInput.text;
  const stderr = stderrInput.text;
  let preservedOutput = '';
  try { preservedOutput = execution.timedOut ? (await readBoundedText(phpunitOutputPath)).text : await readFile(phpunitOutputPath, 'utf8'); } catch {}
  // The recipe-run JSON payload carries the step ledger the raw log concatenation
  // discards. Preserve it as first-class structured evidence so a run that never
  // reaches the PHPUnit step still names the stage that stopped it.
  const recipeRun = execution.timedOut
    ? { ...recipeRunProjection(stdout), payload: null, phpunitIndexes: [], parse_status: 'bounded_timeout_projection' }
    : parseRecipeRunPayload(stdout, stderr);
  const recipeRunSteps = buildRecipeRunStepsLedger(recipeRun);
  await writeFile(recipeRunStepsPath, `${JSON.stringify(recipeRunSteps, null, 2)}\n`);
  const timeoutEvidence = execution.timedOut ? wpCodeboxTimeoutDiagnostics({
    phase: 'wp-codebox-phpunit-recipe-run',
    elapsedSeconds: execution.elapsedSeconds,
    budgetSeconds: phpunitTimeoutSeconds,
    selected: [selectedTestFile, ...changedTestFiles].filter(Boolean),
    termination: execution.termination,
    artifacts: [
      'artifact://files/recipe-run.json',
      'artifact://files/recipe-run-steps.json',
      'artifact://files/phpunit-output.log',
    ],
    payload: recipeRun,
    stderr,
    runtimeCrash: execution.runtimeCrash,
    secretValues: configuredSecretValues(),
  }) : null;
  if (timeoutEvidence) {
    await atomicCopy(execution.stdoutPath, path.join(filesDirectory, 'recipe-run.json'));
    await writeFile(path.join(filesDirectory, 'wp-codebox-timeout-diagnostics.json'), `${JSON.stringify(timeoutEvidence, null, 2)}\n`);
  }
  if (managedRuntimeServices.length > 0) {
    await writeFile(managedRuntimeServicesPath, `${JSON.stringify(managedRuntimeServices, null, 2)}\n`);
  }

  // A failed bootstrap stage means PHPUnit never got to run. Trust that over an
  // optimistic structured sidecar: reporting a pass because the sidecar claims
  // one is exactly how a red run shows up green.
  const stageLog = await readPhpunitStageLog(artifactDirectory, execution.timedOut);
  const stageFailure = (stageLog || '')
    .split('\n')
    .map((line) => line.trim())
    .find((line) => /^(STAGE_FAIL|STAGE_FATAL|STAGE_DIE):/.test(line)) || '';

  let results;
  try { results = json(await readFile(testResultsPath, 'utf8'), null); } catch { results = null; }
  const validResults = validTestResults(results);
  const structuredExecution = validResults && results.summary.total > 0 && !stageFailure;
  const preservedAggregate = phpunitAggregate(preservedOutput);
  const preservedOutputReferenced = validResults && Array.isArray(results.rawLogReferences) && results.rawLogReferences.some((reference) => reference?.path === 'files/phpunit-output.log' && reference?.kind === 'phpunit-output');
  const preservedOutputMatches = structuredExecution && preservedOutputReferenced && phpunitSummaryMatches(preservedAggregate, results.summary);
  const referencedOutput = structuredExecution ? await readMatchingPhpunitOutput(artifactDirectory, results) : '';
  const output = execution.timedOut
    ? `${JSON.stringify(timeoutEvidence)}\n`
    : redactSecrets( structuredExecution
    ? ((preservedOutputMatches && preservedOutput) || referencedOutput || stageLog || structuredPhpunitOutputUnavailable(results, recipeRun))
    : extractPhpunitOutput(stdout, stderr, recipeRun) );
  await writeFile(phpunitOutputPath, output);
  const aggregate = phpunitAggregate(output);
  if (!validResults) {
    results = { schema: 'wp-codebox/test-results/v1', status: 'unknown', summary: aggregate || emptyTestSummary(), suites: [], rawLogReferences: [] };
  }
  if (results && typeof results === 'object') {
    if (stageFailure) {
      results.summary = emptyTestSummary();
    } else if (!structuredExecution && aggregate) {
      results.summary = { ...results.summary, ...aggregate };
    }
    results.status = stageFailure ? 'failed' : normalizedTestStatus(results, aggregate, execution.status, validResults);
    const references = Array.isArray(results.rawLogReferences) ? results.rawLogReferences : [];
    results.rawLogReferences = [
      ...references.filter((reference) => reference?.path !== 'files/phpunit-output.log'),
      { path: 'files/phpunit-output.log', kind: 'phpunit-output' },
      { path: 'logs/recipe-run.stdout.log', kind: 'recipe-run-stdout' },
      { path: 'logs/recipe-run.stderr.log', kind: 'recipe-run-stderr' },
    ];
    const evidenceReferences = [
      { kind: 'structured-test-results', uri: 'artifact://files/test-results.json' },
      { kind: 'raw-phpunit-output', uri: 'artifact://files/phpunit-output.log' },
      { kind: 'test-execution-diagnosis', uri: 'artifact://files/phpunit-execution-diagnosis.json' },
      { kind: 'recipe-run-steps', uri: 'artifact://files/recipe-run-steps.json' },
    ];
    if (timeoutEvidence) {
      evidenceReferences.push({ kind: 'wp-codebox-timeout-diagnostics', uri: 'artifact://files/wp-codebox-timeout-diagnostics.json' });
      evidenceReferences.push({ kind: 'recipe-run-payload', uri: 'artifact://files/recipe-run.json' });
    }
    if (managedRuntimeServices.length > 0) {
      evidenceReferences.push({ kind: 'managed-runtime-services', uri: 'artifact://files/managed-runtime-services.json' });
    }
    results.evidenceReferences = evidenceReferences;
    await writeFile(testResultsPath, `${JSON.stringify(results, null, 2)}\n`);
  }

  const diagnosis = await phpunitExecutionDiagnosis(artifactDirectory, results, execution, stageLog, recipeRunSteps);
  await writeFile(path.join(filesDirectory, 'phpunit-execution-diagnosis.json'), `${JSON.stringify(diagnosis, null, 2)}\n`);

  process.stdout.write('Structured PHPUnit evidence: artifact://files/test-results.json\n');
  process.stdout.write('Full PHPUnit output: artifact://files/phpunit-output.log\n');
  process.stdout.write('PHPUnit execution diagnosis: artifact://files/phpunit-execution-diagnosis.json\n');
  process.stdout.write('Recipe run step ledger: artifact://files/recipe-run-steps.json\n');
  if (timeoutEvidence) {
    process.stdout.write(`${JSON.stringify(timeoutEvidence)}\n`);
  }
  if (stageFailure) {
    process.stdout.write(`BOOTSTRAP FAILURE: ${stageFailure.replace(/^STAGE_(FAIL|FATAL|DIE):/, '')}\n`);
  }
  if (diagnosis.executed_tests === 0) {
    process.stdout.write(`PHPUNIT_ZERO_TESTS cause=${diagnosis.cause}\n`);
    process.stdout.write(`  ${diagnosis.detail}\n`);
    process.stdout.write(`  ${diagnosis.remediation}\n`);
  }
  if (managedRuntimeServices.length > 0) {
    process.stdout.write('Managed runtime service evidence: artifact://files/managed-runtime-services.json\n');
  }
  return results.status;
}
// A selected test set must either execute or say why it did not. The sandbox
// bootstrap logs stage markers to a diagnostic file; classify them so a zero
// count names the failing seam instead of reporting an empty pass. The
// recipe-run step ledger supplies the stage identity for runs that never
// reached the sandbox bootstrap at all (or that stopped inside a recipe step),
// and is evaluated before the bootstrap_evidence_unavailable fallback so the
// most specific stage wins.
async function phpunitExecutionDiagnosis(artifactDirectory, results, execution, stageLog, recipeRunSteps) {
  const summary = isObject(results?.summary) ? results.summary : {};
  const executed = Number.isInteger(summary.total) ? summary.total : 0;
  const markers = stageLog === null
    ? []
    : stageLog.split('\n').filter((line) => /^(STAGE_FAIL|STAGE_FATAL|STAGE_DIE|NO_TEST_FILES|DISCOVERY:|SCOPED_TEST_FILES|PLUGIN_DETECTED|THEME_DETECTED|PLUGIN_ACTIVATE|NOTICE:no plugin entry file)/.test(line.trim()));
  const has = (pattern) => markers.some((line) => pattern.test(line));
  const scoped = markers.map((line) => line.match(/^SCOPED_TEST_FILES requested=(\d+) matched=(\d+)/)).find(Boolean);
  const discovery = markers.map((line) => line.match(/^DISCOVERY:.*\bfound=(\d+)/)).find(Boolean);

  const activation = {
    target: { slug, source: root, source_subpath: subpath || null, activate: true, activated: has(new RegExp(`^PLUGIN_ACTIVATE(_OK)? ${escapeRegExp(slug)}/`)) },
    validation_dependencies: dependencies.map(({ slug: dependencySlug, source }) => ({
      slug: dependencySlug,
      source,
      activate: true,
      activated: has(new RegExp(`^PLUGIN_ACTIVATE(_OK)? ${escapeRegExp(dependencySlug)}/`)),
    })),
    order: activationPlan.map(({ role, slug: planSlug }) => ({ role, slug: planSlug })),
  };

  const classification = (() => {
    if (has(/^STAGE_FAIL:activation/)) {
      return {
        cause: 'activation_failed',
        detail: 'Plugin activation raised a throwable before PHPUnit discovery.',
        remediation: 'Read the STAGE_FAIL:activation trace in the stage log; a validation dependency may be missing from validation_dependencies.',
      };
    }
    if (has(/^(STAGE_FAIL|STAGE_FATAL|STAGE_DIE)/)) {
      const marker = markers.find((line) => /^(STAGE_FAIL|STAGE_FATAL|STAGE_DIE)/.test(line)) || '';
      const stage = marker.split(':')[1] || 'unknown';
      return {
        cause: 'bootstrap_failed',
        detail: `The sandbox bootstrap failed at stage '${stage}': ${marker}`,
        remediation: 'Read the failing stage trace in the stage log excerpt below and in artifact://files/phpunit-output.log.',
      };
    }
    if (executed > 0) {
      return { cause: 'tests_executed', detail: `PHPUnit executed ${executed} test(s).`, remediation: 'No execution diagnosis is required.' };
    }
    // Ranked under the stage markers, which name the failing seam directly, and
    // over every ledger-derived cause: with a trapped runtime the ledger is a
    // description of the wreckage, not of what stopped the run.
    if (execution?.runtimeCrash) {
      const crash = execution.runtimeCrash;
      return {
        cause: 'runtime_crashed',
        detail: `The WP Codebox runtime raised an unclaimed fatal error before PHPUnit executed (${crash.id}): ${crash.message}`,
        remediation: crash.wasm_frame
          ? 'A PHP-WASM trap leaves the interpreter unusable, so the recipe-run cannot recover. Read logs/recipe-run.stderr.log for the faulting stack and identify the PHP call that trapped; an unsupported extension call (for example async mysqli polling) is the usual cause.'
          : 'Read logs/recipe-run.stderr.log for the faulting stack; the runtime terminated before PHPUnit could be invoked.',
      };
    }
    if (recipeRunSteps?.parse_status === 'unparseable') {
      return {
        cause: 'recipe_run_payload_unparseable',
        detail: 'The recipe-run JSON payload could not be parsed, so no execution step ledger is available.',
        remediation: 'Read logs/recipe-run.stdout.log and logs/recipe-run.stderr.log for the raw recipe-run output; a crashed or truncated recipe-run may not have flushed its JSON summary.',
      };
    }
    if (recipeRunSteps?.parse_status === 'no_executions') {
      return {
        cause: 'recipe_run_no_executions',
        detail: 'The recipe-run payload parsed but declared no execution steps, so no sandbox step ran and PHPUnit never started.',
        remediation: 'Inspect artifact://files/recipe-run-steps.json and logs/recipe-run.stderr.log for why the recipe dispatched no steps.',
      };
    }
    const failedRecipeStep = (recipeRunSteps?.executions || []).find((step) => Number.isInteger(step.exit_code) && step.exit_code !== 0);
    if (failedRecipeStep) {
      return {
        cause: 'recipe_step_failed',
        detail: `recipe step '${recipeStepName(failedRecipeStep)}' exited with code ${failedRecipeStep.exit_code} before PHPUnit executed.`,
        remediation: 'Read the failing step trace in artifact://files/phpunit-output.log and its ledger entry in artifact://files/recipe-run-steps.json; fix the command, dependency, or configuration that step depends on.',
      };
    }
    // A missing PHPUnit step is inferred from the ledger, so it must lose to
    // any stage marker that names the seam directly. The bootstrap legitimately
    // skips the PHPUnit invocation when discovery or the changed-file scope
    // matched nothing, and `NO_TEST_FILES` / `SCOPED_TEST_FILES ... matched=0`
    // is a sharper answer than "no step invoked PHPUnit". With no stage log
    // there is no sharper answer, so the ledger wins there.
    const phpunitStepNotExecuted = {
      cause: 'phpunit_step_not_executed',
      detail: `The recipe-run ledger recorded ${recipeRunSteps?.executions?.length ?? 0} execution step(s) but none invoked PHPUnit, so test execution never started.`,
      remediation: 'Inspect artifact://files/recipe-run-steps.json for the step ledger and artifact://files/phpunit-output.log for the setup output that was retained.',
    };
    if (stageLog === null) {
      if (recipeRunSteps?.phpunit_executed === false) {
        return phpunitStepNotExecuted;
      }
      return {
        cause: 'bootstrap_evidence_unavailable',
        detail: 'The sandbox produced no PHPUnit stage log, so the run did not reach the bootstrap that records stage markers.',
        remediation: 'Inspect artifact://files/phpunit-output.log and logs/recipe-run.stderr.log for a runtime or recipe-level failure.',
      };
    }
    if (has(/^NOTICE:no plugin entry file/) || !has(/^(PLUGIN_DETECTED|THEME_DETECTED)/)) {
      return {
        cause: 'target_component_not_mounted',
        detail: `The sandbox never loaded an entry file for the component under review (${slug}); its mount or plugin header is missing.`,
        remediation: 'Confirm wp_codebox_source_root/wp_codebox_source_subpath resolve to a directory containing the plugin entry file.',
      };
    }
    if (scoped && Number(scoped[1]) > 0 && Number(scoped[2]) === 0) {
      const untranslated = changedTestFileScope.untranslated;
      if (untranslated.length > 0) {
        return {
          cause: 'changed_file_sandbox_path_untranslated',
          detail: `${untranslated.length} selected path(s) could not be mapped into the sandbox, so the filter could not match them: ${untranslated.slice(0, 10).join(', ')}`,
          remediation: 'Confirm the selected paths sit under the mounted component (wp_codebox_source_root/wp_codebox_source_subpath) or under a configured wp_codebox_phpunit_mounts entry.',
        };
      }
      return {
        cause: 'changed_file_filter_mismatch',
        detail: `The changed-file scope requested ${scoped[1]} file(s) and matched 0 discovered test files.`,
        remediation: 'Confirm the selected paths sit under the configured PHPUnit test root and match the suite\'s discovery suffixes/prefixes.',
      };
    }
    if (has(/^NO_TEST_FILES/) || (discovery && Number(discovery[1]) === 0)) {
      return {
        cause: 'phpunit_discovery_empty',
        detail: 'PHPUnit discovery found no test files under the configured test root.',
        remediation: 'Check wp_codebox_phpunit_test_root and the phpunit.xml testsuite directories against the mounted component.',
      };
    }
    if (recipeRunSteps?.phpunit_executed === false) {
      return phpunitStepNotExecuted;
    }
    return {
      cause: 'suite_reported_no_tests',
      detail: 'The sandbox mounted, activated, and discovered the component but the assembled suite contained no test cases.',
      remediation: 'Confirm the discovered files declare concrete PHPUnit\\Framework\\TestCase subclasses with runnable test methods.',
    };
  })();

  return {
    schema: 'homeboy/wordpress-phpunit-execution-diagnosis/v1',
    executed_tests: executed,
    recipe_run_exit_code: execution.status,
    scope: {
      selected_test_file: selectedTestFile || null,
      changed_test_files: changedTestFiles,
      changed_test_files_sandbox: changedTestFileScope.sandbox,
      changed_test_files_untranslated: changedTestFileScope.untranslated,
      test_root: phpunitProfile.testRoot,
      phpunit_config: phpunitProfile.config || null,
    },
    activation,
    ...classification,
    evidence: [
      { kind: 'raw-phpunit-output', uri: 'artifact://files/phpunit-output.log' },
      { kind: 'structured-test-results', uri: 'artifact://files/test-results.json' },
      { kind: 'recipe-options', uri: 'artifact://wp-codebox-phpunit-recipe-options.json' },
      { kind: 'recipe-provenance', uri: 'artifact://wp-codebox-phpunit-provenance.json' },
      { kind: 'recipe-run-steps', uri: 'artifact://files/recipe-run-steps.json' },
    ],
    stage_markers: markers.slice(0, 60),
  };
}
async function readPhpunitStageLog(artifactDirectory, bounded = false) {
  const stageRoot = path.join(artifactDirectory, 'files', 'phpunit');
  const candidates = [path.join(stageRoot, '.pg-test-result.txt')];
  try {
    for (const entry of await readdir(stageRoot, { withFileTypes: true })) {
      if (entry.isDirectory()) {
        candidates.push(path.join(stageRoot, entry.name, '.pg-test-result.txt'));
      }
    }
  } catch {}
  for (const candidate of candidates) {
    try { return bounded ? (await readBoundedText(candidate)).text : await readFile(candidate, 'utf8'); } catch {}
  }
  return null;
}
async function readMatchingPhpunitOutput(artifactDirectory, results) {
  const references = [
    ...(Array.isArray(results.rawLogReferences) ? results.rawLogReferences : []),
    ...(Array.isArray(results.suites) ? results.suites.flatMap((suite) => Array.isArray(suite?.rawLogReferences) ? suite.rawLogReferences : []) : []),
  ];
  const canonicalRoot = await realpath(artifactDirectory);
  const seen = new Set();
  for (const reference of references) {
    if (!['phpunit-output', 'commands-log'].includes(reference?.kind) || typeof reference.path !== 'string' || seen.has(reference.path)) {
      continue;
    }
    seen.add(reference.path);
    if (path.isAbsolute(reference.path) || reference.path.includes('\\') || reference.path.split('/').includes('..')) {
      continue;
    }
    const candidate = path.resolve(canonicalRoot, reference.path);
    if (!candidate.startsWith(`${canonicalRoot}${path.sep}`)) {
      continue;
    }
    try {
      const stat = await lstat(candidate);
      const canonical = await realpath(candidate);
      if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 8 * 1024 * 1024 || !canonical.startsWith(`${canonicalRoot}${path.sep}`)) {
        continue;
      }
      const output = await readFile(canonical, 'utf8');
      const aggregate = reference.kind === 'commands-log' ? phpunitCommandLogAggregate(output) : phpunitAggregate(output);
      if (phpunitSummaryMatches(aggregate, results.summary)) {
        return output;
      }
    } catch {}
  }
  return '';
}
function phpunitCommandLogAggregate(output) {
  const sections = [...output.matchAll(/(?:^|\n---\n)(\[[^\]\n]+\]\s+wordpress\.phpunit\b[\s\S]*?)(?=\n---\n\[[^\]\n]+\]\s+\S|$)/g)]
    .map((match) => phpunitAggregate(match[1]))
    .filter(Boolean);
  if (sections.length === 0) {
    return phpunitAggregate(output);
  }
  return sections.reduce((aggregate, section) => ({
    total: aggregate.total + section.total,
    passed: aggregate.passed + section.passed,
    failed: aggregate.failed + section.failed,
    skipped: aggregate.skipped + section.skipped,
    assertions: aggregate.assertions + section.assertions,
  }), { total: 0, passed: 0, failed: 0, skipped: 0, assertions: 0 });
}
function phpunitSummaryMatches(aggregate, summary) {
  return aggregate !== null && ['total', 'passed', 'failed', 'skipped'].every((key) => aggregate[key] === summary[key]);
}
function escapeRegExp(value) {
  return String(value).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}
function validTestResults(results) {
  if (!isObject(results) || results.schema !== 'wp-codebox/test-results/v1' || !['passed', 'failed', 'skipped', 'unknown'].includes(results.status) || !isObject(results.summary)) {
    return false;
  }
  return ['total', 'passed', 'failed', 'skipped'].every((key) => Number.isInteger(results.summary[key]) && results.summary[key] >= 0);
}
function emptyTestSummary() {
  return { total: 0, passed: 0, failed: 0, skipped: 0, unknown: 0 };
}
function normalizedTestStatus(results, aggregate, executionStatus, validResults) {
  if (executionStatus !== 0 || (validResults && results.status === 'failed') || aggregate?.failed > 0) {
    return 'failed';
  }
  if (!validResults) {
    return 'unknown';
  }
  const total = aggregate?.total ?? results.summary.total;
  if (total === 0) {
    return ['skip', 'skipped'].includes(settings.phpunit_no_tests || 'skipped') ? 'skipped' : 'failed';
  }
  if (results.status === 'unknown' && aggregate) {
    return 'passed';
  }
  return results.status;
}
function parseRecipeRunPayload(stdout, stderr = '') {
  const payload = json(stdout.trim(), null) || extractRecipeRunJson(stdout);
  if (payload === null) {
    return {
      payload: null,
      executions: [],
      phpunitIndexes: [],
      parse_status: 'unparseable',
      parse_diagnostics: recipeRunParseDiagnostics(stdout, stderr),
    };
  }
  const executions = isObject(payload) && Array.isArray(payload.executions) ? payload.executions : [];
  const phpunitIndexes = [];
  executions.forEach((execution, index) => { if (isPhpunitExecution(execution)) { phpunitIndexes.push(index); } });
  return {
    payload,
    executions,
    phpunitIndexes,
    parse_status: executions.length > 0 ? 'executions' : 'no_executions',
  };
}
function extractRecipeRunJson(stdout) {
  const candidates = [];
  for (let start = 0; start < stdout.length; start += 1) {
    if (stdout[start] !== '{') {
      continue;
    }
    let depth = 0;
    let quoted = false;
    let escaped = false;
    for (let end = start; end < stdout.length; end += 1) {
      const character = stdout[end];
      if (quoted) {
        if (escaped) {
          escaped = false;
        } else if (character === '\\') {
          escaped = true;
        } else if (character === '"') {
          quoted = false;
        }
        continue;
      }
      if (character === '"') {
        quoted = true;
      } else if (character === '{') {
        depth += 1;
      } else if (character === '}') {
        depth -= 1;
        if (depth === 0) {
          const candidate = json(stdout.slice(start, end + 1), null);
          if (isObject(candidate)) {
            candidates.push(candidate);
          }
          break;
        }
      }
    }
  }
  return candidates.findLast((candidate) => Array.isArray(candidate.executions)) || candidates.at(-1) || null;
}
function recipeRunParseDiagnostics(stdout, stderr) {
  const diagnostic = (value) => {
    const redacted = redactSecrets(typeof value === 'string' ? value : '');
    const bytes = Buffer.byteLength(redacted);
    const excerpt = Buffer.from(redacted).subarray(0, 4096).toString('utf8');
    return { bytes, truncated: bytes > 4096, excerpt };
  };
  return {
    reason: 'recipe-run stdout did not contain a complete JSON object payload',
    stdout: diagnostic(stdout),
    stderr: diagnostic(stderr),
  };
}
// A PHPUnit invocation is identified by its recipe command name when the
// payload carries one, and otherwise by the PHPUnit output signatures it
// produced. Composer install and plugin activation payloads are setup JSON and
// never match these signatures, so a ledger that records only those steps is
// correctly reported as not having executed PHPUnit.
function isPhpunitExecution(execution) {
  if (!execution || typeof execution !== 'object') {
    return false;
  }
  if (typeof execution.command === 'string' && /phpunit/i.test(execution.command)) {
    return true;
  }
  const output = `${typeof execution.stdout === 'string' ? execution.stdout : ''}\n${typeof execution.stderr === 'string' ? execution.stderr : ''}`;
  return /PHPUnit\s+\d+\.\d+|\bOK\s*\(\s*\d+\s+tests?|\bTests:\s*\d+,\s*Assertions:/i.test(output);
}
function integerExitCode(execution) {
  const value = execution?.exitCode ?? execution?.exit_code;
  return Number.isInteger(value) ? value : undefined;
}
function buildRecipeRunStepsLedger(recipeRun) {
  const executions = recipeRun.executions.map((execution, index) => {
    const stdout = typeof execution?.stdout === 'string' ? execution.stdout : '';
    const stderr = typeof execution?.stderr === 'string' ? execution.stderr : '';
    return clean({
      index,
      command: typeof execution?.command === 'string' && execution.command !== '' ? execution.command : undefined,
      status: typeof execution?.status === 'string' && execution.status !== '' ? execution.status : undefined,
      exit_code: integerExitCode(execution),
      stdout_bytes: Buffer.byteLength(stdout),
      stderr_bytes: Buffer.byteLength(stderr),
      phpunit: recipeRun.phpunitIndexes.includes(index),
    });
  });
  return {
    schema: 'homeboy/wordpress-recipe-run-steps/v1',
    parse_status: recipeRun.parse_status,
    phpunit_executed: recipeRun.phpunitIndexes.length > 0,
    phpunit_step_indexes: recipeRun.phpunitIndexes,
    executions,
    ...(recipeRun.parse_diagnostics ? { parse_diagnostics: recipeRun.parse_diagnostics } : {}),
  };
}
function recipeStepName(step) {
  return step?.command || `step ${step?.index ?? '?'}`;
}
function noPhpunitExecutionBanner(recipeRun) {
  const steps = recipeRun.executions.map((execution, index) => {
    const command = recipeStepName({ ...execution, index });
    const exitCode = integerExitCode(execution);
    const stdoutBytes = typeof execution?.stdout === 'string' ? Buffer.byteLength(execution.stdout) : 0;
    const stderrBytes = typeof execution?.stderr === 'string' ? Buffer.byteLength(execution.stderr) : 0;
    return `  [${index}] ${command} (exit_code=${exitCode === undefined ? 'unset' : exitCode}, stdout=${stdoutBytes}B, stderr=${stderrBytes}B)`;
  });
  return [
    '============================================================',
    'WP_CODEBOX_RECIPE_RUN: NO_PHPUNIT_EXECUTION',
    `The recipe-run payload reported ${recipeRun.executions.length} execution step(s), but none invoked PHPUnit.`,
    'Test execution never started; the setup output below is retained for the record.',
    'Structured step ledger: artifact://files/recipe-run-steps.json',
    ...steps,
    '============================================================',
    '',
  ].join('\n');
}
function structuredPhpunitOutputUnavailable(results, recipeRun) {
  const summary = results.summary;
  const setupOutput = recipeRun.executions.flatMap((execution) => [execution?.stdout, execution?.stderr]).filter((value) => typeof value === 'string' && value !== '').join('');
  if (setupOutput) {
    return setupOutput;
  }
  return [
    '============================================================',
    'WP_CODEBOX_PHPUNIT_OUTPUT: UNAVAILABLE',
    `Structured results report ${summary.total} test(s), ${summary.failed} failed, but no raw PHPUnit output artifact was retained.`,
    '============================================================',
    '',
  ].join('\n');
}
function extractPhpunitOutput(stdout, stderr, recipeRun) {
  const parsed = recipeRun || parseRecipeRunPayload(stdout);
  if (parsed.payload === null) {
    return stdout + stderr;
  }
  const chunks = [];
  for (const execution of parsed.executions) {
    if (typeof execution?.stdout === 'string') { chunks.push(execution.stdout); }
    if (typeof execution?.stderr === 'string') { chunks.push(execution.stderr); }
  }
  // A setup-only run must not read like a successful-but-empty PHPUnit run.
  // Make the missing PHPUnit step explicit with a machine-greppable banner.
  if (parsed.executions.length > 0 && parsed.phpunitIndexes.length === 0) {
    return `${noPhpunitExecutionBanner(parsed)}${chunks.join('')}`;
  }
  if (chunks.length > 0) {
    return chunks.join('');
  }
  return stdout + stderr;
}
function phpunitAggregate(output) {
  const ok = output.match(/\bOK \((\d+) tests?,\s*(\d+) assertions?\)/i);
  if (ok) {
    return { total: Number(ok[1]), passed: Number(ok[1]), failed: 0, skipped: 0, assertions: Number(ok[2]) };
  }
  const summary = [...output.matchAll(/\bTests:\s*(\d+),\s*Assertions:\s*(\d+)([^\n]*)/gi)].pop();
  if (!summary) { return null; }
  const total = Number(summary[1]);
  const assertions = Number(summary[2]);
  const tail = summary[3];
  const count = (name) => Number((tail.match(new RegExp(`\\b${name}:\\s*(\\d+)`, 'i')) || [])[1] || 0);
  const failed = count('Errors') + count('Failures');
  const skipped = count('Skipped') + count('Incomplete');
  return { total, passed: Math.max(0, total - failed - skipped), failed, skipped, assertions };
}
async function resolvePhpunitProfile(configuration, pluginDirectory, pluginSlug) {
  const sandboxRoot = sandboxPluginDirectory(pluginSlug);
  const configured = typeof configuration.wp_codebox_phpunit_config === 'string' ? configuration.wp_codebox_phpunit_config : '';
  let config = configured;
  let hostConfig = '';
  if (configured) {
    hostConfig = path.isAbsolute(configured) ? configured : path.join(pluginDirectory, configured);
  } else {
    for (const candidate of ['phpunit.xml', 'phpunit.xml.dist']) {
      try { await access(path.join(pluginDirectory, candidate)); config = `${sandboxRoot}/${candidate}`; hostConfig = path.join(pluginDirectory, candidate); break; } catch {}
    }
  }
  const testRoot = configuration.wp_codebox_phpunit_test_root || `${sandboxRoot}/tests`;
  const cwd = configuration.wp_codebox_phpunit_cwd || sandboxRoot;
  let environment = 'wordpress-integration';
  let bootstrap = '';
  if (hostConfig) {
    try {
      const xml = await readFile(hostConfig, 'utf8');
      bootstrap = xml.match(/\bbootstrap\s*=\s*["']([^"']+)["']/i)?.[1] || '';
      if (bootstrap) {
        const bootstrapPath = path.resolve(path.dirname(hostConfig), bootstrap);
        const source = await readFile(bootstrapPath, 'utf8').catch(() => '');
        environment = /WP_UnitTestCase|wp-load\.php|WP_TESTS_DIR|wp-tests-config/i.test(source) ? 'wordpress-integration' : 'standalone-php';
      }
    } catch {}
  }
  return { config, testRoot, cwd, environment, hostConfig, bootstrap };
}
function resolvePhpunitBootstrap(configuration, profile) {
  const requestedMode = configuration.wp_codebox_phpunit_bootstrap_mode || 'auto';
  if (!['auto', 'managed', 'project'].includes(requestedMode)) {
    throw new Error(`Unsupported WP Codebox PHPUnit bootstrap mode: ${requestedMode}`);
  }
  let mode = requestedMode;
  if (requestedMode === 'auto') {
    mode = profile.bootstrap ? 'project' : 'managed';
  }
  const override = typeof configuration.wp_codebox_phpunit_project_bootstrap === 'string'
    ? configuration.wp_codebox_phpunit_project_bootstrap.trim()
    : '';
  return {
    mode,
    projectBootstrap: mode === 'project' ? override : '',
  };
}
async function persistRecipeEvidence(artifactDirectory, recipeOptions, generatedRecipePath, profile, resolvedDependencies) {
  const sourceRefs = [{ slug, source: root, source_subpath: subpath || null }, ...resolvedDependencies.map((dependency) => ({ slug: dependency.slug, source: dependency.source }))];
  const wpCodeboxArgv = wpCodeboxCommand();
  const wpCodeboxCli = wpCodeboxArgv[wpCodeboxArgv.length - 1];
  await Promise.all([
    copyFile(generatedRecipePath, path.join(artifactDirectory, 'wp-codebox-phpunit-recipe.json')),
    writeFile(path.join(artifactDirectory, 'wp-codebox-phpunit-recipe-options.json'), `${JSON.stringify(recipeOptions, null, 2)}\n`),
    writeFile(path.join(artifactDirectory, 'wp-codebox-phpunit-profile.json'), `${JSON.stringify({ wordpress: { topology }, phpunit: { config: profile.config, cwd: profile.cwd, test_root: profile.testRoot, environment: profile.environment, bootstrap_mode: recipeOptions.bootstrapMode, project_bootstrap: recipeOptions.projectBootstrap || null, passthrough_args: recipeOptions.phpunitArgs, extra_mounts: recipeOptions.mounts } }, null, 2)}\n`),
    writeFile(path.join(artifactDirectory, 'wp-codebox-phpunit-provenance.json'), `${JSON.stringify({ source_refs: sourceRefs, activation: { order: activationPlan.map(({ role, slug: planSlug }) => ({ role, slug: planSlug, activate: true })) }, scope: { selected_test_file: selectedTestFile || null, changed_test_files: changedTestFiles, changed_test_files_sandbox: changedTestFileScope.sandbox }, wp_codebox: { cli_bin: wpCodeboxCli, resolved_cli_path: wpCodeboxCli, command: wpCodeboxArgv } }, null, 2)}\n`),
  ]);
}
function runScript(script, args) {
  const result = spawnSync('bash', [path.join(scriptDirectory, script), ...args], { cwd: componentPath, env: environmentWithoutDatabaseAdministration(), encoding: 'utf8', stdio: 'inherit' });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`${script} failed with exit code ${result.status ?? 1}`);
  }
}
