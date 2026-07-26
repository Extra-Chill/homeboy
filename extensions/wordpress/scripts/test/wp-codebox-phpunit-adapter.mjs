#!/usr/bin/env node
/**
 * External dependencies
 */
import { createWriteStream } from 'node:fs';
import { access, copyFile, mkdir, mkdtemp, readdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';

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
  bootstrapMode: settings.wp_codebox_phpunit_bootstrap_mode,
  projectBootstrap: settings.wp_codebox_phpunit_project_bootstrap,
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
  await handoffArtifacts(runArtifacts, execution);
  if (execution.status !== 0) {
    process.exitCode = execution.status;
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
    await preservePhpunitOutput(artifactRoot, execution, []);
    if (process.env.HOMEBOY_TEST_RESULTS_FILE) {
      runScript('parse-test-results.sh', [artifactRoot]);
    }
    if (process.env.HOMEBOY_TEST_FAILURES_FILE) {
      runScript('parse-test-failures.sh', [artifactRoot, componentPath]);
    }
    return;
  }
  const runtime = pointer?.paths?.runtimeDirectory;
  if (typeof runtime !== 'string' || !/^runtime-[A-Za-z0-9][A-Za-z0-9-]*$/.test(runtime)) {
    return;
  }
  const artifactDirectory = path.join(artifactRoot, runtime);
  const managedRuntimeServices = Array.isArray(pointer.managedRuntimeServices) ? pointer.managedRuntimeServices : [];
  await preservePhpunitOutput(artifactDirectory, execution, managedRuntimeServices);
  if (process.env.HOMEBOY_TEST_RESULTS_FILE) {
    runScript('parse-test-results.sh', [artifactDirectory]);
  }
  if (process.env.HOMEBOY_TEST_FAILURES_FILE) {
    runScript('parse-test-failures.sh', [artifactDirectory, componentPath]);
  }
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
  if (!results && aggregate) {
    results = { schema: 'wp-codebox/test-results/v1', status: aggregate.failed > 0 ? 'failed' : 'passed', summary: aggregate };
  }
  if (results && typeof results === 'object') {
    const summary = isObject(results.summary) ? results.summary : {};
    if (aggregate) {
      results.summary = { ...summary, ...aggregate };
    }
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
  if (hostConfig) {
    try {
      const xml = await readFile(hostConfig, 'utf8');
      const bootstrap = xml.match(/\bbootstrap\s*=\s*["']([^"']+)["']/i)?.[1];
      if (bootstrap) {
        const bootstrapPath = path.resolve(path.dirname(hostConfig), bootstrap);
        const source = await readFile(bootstrapPath, 'utf8').catch(() => '');
        environment = /WP_UnitTestCase|wp-load\.php|WP_TESTS_DIR|wp-tests-config/i.test(source) ? 'wordpress-integration' : 'standalone-php';
      }
    } catch {}
  }
  return { config, testRoot, cwd, environment, hostConfig };
}
async function persistRecipeEvidence(artifactDirectory, recipeOptions, generatedRecipePath, profile, resolvedDependencies) {
  const sourceRefs = [{ slug, source: root, source_subpath: subpath || null }, ...resolvedDependencies.map((dependency) => ({ slug: dependency.slug, source: dependency.source }))];
  await Promise.all([
    copyFile(generatedRecipePath, path.join(artifactDirectory, 'wp-codebox-phpunit-recipe.json')),
    writeFile(path.join(artifactDirectory, 'wp-codebox-phpunit-recipe-options.json'), `${JSON.stringify(recipeOptions, null, 2)}\n`),
    writeFile(path.join(artifactDirectory, 'wp-codebox-phpunit-profile.json'), `${JSON.stringify({ wordpress: { topology }, phpunit: { config: profile.config, cwd: profile.cwd, test_root: profile.testRoot, environment: profile.environment, bootstrap_mode: recipeOptions.bootstrapMode, passthrough_args: recipeOptions.phpunitArgs, extra_mounts: recipeOptions.mounts } }, null, 2)}\n`),
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
