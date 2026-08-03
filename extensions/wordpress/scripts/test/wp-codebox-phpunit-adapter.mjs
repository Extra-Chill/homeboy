#!/usr/bin/env node
/**
 * External dependencies
 */
import { createWriteStream } from 'node:fs';
import { access, copyFile, lstat, mkdir, mkdtemp, readdir, readFile, realpath, rename, rm, unlink, writeFile } from 'node:fs/promises';
import { hostname, tmpdir } from 'node:os';
import path from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const settings = parseSettings(process.env.HOMEBOY_SETTINGS_JSON);
const componentPath = required(process.env.HOMEBOY_COMPONENT_PATH, 'HOMEBOY_COMPONENT_PATH');
const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const extensionRoot = path.resolve(scriptDirectory, '../..');
const harnessSource = path.join(extensionRoot, 'vendor');
const NATIVE_MARIADB_CAPABILITY = 'runtime-service:mysql:native:mariadb';
const RUNTIME_SERVICE_CAPABILITIES_SCHEMA = 'wp-codebox/runtime-service-capabilities/v1';
const slug = process.env.COMPONENT_ID || path.basename(componentPath);
const root = settings.wp_codebox_source_root || componentPath;
const subpath = settings.wp_codebox_source_subpath || undefined;
const pluginSourceDirectory = subpath ? path.join(root, subpath) : root;
const phpunitProfile = await resolvePhpunitProfile(settings, pluginSourceDirectory, slug);
const phpunitBootstrap = resolvePhpunitBootstrap(settings, phpunitProfile);
const topology = await resolveWordPressTopology(settings, pluginSourceDirectory);
const databaseService = resolveDatabaseService(settings, process.env);
requireDatabaseServiceCapability(databaseService);
runPrepareSteps(settings.wp_codebox_prepare_steps, pluginSourceDirectory);
const directory = await mkdtemp(path.join(tmpdir(), 'homeboy-wp-codebox-phpunit-'));
const optionsPath = path.join(directory, 'options.json');
const recipePath = path.join(directory, 'recipe.json');
const artifacts = process.env.HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR || path.join(directory, 'artifacts');
const runArtifacts = path.join(artifacts, `wp-codebox-phpunit.${process.pid}`);
const dependencies = await Promise.all((await dependencyPaths(settings, [componentPath, pluginSourceDirectory])).map(async (source) => {
  const dependencySlug = path.basename(source).replace(/@[^/]+$/, '');
  return { source, slug: dependencySlug, sandboxDirectory: sandboxPluginDirectory(dependencySlug), composer: await composerPreparation(source) };
}));
await requireHarness(harnessSource);
const options = clean({
  wordpressVersion: settings.wordpress_runtime_version,
  phpVersion: settings.wordpress_runtime_php_version,
  databaseType: settings.database_type,
  services: databaseService ? [databaseService.service] : undefined,
  pluginSlug: slug,
  extra_plugins: [
    { source: root, sourceSubpath: subpath, slug, activate: false },
    ...dependencies.map(({ source, slug: dependencySlug, composer }) => clean({ source, slug: dependencySlug, activate: true, composer })),
  ],
  dependencyMounts: [...new Set([sandboxPluginDirectory(slug), ...dependencies.map(({ sandboxDirectory }) => sandboxDirectory)])],
  testRoot: phpunitProfile.testRoot,
  phpunitXml: phpunitProfile.config,
  cwd: phpunitProfile.cwd,
  phpunitArgs: process.argv.slice(2),
  env: settings.bench_env,
  wpConfigDefines: settings.wp_config_defines,
  autoloadFile: '/wp-codebox-vendor/autoload.php',
  bootstrapMode: phpunitBootstrap.mode,
  projectBootstrap: phpunitBootstrap.projectBootstrap,
  multisite: topology.multisite,
  preloadFiles: settings.wp_codebox_phpunit_preload_files,
  mounts: [...canonicalMounts(settings.wp_codebox_phpunit_mounts), { source: harnessSource, target: '/wp-codebox-vendor', mode: 'readonly' }],
});

