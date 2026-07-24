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

const settings = json(process.env.HOMEBOY_SETTINGS_JSON, {});
const componentPath = required(process.env.HOMEBOY_COMPONENT_PATH, 'HOMEBOY_COMPONENT_PATH');
const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const extensionRoot = path.resolve(scriptDirectory, '../..');
const harnessSource = path.join(extensionRoot, 'vendor');
const slug = process.env.COMPONENT_ID || path.basename(componentPath);
const root = settings.wp_codebox_source_root || componentPath;
const subpath = settings.wp_codebox_source_subpath || undefined;
const pluginSourceDirectory = subpath ? path.join(root, subpath) : root;
const multisite = await resolveMultisite(settings, pluginSourceDirectory);
const directory = await mkdtemp(path.join(tmpdir(), 'homeboy-wp-codebox-phpunit-'));
const optionsPath = path.join(directory, 'options.json');
const recipePath = path.join(directory, 'recipe.json');
const artifacts = process.env.HOMEBOY_WP_CODEBOX_ARTIFACTS_DIR || path.join(directory, 'artifacts');
const runArtifacts = path.join(artifacts, `wp-codebox-phpunit.${process.pid}`);
const dependencies = dependencyPaths(settings).map((source) => {
  const dependencySlug = path.basename(source).replace(/@[^/]+$/, '');
  return { source, slug: dependencySlug, sandboxDirectory: sandboxPluginDirectory(dependencySlug) };
});
await requireHarness(harnessSource);
const options = clean({
  wordpressVersion: settings.wordpress_runtime_version,
  phpVersion: settings.wordpress_runtime_php_version,
  databaseType: settings.database_type,
  pluginSlug: slug,
  extra_plugins: [
    { source: root, sourceSubpath: subpath, slug, activate: false },
    ...dependencies.map(({ source, slug: dependencySlug }) => ({ source, slug: dependencySlug, activate: false })),
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
  await mkdir(runArtifacts, { recursive: true });
  const execution = await runCaptured(['recipe-run', '--recipe', recipePath, '--artifacts', runArtifacts, '--json']);
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
  return Object.entries({ ...process.env, ...configured })
    .filter(([name, value]) => /(?:auth|cookie|credential|key|nonce|passw|secret|session|token)/i.test(name) && typeof value === 'string' && value.length >= 8)
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
  const result = spawnSync(process.env.HOMEBOY_WP_CODEBOX_BIN || process.env.WP_CODEBOX_BIN || 'wp-codebox', args, { cwd: componentPath, encoding: 'utf8', stdio: 'inherit' });
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
function json(value, fallback) { try { return value ? JSON.parse(value) : fallback; } catch { return fallback; } }
function clean(value) { return Object.fromEntries(Object.entries(value).filter(([, entry]) => entry !== undefined && entry !== '' && !(Array.isArray(entry) && entry.length === 0))); }
function dependencyPaths(configuration) {
  const configured = Array.isArray(configuration.validation_dependencies) ? configuration.validation_dependencies : [];
  const canonical = (process.env.HOMEBOY_WORDPRESS_DEPENDENCY_PATHS || '').split('\n');
  return [...new Set([...canonical, ...configured].filter((value) => typeof value === 'string' && path.isAbsolute(value)))];
}
function sandboxPluginDirectory(pluginSlug) {
  return `/wordpress/wp-content/plugins/${pluginSlug}`;
}
function canonicalMounts(value) {
  return Array.isArray(value) ? value : [];
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
  const result = spawnSync('bash', [path.join(scriptDirectory, script), ...args], { cwd: componentPath, encoding: 'utf8', stdio: 'inherit' });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    throw new Error(`${script} failed with exit code ${result.status ?? 1}`);
  }
}
