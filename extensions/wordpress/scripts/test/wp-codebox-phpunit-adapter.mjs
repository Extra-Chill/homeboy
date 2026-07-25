#!/usr/bin/env node
/**
 * External dependencies
 */
import { createWriteStream } from 'node:fs';
import { access, copyFile, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
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
const slug = process.env.COMPONENT_ID || path.basename(componentPath);
const root = settings.wp_codebox_source_root || componentPath;
const subpath = settings.wp_codebox_source_subpath || undefined;
const pluginSourceDirectory = subpath ? path.join(root, subpath) : root;
const multisite = await resolveMultisite(settings, pluginSourceDirectory);
const databaseService = resolveDatabaseService(settings, process.env);
runPrepareSteps(settings.wp_codebox_prepare_steps, pluginSourceDirectory);
const directory = await mkdtemp(path.join(tmpdir(), 'homeboy-wp-codebox-phpunit-'));
const optionsPath = path.join(directory, 'options.json');
const recipePath = path.join(directory, 'recipe.json');
const artifacts = process.env.HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR || path.join(directory, 'artifacts');
const runArtifacts = path.join(artifacts, `wp-codebox-phpunit.${process.pid}`);
const dependencies = await Promise.all(dependencyPaths(settings).map(async (source) => {
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
    ...dependencies.map(({ source, slug: dependencySlug, composer }) => clean({ source, slug: dependencySlug, activate: false, composer })),
  ],
  dependencyMounts: [...new Set([sandboxPluginDirectory(slug), ...dependencies.map(({ sandboxDirectory }) => sandboxDirectory)])],
  testRoot: settings.wp_codebox_phpunit_test_root,
  phpunitXml: settings.wp_codebox_phpunit_config,
  cwd: settings.wp_codebox_phpunit_cwd,
  phpunitArgs: process.argv.slice(2),
  env: settings.bench_env,
  wpConfigDefines: settings.wp_config_defines,
  bootstrapMode: settings.wp_codebox_phpunit_bootstrap_mode,
  projectBootstrap: settings.wp_codebox_phpunit_project_bootstrap,
  multisite,
  preloadFiles: settings.wp_codebox_phpunit_preload_files,
  mounts: [...canonicalMounts(settings.wp_codebox_phpunit_mounts), { source: harnessSource, target: '/wp-codebox-vendor', mode: 'readonly' }],
});

try {
  await writeFile(optionsPath, `${JSON.stringify(options)}\n`);
  run(['recipe', 'build', 'phpunit', '--options', optionsPath, '--output', recipePath]);
  await applyDatabaseServiceAuthorization(recipePath);
  await mkdir(runArtifacts, { recursive: true });
  const executionArgs = ['recipe-run', '--recipe', recipePath, '--artifacts', runArtifacts, '--json'];
  if (databaseService) {
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
      process.stdout.write(chunk);
    });
    child.stderr.on('data', (chunk) => {
      chunk = stderrRedactor.write(chunk);
      stderrFile.write(chunk);
      process.stderr.write(chunk);
    });
    child.on('error', (error) => { childError = error; });
    child.on('close', (status) => {
      const stdoutTail = stdoutRedactor.end();
      const stderrTail = stderrRedactor.end();
      stdoutFile.write(stdoutTail);
      stderrFile.write(stderrTail);
      process.stdout.write(stdoutTail);
      process.stderr.write(stderrTail);
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
      if (boundary === 0) { return ''; }
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
async function resolveMultisite(configuration, pluginDirectory) {
  const fromEnv = process.env.HOMEBOY_WORDPRESS_MULTISITE;
  if (fromEnv !== undefined && fromEnv !== '') {
    return truthy(fromEnv);
  }
  if (configuration.wp_codebox_multisite !== undefined) {
    return truthy(configuration.wp_codebox_multisite);
  }
  return pluginRequiresNetwork(pluginDirectory);
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
    return /^[\s*#\/]*Network\s*:\s*(true|1|yes|on)\b/im.test(block);
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
function dependencyPaths(configuration) {
  const configured = Array.isArray(configuration.validation_dependencies) ? configuration.validation_dependencies : [];
  const canonical = (process.env.HOMEBOY_WORDPRESS_DEPENDENCY_PATHS || '').split('\n');
  return [...new Set([...canonical, ...configured].filter((value) => typeof value === 'string' && path.isAbsolute(value)))];
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
    throw new Error('wp_codebox_database_service must be an object shaped as {provider,secret_env}');
  }
  const unknownKeys = Object.keys(value).filter((key) => !['provider', 'engine', 'allowed_hosts', 'secret_env'].includes(key));
  if (unknownKeys.length > 0) {
    throw new Error(`wp_codebox_database_service contains unsupported fields: ${unknownKeys.join(', ')}`);
  }
  if (configuration.database_type !== 'mysql') {
    throw new Error('wp_codebox_database_service requires database_type=mysql');
  }
  if (value.provider !== 'external') {
    throw new Error('wp_codebox_database_service.provider must be external');
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
  if (!databaseService) {
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
  try { pointer = await readFile(path.join(artifactRoot, 'latest-runtime.json'), 'utf8'); } catch { return; }
  const runtime = json(pointer, {}).paths?.runtimeDirectory;
  if (typeof runtime !== 'string' || !/^runtime-[A-Za-z0-9][A-Za-z0-9-]*$/.test(runtime)) {
    return;
  }
  const artifactDirectory = path.join(artifactRoot, runtime);
  await preservePhpunitOutput(artifactDirectory, execution);
  if (process.env.HOMEBOY_TEST_RESULTS_FILE) {
    runScript('parse-test-results.sh', [artifactDirectory]);
  }
  if (process.env.HOMEBOY_TEST_FAILURES_FILE) {
    runScript('parse-test-failures.sh', [artifactDirectory, componentPath]);
  }
}
async function preservePhpunitOutput(artifactDirectory, execution) {
  const filesDirectory = path.join(artifactDirectory, 'files');
  const logsDirectory = path.join(artifactDirectory, 'logs');
  const testResultsPath = path.join(filesDirectory, 'test-results.json');
  const phpunitOutputPath = path.join(filesDirectory, 'phpunit-output.log');
  await mkdir(filesDirectory, { recursive: true });
  await mkdir(logsDirectory, { recursive: true });
  await copyFile(execution.stdoutPath, path.join(logsDirectory, 'recipe-run.stdout.log'));
  await copyFile(execution.stderrPath, path.join(logsDirectory, 'recipe-run.stderr.log'));

  const stdout = await readFile(execution.stdoutPath, 'utf8');
  const stderr = await readFile(execution.stderrPath, 'utf8');
  const output = extractPhpunitOutput(stdout, stderr);
  await writeFile(phpunitOutputPath, output);

  let results;
  try { results = json(await readFile(testResultsPath, 'utf8'), null); } catch { results = null; }
  if (results && typeof results === 'object') {
    const references = Array.isArray(results.rawLogReferences) ? results.rawLogReferences : [];
    results.rawLogReferences = [
      ...references.filter((reference) => reference?.path !== 'files/phpunit-output.log'),
      { path: 'files/phpunit-output.log', kind: 'phpunit-output' },
      { path: 'logs/recipe-run.stdout.log', kind: 'recipe-run-stdout' },
      { path: 'logs/recipe-run.stderr.log', kind: 'recipe-run-stderr' },
    ];
    results.evidenceReferences = [
      { kind: 'structured-test-results', uri: 'artifact://files/test-results.json' },
      { kind: 'raw-phpunit-output', uri: 'artifact://files/phpunit-output.log' },
    ];
    await writeFile(testResultsPath, `${JSON.stringify(results, null, 2)}\n`);
  }

  process.stdout.write('Structured PHPUnit evidence: artifact://files/test-results.json\n');
  process.stdout.write('Full PHPUnit output: artifact://files/phpunit-output.log\n');
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
function runScript(script, args) {
  const result = spawnSync('bash', [path.join(scriptDirectory, script), ...args], { cwd: componentPath, env: environmentWithoutDatabaseAdministration(), encoding: 'utf8', stdio: 'inherit' });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`${script} failed with exit code ${result.status ?? 1}`);
  }
}