try {
  await writeFile(optionsPath, `${JSON.stringify(options)}\n`);
  run(['recipe', 'build', 'phpunit', '--options', optionsPath, '--output', recipePath]);
  await applyDatabaseServiceAuthorization(recipePath);
  await mkdir(runArtifacts, { recursive: true });
  await persistRecipeEvidence(runArtifacts, options, recipePath, phpunitProfile, dependencies);
  const executionArgs = ['recipe-run', '--recipe', recipePath, '--artifacts', runArtifacts, '--json'];
  if (databaseService?.boundary) {
    executionArgs.push('--approve-external-service-writes', '--policy', JSON.stringify(databaseService.policy));
  }
  const execution = await runCaptured(executionArgs);
  const artifactStatus = await handoffArtifacts(runArtifacts, execution);
  if (execution.status !== 0 || !['passed', 'skipped'].includes(artifactStatus)) {
    process.exitCode = execution.status || 1;
  } else {
    process.stdout.write('WP Codebox test run complete.\n');
  }
} finally {
  await rm(directory, { recursive: true, force: true });
}
async function runCaptured(args) {
  const logsDirectory = path.join(runArtifacts, 'logs');
  const stdoutPath = path.join(logsDirectory, 'recipe-run.stdout.log');
  const stderrPath = path.join(logsDirectory, 'recipe-run.stderr.log');
  await mkdir(logsDirectory, { recursive: true });
  const secretValues = configuredSecretValues();

  return new Promise((resolve, reject) => {
    const child = spawn(process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox', args, { cwd: componentPath, stdio: ['ignore', 'pipe', 'pipe'] });
    const stdoutFile = createWriteStream(stdoutPath);
    const stderrFile = createWriteStream(stderrPath);
    const stdoutRedactor = createLineRedactor(secretValues);
    const stderrRedactor = createLineRedactor(secretValues);
    let childError;

    child.stdout.on('data', (chunk) => {
      chunk = stdoutRedactor.write(chunk);
      stdoutFile.write(chunk);
    });
    child.stderr.on('data', (chunk) => {
      chunk = stderrRedactor.write(chunk);
      stderrFile.write(chunk);
    });
    child.on('error', (error) => { childError = error; });
    child.on('close', (status) => {
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
        resolve({ status: status ?? 1, stdoutPath, stderrPath });
      }, reject);
    });
  });
}
function configuredSecretValues() {
  const configured = settings.bench_env && typeof settings.bench_env === 'object' ? settings.bench_env : {};
  const databaseSecretNames = databaseService?.secretEnv || [];
  return Object.entries({ ...process.env, ...configured })
    .filter(([name, value]) => typeof value === 'string' && value.length > 0 && (databaseSecretNames.includes(name) || (/(?:auth|cookie|credential|key|nonce|passw|secret|session|token)/i.test(name) && value.length >= 8)))
    .map(([, value]) => value)
    .sort((left, right) => right.length - left.length);
}
function createLineRedactor(secretValues) {
  const decoder = new StringDecoder('utf8');
  let pending = '';
  const overlap = Math.max(0, ...secretValues.map((secret) => secret.length - 1));
  const redact = (text) => {
    for (const secret of secretValues) {
      text = text.replaceAll(secret, '[REDACTED]');
    }
    return text;
  };
  return {
    write(chunk) {
      pending += decoder.write(chunk);
      const boundary = pending.lastIndexOf('\n') + 1;
      // JSON recipe responses can be a single line larger than a pipe buffer.
      // Flush bounded chunks so a crashed PHPUnit run cannot deadlock at 64 KiB.
      if (boundary === 0) {
        if (pending.length < 8192) { return ''; }
        // Keep enough input to redact a secret split across the flush boundary.
        const buffered = pending.slice(0, Math.max(0, pending.length - overlap));
        pending = pending.slice(buffered.length);
        return redact(buffered);
      }
      const completeLines = pending.slice(0, boundary);
      pending = pending.slice(boundary);
      return redact(completeLines);
    },
    end() {
      pending += decoder.end();
      return redact(pending);
    },
  };
}

