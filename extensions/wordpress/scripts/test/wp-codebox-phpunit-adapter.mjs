#!/usr/bin/env node
/**
 * External dependencies
 */
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const settings = json(process.env.HOMEBOY_SETTINGS_JSON, {});
const componentPath = required(process.env.HOMEBOY_COMPONENT_PATH, 'HOMEBOY_COMPONENT_PATH');
const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const extensionRoot = path.resolve(scriptDirectory, '../..');
const harnessSource = path.join(extensionRoot, 'vendor');
const slug = process.env.COMPONENT_ID || path.basename(componentPath);
const root = settings.wp_codebox_source_root || componentPath;
const subpath = settings.wp_codebox_source_subpath || undefined;
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
  preloadFiles: settings.wp_codebox_phpunit_preload_files,
  mounts: [...canonicalMounts(settings.wp_codebox_phpunit_mounts), { source: harnessSource, target: '/wp-codebox-vendor', mode: 'readonly' }],
});

try {
  await writeFile(optionsPath, `${JSON.stringify(options)}\n`);
  run(['recipe', 'build', 'phpunit', '--options', optionsPath, '--output', recipePath]);
  await mkdir(runArtifacts, { recursive: true });
  run(['recipe-run', '--recipe', recipePath, '--artifacts', runArtifacts, '--json']);
  await handoffArtifacts(runArtifacts);
  process.stdout.write('WP Codebox test run complete.\n');
} finally {
  await rm(directory, { recursive: true, force: true });
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
async function handoffArtifacts(artifactRoot) {
  let pointer;
  try { pointer = await readFile(path.join(artifactRoot, 'latest-runtime.json'), 'utf8'); } catch { return; }
  const runtime = json(pointer, {}).paths?.runtimeDirectory;
  if (typeof runtime !== 'string' || !/^runtime-[A-Za-z0-9][A-Za-z0-9-]*$/.test(runtime)) {
    return;
  }
  const artifactDirectory = path.join(artifactRoot, runtime);
  if (process.env.HOMEBOY_TEST_RESULTS_FILE) {
    runScript('parse-test-results.sh', [artifactDirectory]);
  }
  if (process.env.HOMEBOY_TEST_FAILURES_FILE) {
    runScript('parse-test-failures.sh', [artifactDirectory, componentPath]);
  }
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