function run(args) {
  const result = spawnSync(process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox', args, { cwd: componentPath, env: environmentWithoutDatabaseAdministration(), encoding: 'utf8', stdio: 'inherit' });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
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
async function composerPreparation(source) {
  try {
    await access(path.join(source, 'composer.json'));
  } catch {
    return undefined;
  }
  try {
    await access(path.join(source, 'vendor/autoload.php'));
    return undefined;
  } catch {
    return 'install';
  }
}
function sandboxPluginDirectory(pluginSlug) {
  return `/wordpress/wp-content/plugins/${pluginSlug}`;
}
function canonicalMounts(value) {
  return Array.isArray(value) ? value : [];
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
      throw new Error(`wp_codebox_database_service secret environment variable is unavailable: ${name}`);
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
  const result = spawnSync(process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox', ['runtime', 'descriptor', '--json'], {
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

  const publishedDirectory = path.join(invocationArtifacts, 'wp-codebox-phpunit');
  const publishedFilesDirectory = path.join(publishedDirectory, 'files');
  const files = [
    { name: 'test-results.json', kind: 'test-results', role: 'structured-test-results', semantic_key: 'test_results', content_type: 'application/json' },
    { name: 'phpunit-output.log', kind: 'phpunit-output', role: 'raw-test-output', semantic_key: 'phpunit_output', content_type: 'text/plain' },
    { name: 'test-failures.json', kind: 'test-failures', role: 'structured-test-failures', semantic_key: 'test_failures', content_type: 'application/json' },
  ];
  await withInvocationArtifactLock(invocationArtifacts, async () => {
    await assertDirectoryTree(invocationArtifacts, ['wp-codebox-phpunit', 'files']);
    await assertDirectoryTree(artifactDirectory, ['files']);
    for (const file of files.slice(0, 2)) {
      await atomicCopy(path.join(artifactDirectory, 'files', file.name), path.join(publishedFilesDirectory, file.name));
    }
    // The parser replaces this valid fallback atomically. Its registration and
    // aggregate counts survive a parser crash without poisoning Homeboy's sidecar.
    await atomicWrite(
      path.join(publishedFilesDirectory, 'test-failures.json'),
      await fallbackTestFailures(publishedFilesDirectory),
    );
    await registerInvocationArtifacts(invocationArtifacts, files.map((file) => ({
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
  const managedRuntimeServicesPath = path.join(filesDirectory, 'managed-runtime-services.json');
  await mkdir(filesDirectory, { recursive: true });
  await mkdir(logsDirectory, { recursive: true });
  await copyFile(execution.stdoutPath, path.join(logsDirectory, 'recipe-run.stdout.log'));
  await copyFile(execution.stderrPath, path.join(logsDirectory, 'recipe-run.stderr.log'));

  const stdout = await readFile(execution.stdoutPath, 'utf8');
  const stderr = await readFile(execution.stderrPath, 'utf8');
  const output = extractPhpunitOutput(stdout, stderr);
  await writeFile(phpunitOutputPath, output);
  if (managedRuntimeServices.length > 0) {
    await writeFile(managedRuntimeServicesPath, `${JSON.stringify(managedRuntimeServices, null, 2)}\n`);
  }

  const aggregate = phpunitAggregate(output);
  let results;
  try { results = json(await readFile(testResultsPath, 'utf8'), null); } catch { results = null; }
  const validResults = validTestResults(results);
  if (!validResults) {
    results = { schema: 'wp-codebox/test-results/v1', status: 'unknown', summary: aggregate || emptyTestSummary(), suites: [], rawLogReferences: [] };
  }
  if (results && typeof results === 'object') {
    const summary = isObject(results.summary) ? results.summary : {};
    if (aggregate) {
      results.summary = { ...summary, ...aggregate };
    }
    results.status = normalizedTestStatus(results, aggregate, execution.status, validResults);
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
    ];
    if (managedRuntimeServices.length > 0) {
      evidenceReferences.push({ kind: 'managed-runtime-services', uri: 'artifact://files/managed-runtime-services.json' });
    }
    results.evidenceReferences = evidenceReferences;
    await writeFile(testResultsPath, `${JSON.stringify(results, null, 2)}\n`);
  }

  process.stdout.write('Structured PHPUnit evidence: artifact://files/test-results.json\n');
  process.stdout.write('Full PHPUnit output: artifact://files/phpunit-output.log\n');
  if (managedRuntimeServices.length > 0) {
    process.stdout.write('Managed runtime service evidence: artifact://files/managed-runtime-services.json\n');
  }
  return results.status;
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
function extractPhpunitOutput(stdout, stderr) {
  const payload = json(stdout.trim(), null);
  if (payload && Array.isArray(payload.executions)) {
    const chunks = [];
    for (const execution of payload.executions) {
      if (typeof execution?.stdout === 'string') { chunks.push(execution.stdout); }
      if (typeof execution?.stderr === 'string') { chunks.push(execution.stderr); }
    }
    if (chunks.length > 0) {
      return chunks.join('');
    }
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
  await Promise.all([
    copyFile(generatedRecipePath, path.join(artifactDirectory, 'wp-codebox-phpunit-recipe.json')),
    writeFile(path.join(artifactDirectory, 'wp-codebox-phpunit-recipe-options.json'), `${JSON.stringify(recipeOptions, null, 2)}\n`),
    writeFile(path.join(artifactDirectory, 'wp-codebox-phpunit-profile.json'), `${JSON.stringify({ wordpress: { topology }, phpunit: { config: profile.config, cwd: profile.cwd, test_root: profile.testRoot, environment: profile.environment, bootstrap_mode: recipeOptions.bootstrapMode, project_bootstrap: recipeOptions.projectBootstrap || null, passthrough_args: recipeOptions.phpunitArgs, extra_mounts: recipeOptions.mounts } }, null, 2)}\n`),
    writeFile(path.join(artifactDirectory, 'wp-codebox-phpunit-provenance.json'), `${JSON.stringify({ source_refs: sourceRefs, wp_codebox: { cli_bin: process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox', resolved_cli_path: process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox', command: [process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox'] } }, null, 2)}\n`),
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
